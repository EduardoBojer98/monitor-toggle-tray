use crate::display;
use crate::tray::MonitorTray;
use fs2::FileExt;
use ksni::TrayService;
use notify_rust::Notification;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::Thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const APP_ID: &str = "monitor-toggle-tray";
pub const APP_NAME: &str = "Monitor Toggle";
pub const INPUT_VCP_CODE: &str = "60";
pub const HDMI1: &str = "0x11";
pub const HDMI2: &str = "0x12";
const INPUT_CACHE_TTL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
pub enum SwitchTarget {
    Hdmi1,
    Hdmi2,
}

impl SwitchTarget {
    pub fn value(self) -> &'static str {
        match self {
            Self::Hdmi1 => HDMI1,
            Self::Hdmi2 => HDMI2,
        }
    }
}

#[derive(Clone, Default)]
pub struct InputCache {
    state: Arc<Mutex<CachedInput>>,
}

#[derive(Default)]
struct CachedInput {
    value: Option<String>,
    last_refresh: Option<Instant>,
}

impl InputCache {
    pub fn current_value(&self) -> Option<String> {
        self.get().ok().flatten().or_else(|| self.peek())
    }

    pub fn get(&self) -> Result<Option<String>, String> {
        if self.needs_refresh() {
            self.refresh()
        } else {
            Ok(self.peek())
        }
    }

    pub fn store(&self, value: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.value = value;
        state.last_refresh = Some(Instant::now());
    }

    pub fn refresh(&self) -> Result<Option<String>, String> {
        match query_current_input() {
            Ok(value) => {
                self.store(Some(value.clone()));
                Ok(Some(value))
            }
            Err(err) => {
                // Keep the last known value, but throttle retries so the tray
                // does not shell out on every repaint when detection is failing.
                let mut state = self.state.lock().unwrap();
                state.last_refresh = Some(Instant::now());
                Err(err)
            }
        }
    }

    fn peek(&self) -> Option<String> {
        self.state.lock().unwrap().value.clone()
    }

    fn needs_refresh(&self) -> bool {
        let state = self.state.lock().unwrap();

        match state.last_refresh {
            Some(last_refresh) => last_refresh.elapsed() >= INPUT_CACHE_TTL,
            None => true,
        }
    }
}

#[derive(Clone)]
pub struct QuitSignal {
    requested: Arc<AtomicBool>,
    main_thread: Thread,
}

impl QuitSignal {
    pub fn new() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
            main_thread: std::thread::current(),
        }
    }

    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.main_thread.unpark();
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

pub fn input_label(value: &str) -> &'static str {
    match value {
        HDMI1 => "HDMI 1",
        HDMI2 => "HDMI 2",
        _ => "Unknown input",
    }
}

pub fn target_label(target: SwitchTarget) -> &'static str {
    input_label(target.value())
}

pub fn notify(summary: &str, body: &str) {
    Notification::new().summary(summary).body(body).show().ok();
}

pub fn tray_icon_theme_path() -> String {
    tray_icon_search_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn tray_icon_name_for_input(input: Option<&str>) -> &'static str {
    match input {
        Some(HDMI1) => "monitor-toggle-tray-hdmi1",
        Some(HDMI2) => "monitor-toggle-tray-hdmi2",
        _ => APP_ID,
    }
}

pub fn tray_icon_uses_theme_assets(input: Option<&str>) -> bool {
    tray_icon_search_dir()
        .map(|dir| dir.join(format!("{}.svg", tray_icon_name_for_input(input))).exists())
        .unwrap_or(false)
}

pub fn autostart_enabled() -> bool {
    std::path::Path::new(&autostart_file_path()).exists()
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let path = autostart_file_path();

    if enabled {
        fs::create_dir_all(autostart_dir())
            .map_err(|err| format!("Could not create autostart directory: {err}"))?;
        fs::write(&path, autostart_desktop_entry()?)
            .map_err(|err| format!("Could not write autostart file {path}: {err}"))?;
    } else if std::path::Path::new(&path).exists() {
        fs::remove_file(&path)
            .map_err(|err| format!("Could not remove autostart file {path}: {err}"))?;
    }

    Ok(())
}

pub fn log_event(message: impl AsRef<str>) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let line = format!("[{timestamp}] {}\n", message.as_ref());

    if fs::create_dir_all(app_state_dir()).is_err() {
        return;
    }

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path())
    {
        let _ = file.write_all(line.as_bytes());
    }
}

pub fn run_command(program: &str, args: &[String]) -> Result<String, String> {
    if should_log_command_start_and_success(program, args) {
        log_event(format!("run_command start: {program} {}", args.join(" ")));
    }
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("{program}: {err}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if should_log_command_start_and_success(program, args) {
            log_event(format!(
                "run_command ok: {program} status={} stdout={} bytes stderr={} bytes",
                output.status,
                stdout.len(),
                output.stderr.len()
            ));
        }
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        log_event(format!(
            "run_command err: {program} status={} detail={detail}",
            output.status
        ));
        Err(if detail.is_empty() {
            format!("{program} exited with status {}", output.status)
        } else {
            format!("{program}: {detail}")
        })
    }
}

pub fn strip_ansi_escape_sequences(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                let _ = chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }

        cleaned.push(ch);
    }

    cleaned
}

pub fn switch_to(target: SwitchTarget, input_cache: &InputCache) -> Result<(), String> {
    log_event(format!("switch_to start: target={}", target_label(target)));
    set_input(target.value())?;
    input_cache.store(Some(target.value().into()));
    std::thread::sleep(Duration::from_millis(700));
    let result = display::prepare_display_layout(target);
    log_event(format!(
        "switch_to done: target={} result={result:?}",
        target_label(target)
    ));
    result
}

pub fn select_target(target: SwitchTarget, input_cache: &InputCache) {
    match switch_to(target, input_cache) {
        Ok(()) => notify(
            "Monitor Input",
            &format!("Switched external display to {}", target_label(target)),
        ),
        Err(err) => notify(
            "Monitor Input",
            &format!("Failed to switch to {}: {}", target_label(target), err),
        ),
    }
}

pub fn run() {
    log_event(format!(
        "main: starting app, log_file={}",
        log_file_path().display()
    ));
    let _instance_lock = match acquire_single_instance_lock() {
        Ok(lock) => lock,
        Err(err) => {
            notify(APP_NAME, &err);
            eprintln!("{err}");
            return;
        }
    };

    let input_cache = InputCache::default();
    input_cache.refresh().ok();

    let quit_signal = QuitSignal::new();
    let (refresh_tx, refresh_rx) = std::sync::mpsc::channel();
    let service = TrayService::new(MonitorTray {
        quit_signal: quit_signal.clone(),
        input_cache: input_cache.clone(),
        refresh_tx: refresh_tx.clone(),
    });
    let handle = service.handle();
    service.spawn();
    drop(refresh_tx);

    std::thread::spawn(move || {
        while refresh_rx.recv().is_ok() {
            handle.update(|_| ());
        }
    });

    while !quit_signal.is_requested() {
        std::thread::park();
    }
}

fn query_current_input() -> Result<String, String> {
    let args = vec!["getvcp".into(), INPUT_VCP_CODE.into()];
    let text = run_command("ddcutil", &args)?;
    parse_current_input(&text).ok_or_else(|| "Unable to parse current monitor input.".into())
}

fn set_input(value: &str) -> Result<(), String> {
    let args = vec!["setvcp".into(), INPUT_VCP_CODE.into(), value.into()];
    run_command("ddcutil", &args).map(|_| ())
}

fn parse_current_input(output: &str) -> Option<String> {
    let markers = ["current value = ", "sl="];

    for marker in markers {
        if let Some(start) = output.find(marker) {
            let value = output[start + marker.len()..]
                .chars()
                .skip_while(|ch| ch.is_whitespace())
                .take_while(|ch| ch.is_ascii_hexdigit() || *ch == 'x' || *ch == 'X')
                .collect::<String>();

            if !value.is_empty() {
                let normalized = value.to_ascii_lowercase();
                return if normalized.starts_with("0x") {
                    Some(normalized)
                } else {
                    Some(format!("0x{normalized}"))
                };
            }
        }
    }

    None
}

fn app_state_dir() -> PathBuf {
    let state_home = env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.local/state")
    });

    PathBuf::from(state_home).join(APP_ID)
}

fn log_file_path() -> PathBuf {
    app_state_dir().join("debug.log")
}

fn should_log_command_start_and_success(program: &str, args: &[String]) -> bool {
    match program {
        "ddcutil" => args.first().map(|arg| arg.as_str()) == Some("setvcp"),
        "xrandr" => !args.iter().any(|arg| arg == "--query"),
        "kscreen-doctor" => !args.iter().any(|arg| arg == "-o"),
        _ => true,
    }
}

fn app_icon_install_dir() -> PathBuf {
    let data_home = env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.local/share")
    });

    PathBuf::from(data_home).join("icons/hicolor/scalable/apps")
}

fn app_icon_install_path() -> PathBuf {
    app_icon_install_dir().join(format!("{APP_ID}.svg"))
}

fn bundled_icon_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

fn tray_icon_search_dir() -> Option<PathBuf> {
    let installed_dir = app_icon_install_dir();
    if tray_icon_set_exists(&installed_dir) {
        return Some(installed_dir);
    }

    let bundled_dir = bundled_icon_dir();
    if tray_icon_set_exists(&bundled_dir) {
        return Some(bundled_dir);
    }

    None
}

fn tray_icon_set_exists(dir: &std::path::Path) -> bool {
    [
        format!("{APP_ID}.svg"),
        format!("{APP_ID}-hdmi1.svg"),
        format!("{APP_ID}-hdmi2.svg"),
    ]
    .into_iter()
    .all(|name| dir.join(name).exists())
}

fn desktop_icon_value() -> String {
    let installed_icon = app_icon_install_path();

    if installed_icon.exists() {
        installed_icon.to_string_lossy().into_owned()
    } else {
        "video-display".into()
    }
}

fn lock_file_path() -> String {
    let base = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    format!("{base}/{APP_ID}.lock")
}

fn autostart_dir() -> String {
    let base = env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.config")
    });

    format!("{base}/autostart")
}

fn autostart_file_path() -> String {
    format!("{}/{}.desktop", autostart_dir(), APP_ID)
}

fn autostart_desktop_entry() -> Result<String, String> {
    let exe =
        env::current_exe().map_err(|err| format!("Could not determine the app path: {err}"))?;
    let exec = exe
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let icon = desktop_icon_value()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    Ok(format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={APP_NAME}\nComment=Tray app for switching monitor input\nExec=\"{exec}\"\nIcon={icon}\nTerminal=false\nCategories=Utility;\nX-GNOME-Autostart-enabled=true\n"
    ))
}

fn acquire_single_instance_lock() -> Result<File, String> {
    let path = lock_file_path();
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|err| format!("Could not open lock file {path}: {err}"))?;

    file.try_lock_exclusive()
        .map_err(|_| "Another instance of Monitor Toggle is already running.".to_string())?;

    file.set_len(0)
        .map_err(|err| format!("Could not initialize lock file {path}: {err}"))?;
    write!(file, "{}", std::process::id())
        .map_err(|err| format!("Could not write lock file {path}: {err}"))?;

    Ok(file)
}
