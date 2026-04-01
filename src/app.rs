use crate::config::ConfigStore;
use crate::display;
use crate::monitor::{self, MonitorCache, MonitorInfo, MonitorSnapshot};
use crate::tray::MonitorTray;
use fs2::FileExt;
use ksni::TrayService;
use notify_rust::Notification;
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::Thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use serde::Deserialize;

pub const APP_ID: &str = "monitor-toggle-tray";
pub const APP_NAME: &str = "Monitor Toggle";

#[derive(Clone)]
pub struct SharedState {
    pub config_store: ConfigStore,
    pub monitor_cache: MonitorCache,
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

pub fn notify(summary: &str, body: &str) {
    Notification::new().summary(summary).body(body).show().ok();
}

pub fn tray_icon_theme_path() -> String {
    tray_icon_search_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn tray_icon_name() -> &'static str {
    APP_ID
}

pub fn tray_icon_available() -> bool {
    tray_icon_search_dir()
        .map(|dir| dir.join(format!("{APP_ID}.svg")).exists())
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

pub fn refresh_snapshot(shared: &SharedState) -> Result<MonitorSnapshot, String> {
    let snapshot = shared.monitor_cache.refresh()?;
    sync_config_with_snapshot(shared, &snapshot).ok();
    Ok(snapshot)
}

pub fn current_snapshot(shared: &SharedState) -> Result<MonitorSnapshot, String> {
    match shared.monitor_cache.get() {
        Ok(snapshot) => Ok(snapshot),
        Err(_) => refresh_snapshot(shared),
    }
}

pub fn resolve_primary(snapshot: &MonitorSnapshot, shared: &SharedState) -> Option<MonitorInfo> {
    if let Some(internal_monitor) = snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.connected && monitor.internal)
    {
        return Some(internal_monitor.clone());
    }

    let config = shared.config_store.current();

    if let Some(primary_id) = config.primary_monitor_id.as_deref() {
        if let Some(primary) = snapshot
            .monitors
            .iter()
            .find(|monitor| monitor.id == primary_id && monitor.connected)
        {
            return Some(primary.clone());
        }
    }

    snapshot.monitors.iter().find(|monitor| monitor.connected).cloned()
}

pub fn controlled_monitors(snapshot: &MonitorSnapshot, shared: &SharedState) -> Vec<MonitorInfo> {
    let config = shared.config_store.current();

    snapshot
        .monitors
        .iter()
        .filter(|monitor| !monitor.internal)
        .filter(|monitor| {
            config
                .settings(&monitor.id)
                .is_some_and(|settings| settings.include_in_quick_switch)
        })
        .cloned()
        .collect()
}

pub fn set_primary_monitor(shared: &SharedState, monitor_id: &str) -> Result<(), String> {
    let snapshot = refresh_snapshot(shared)?;
    let monitor = snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.id == monitor_id)
        .ok_or_else(|| "Selected monitor is no longer available.".to_string())?;

    shared.config_store.update(|config| {
        config.primary_monitor_id = Some(monitor.id.clone());
        config
            .settings_mut_or_insert(&monitor.id, &monitor.display_name)
            .display_name = monitor.display_name.clone();
    })?;

    Ok(())
}

pub fn toggle_controlled_monitor(shared: &SharedState, monitor_id: &str) -> Result<bool, String> {
    let snapshot = refresh_snapshot(shared)?;
    let monitor = snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.id == monitor_id)
        .ok_or_else(|| "Selected monitor is no longer available.".to_string())?;

    if monitor.internal {
        return Err("The primary built-in display cannot be toggled.".into());
    }

    shared.config_store.update(|config| {
        let settings = config.settings_mut_or_insert(&monitor.id, &monitor.display_name);
        settings.include_in_quick_switch = !settings.include_in_quick_switch;
        settings.include_in_quick_switch
    })
}

pub fn set_laptop_input(
    shared: &SharedState,
    monitor_id: &str,
    input: Option<&str>,
) -> Result<(), String> {
    let snapshot = refresh_snapshot(shared)?;
    let monitor = snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.id == monitor_id)
        .ok_or_else(|| "Selected monitor is no longer available.".to_string())?;

    shared.config_store.update(|config| {
        let settings = config.settings_mut_or_insert(&monitor.id, &monitor.display_name);
        settings.laptop_input = input.map(|value| value.to_string());
    })?;

    Ok(())
}

pub fn set_toggle_input(
    shared: &SharedState,
    monitor_id: &str,
    input: Option<&str>,
) -> Result<(), String> {
    let snapshot = refresh_snapshot(shared)?;
    let monitor = snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.id == monitor_id)
        .ok_or_else(|| "Selected monitor is no longer available.".to_string())?;

    shared.config_store.update(|config| {
        let settings = config.settings_mut_or_insert(&monitor.id, &monitor.display_name);
        settings.toggle_input = input.map(|value| value.to_string());
    })?;

    Ok(())
}

pub fn capture_current_input_as_laptop(shared: &SharedState, monitor_id: &str) -> Result<(), String> {
    let snapshot = refresh_snapshot(shared)?;
    let monitor = snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.id == monitor_id)
        .ok_or_else(|| "Selected monitor is no longer available.".to_string())?;
    let current_input = monitor
        .ddc
        .as_ref()
        .and_then(|ddc| ddc.current_input.clone())
        .ok_or_else(|| "Current monitor input is not available for this display.".to_string())?;

    set_laptop_input(shared, monitor_id, Some(&current_input))
}

pub fn quick_switch(shared: &SharedState) -> Result<String, String> {
    let snapshot = refresh_snapshot(shared)?;
    let primary = resolve_primary(&snapshot, shared)
        .ok_or_else(|| "No primary display is currently available.".to_string())?;
    let controlled = controlled_monitors(&snapshot, shared);

    if controlled.is_empty() {
        return Err("No external monitors are selected for quick switch.".into());
    }

    let turn_off = controlled.iter().any(|monitor| monitor.active);
    let config = shared.config_store.current();
    let mut notes = Vec::new();

    let outputs = controlled
        .iter()
        .map(|monitor| display::OutputLayout {
            name: monitor.output_name.clone(),
            position: config
                .settings(&monitor.id)
                .and_then(|settings| match (settings.saved_position_x, settings.saved_position_y) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                }),
            size: config.settings(&monitor.id).and_then(|settings| {
                match (settings.saved_width, settings.saved_height) {
                    (Some(width), Some(height)) => Some((width, height)),
                    _ => None,
                }
            }),
        })
        .collect::<Vec<_>>();
    if turn_off {
        let output_names = outputs
            .iter()
            .map(|layout| layout.name.clone())
            .collect::<Vec<_>>();
        log_event(format!(
            "quick_switch: turning controlled monitors off primary={} outputs={}",
            primary.output_name,
            output_names.join(", ")
        ));
        display::disable_outputs(&primary.output_name, &output_names)?;
        std::thread::sleep(Duration::from_millis(250));
    }

    for monitor in &controlled {
        let settings = config.settings(&monitor.id);
        let desired_input = if turn_off {
            settings.and_then(|settings| settings.toggle_input.as_deref())
        } else {
            settings.and_then(|settings| settings.laptop_input.as_deref())
        };

        if let (Some(ddc), Some(input)) = (monitor.ddc.as_ref(), desired_input) {
            if let Err(err) = monitor::set_input_for_monitor(ddc.display_number, input) {
                notes.push(format!("{}: {err}", monitor.display_name));
            }
        } else if monitor.ddc.is_none() {
            notes.push(format!(
                "{}: input switching is not available",
                monitor.display_name
            ));
        } else {
            notes.push(format!(
                "{}: no {} input is configured",
                monitor.display_name,
                if turn_off { "toggle-to" } else { "laptop" }
            ));
        }
    }

    std::thread::sleep(Duration::from_millis(700));

    if !turn_off {
        let primary_position = config
            .settings(&primary.id)
            .and_then(|settings| match (settings.saved_position_x, settings.saved_position_y) {
                (Some(x), Some(y)) => Some((x, y)),
                _ => primary.position,
            });
        log_event(format!(
            "quick_switch: turning controlled monitors on primary={} primary_position={:?} outputs={}",
            primary.output_name,
            primary_position,
            outputs
                .iter()
                .map(|layout| format!("{}@{:?}", layout.name, layout.position))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        display::enable_outputs(&primary.output_name, primary_position, &outputs)?;
    } else {
        log_event("quick_switch: controlled monitors switched away from laptop inputs");
    }

    shared.monitor_cache.invalidate();
    let state_label = if turn_off {
        "controlled monitors off"
    } else {
        "controlled monitors on"
    };

    if notes.is_empty() {
        Ok(format!("Quick switch complete: {state_label}."))
    } else {
        Ok(format!(
            "Quick switch complete: {state_label}. {}",
            notes.join(" | ")
        ))
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

    let shared = SharedState {
        config_store: ConfigStore::load(),
        monitor_cache: MonitorCache::default(),
    };
    refresh_snapshot(&shared).ok();

    let quit_signal = QuitSignal::new();
    let (refresh_tx, refresh_rx) = std::sync::mpsc::channel();
    let service = TrayService::new(MonitorTray {
        quit_signal: quit_signal.clone(),
        shared: shared.clone(),
        refresh_tx: refresh_tx.clone(),
    });
    let handle = service.handle();
    service.spawn();

    let refresh_state = shared.clone();
    let periodic_refresh_tx = refresh_tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        refresh_snapshot(&refresh_state).ok();
        periodic_refresh_tx.send(()).ok();
    });

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

fn sync_config_with_snapshot(shared: &SharedState, snapshot: &MonitorSnapshot) -> Result<(), String> {
    shared.config_store.update(|config| {
        if let Some(internal_monitor) = snapshot
            .monitors
            .iter()
            .find(|monitor| monitor.connected && monitor.internal)
        {
            config.primary_monitor_id = Some(internal_monitor.id.clone());
        } else if config.primary_monitor_id.is_none() {
            config.primary_monitor_id = snapshot
                .monitors
                .iter()                
                .find(|monitor| monitor.connected)
                .map(|monitor| monitor.id.clone());
        }

        for monitor in &snapshot.monitors {
            let settings = config.settings_mut_or_insert(&monitor.id, &monitor.display_name);
            if settings.laptop_input.is_none() && monitor.active {
                if let Some(current_input) = monitor
                    .ddc
                    .as_ref()
                    .and_then(|ddc| ddc.current_input.clone())
                {
                    settings.laptop_input = Some(current_input);
                }
            }
        }
    })?;

    Ok(())
}

pub fn save_current_layout(shared: &SharedState) -> Result<String, String> {
    let snapshot = refresh_snapshot(shared)?;
    save_current_layout_snapshot(shared, &snapshot)
}

fn save_current_layout_snapshot(shared: &SharedState, snapshot: &MonitorSnapshot) -> Result<String, String> {
    if let Some(saved_layout) = load_kwin_output_layout(snapshot)? {
        shared.config_store.update(|config| {
            for entry in &saved_layout {
                let settings = config.settings_mut_or_insert(&entry.monitor_id, &entry.display_name);
                settings.saved_position_x = Some(entry.position.0);
                settings.saved_position_y = Some(entry.position.1);
                settings.saved_width = Some(entry.size.0);
                settings.saved_height = Some(entry.size.1);
            }
        })?;

        let summary = saved_layout
            .iter()
            .map(|entry| {
                format!(
                    "{}@{},{} {}x{}",
                    entry.output_name, entry.position.0, entry.position.1, entry.size.0, entry.size.1
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        log_event(format!("save_current_layout: saved positions from kwinoutputconfig={summary}"));
        return Ok(format!("Saved current layout: {summary}"));
    }

    let active_with_positions = snapshot
        .monitors
        .iter()
        .filter(|monitor| monitor.active)
        .filter_map(|monitor| monitor.position.map(|position| (monitor, position)))
        .collect::<Vec<_>>();
    let unique_positions = active_with_positions
        .iter()
        .map(|(_, position)| *position)
        .collect::<BTreeSet<_>>();

    if active_with_positions.len() < 2 || unique_positions.len() != active_with_positions.len() {
        let message =
            "Could not save layout because active monitor positions are incomplete or overlapping.";
        log_event(format!("save_current_layout: skipped: {message}"));
        return Err(message.into());
    }

    shared.config_store.update(|config| {
        for (monitor, (x, y)) in &active_with_positions {
            let settings = config.settings_mut_or_insert(&monitor.id, &monitor.display_name);
            settings.saved_position_x = Some(*x);
            settings.saved_position_y = Some(*y);
            if let Some((width, height)) = monitor.current_mode {
                settings.saved_width = Some(width);
                settings.saved_height = Some(height);
            }
        }
    })?;

    let summary = active_with_positions
        .iter()
        .map(|(monitor, (x, y))| format!("{}@{},{}", monitor.output_name, x, y))
        .collect::<Vec<_>>()
        .join(", ");
    log_event(format!("save_current_layout: saved positions={summary}"));
    Ok(format!("Saved current layout: {summary}"))
}

#[derive(Deserialize)]
struct KwinConfigSection {
    name: String,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct KwinOutputMeta {
    #[serde(rename = "connectorName")]
    connector_name: String,
    mode: KwinMode,
}

#[derive(Deserialize)]
struct KwinMode {
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct KwinSetup {
    outputs: Vec<KwinSetupOutput>,
}

#[derive(Deserialize)]
struct KwinSetupOutput {
    enabled: bool,
    #[serde(rename = "outputIndex")]
    output_index: usize,
    position: KwinPosition,
}

#[derive(Deserialize)]
struct KwinPosition {
    x: i32,
    y: i32,
}

struct SavedLayoutEntry {
    monitor_id: String,
    display_name: String,
    output_name: String,
    position: (i32, i32),
    size: (u32, u32),
}

fn load_kwin_output_layout(snapshot: &MonitorSnapshot) -> Result<Option<Vec<SavedLayoutEntry>>, String> {
    let path = kwin_output_config_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };

    let sections = serde_json::from_str::<Vec<KwinConfigSection>>(&text)
        .map_err(|err| format!("Could not parse {}: {err}", path.display()))?;
    let outputs = sections
        .iter()
        .find(|section| section.name == "outputs")
        .map(|section| serde_json::from_value::<Vec<KwinOutputMeta>>(section.data.clone()))
        .transpose()
        .map_err(|err| format!("Could not parse outputs from {}: {err}", path.display()))?
        .unwrap_or_default();
    let setups = sections
        .iter()
        .find(|section| section.name == "setups")
        .map(|section| serde_json::from_value::<Vec<KwinSetup>>(section.data.clone()))
        .transpose()
        .map_err(|err| format!("Could not parse setups from {}: {err}", path.display()))?
        .unwrap_or_default();

    if outputs.is_empty() || setups.is_empty() {
        return Ok(None);
    }

    let active_names = snapshot
        .monitors
        .iter()
        .filter(|monitor| monitor.active)
        .map(|monitor| monitor.output_name.clone())
        .collect::<BTreeSet<_>>();
    if active_names.len() < 2 {
        return Ok(None);
    }

    for setup in setups {
        let enabled = setup
            .outputs
            .iter()
            .filter(|output| output.enabled)
            .filter_map(|output| outputs.get(output.output_index).map(|meta| (output, meta)))
            .collect::<Vec<_>>();
        let enabled_names = enabled
            .iter()
            .map(|(_, meta)| meta.connector_name.clone())
            .collect::<BTreeSet<_>>();

        if enabled_names != active_names {
            continue;
        }

        let mut saved = Vec::new();
        for (setup_output, meta) in enabled {
            if let Some(monitor) = snapshot
                .monitors
                .iter()
                .find(|monitor| monitor.output_name == meta.connector_name)
            {
                saved.push(SavedLayoutEntry {
                    monitor_id: monitor.id.clone(),
                    display_name: monitor.display_name.clone(),
                    output_name: monitor.output_name.clone(),
                    position: (setup_output.position.x, setup_output.position.y),
                    size: (meta.mode.width, meta.mode.height),
                });
            }
        }

        if !saved.is_empty() {
            return Ok(Some(saved));
        }
    }

    Ok(None)
}

fn kwin_output_config_path() -> PathBuf {
    config_home().join("kwinoutputconfig.json")
}

fn config_home() -> PathBuf {
    let base = env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.config")
    });

    PathBuf::from(base)
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
        "ddcutil" => args.iter().any(|arg| arg == "setvcp"),
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
    if tray_icon_exists(&installed_dir) {
        return Some(installed_dir);
    }

    let bundled_dir = bundled_icon_dir();
    if tray_icon_exists(&bundled_dir) {
        return Some(bundled_dir);
    }

    None
}

fn tray_icon_exists(dir: &std::path::Path) -> bool {
    dir.join(format!("{APP_ID}.svg")).exists()
}

fn desktop_icon_value() -> String {
    let installed_icon = app_icon_install_path();

    if installed_icon.exists() {
        APP_ID.into()
    } else {
        let bundled_icon = bundled_icon_dir().join(format!("{APP_ID}.svg"));

        if bundled_icon.exists() {
            bundled_icon.to_string_lossy().into_owned()
        } else {
            "video-display".into()
        }
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
