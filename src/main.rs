use fs2::FileExt;
use ksni::{Tray, TrayService, menu};
use notify_rust::Notification;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread::Thread;
use std::time::{Duration, Instant};

const APP_ID: &str = "monitor-toggle-tray";
const INPUT_VCP_CODE: &str = "60";
const HDMI1: &str = "0x11";
const HDMI2: &str = "0x12";
const INPUT_CACHE_TTL: Duration = Duration::from_secs(2);
const HDMI1_LAYOUT_ATTEMPTS: usize = 10;
const HDMI1_LAYOUT_RETRY_DELAY: Duration = Duration::from_millis(700);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayBackend {
    KscreenDoctor,
    Xrandr,
}

#[derive(Clone)]
struct DisplayOutput {
    id: String,
    name: String,
    connected: bool,
    internal: bool,
}

#[derive(Clone)]
struct DisplayPair {
    anchor: DisplayOutput,
    external: DisplayOutput,
}

#[derive(Clone, Copy)]
enum SwitchTarget {
    Hdmi1,
    Hdmi2,
}

impl SwitchTarget {
    fn value(self) -> &'static str {
        match self {
            Self::Hdmi1 => HDMI1,
            Self::Hdmi2 => HDMI2,
        }
    }
}

#[derive(Clone, Default)]
struct InputCache {
    state: Arc<Mutex<CachedInput>>,
}

#[derive(Default)]
struct CachedInput {
    value: Option<String>,
    last_refresh: Option<Instant>,
}

impl InputCache {
    fn current_value(&self) -> Option<String> {
        self.get().ok().flatten().or_else(|| self.peek())
    }

    fn get(&self) -> Result<Option<String>, String> {
        if self.needs_refresh() {
            self.refresh()
        } else {
            Ok(self.peek())
        }
    }

    fn store(&self, value: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.value = value;
        state.last_refresh = Some(Instant::now());
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

    fn refresh(&self) -> Result<Option<String>, String> {
        match query_current_input() {
            Ok(value) => {
                self.store(Some(value.clone()));
                Ok(Some(value))
            }
            Err(err) => {
                let mut state = self.state.lock().unwrap();
                state.last_refresh = Some(Instant::now());
                Err(err)
            }
        }
    }
}

fn input_label(value: &str) -> &'static str {
    match value {
        HDMI1 => "HDMI 1",
        HDMI2 => "HDMI 2",
        _ => "Unknown input",
    }
}

fn target_label(target: SwitchTarget) -> &'static str {
    input_label(target.value())
}

fn is_internal_output(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();

    ["edp", "lvds", "dsi", "lcd", "panel", "internal", "embedded"]
        .iter()
        .any(|marker| normalized.starts_with(marker) || normalized.contains(marker))
}

fn has_internal_marker(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();

    ["panel", "internal", "embedded", "laptop", "built-in"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn is_external_output(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();

    ["hdmi", "displayport", "dp-", "dvi", "vga"]
        .iter()
        .any(|marker| normalized.starts_with(marker) || normalized.contains(marker))
}

fn find_internal_output(outputs: &[DisplayOutput]) -> Option<DisplayOutput> {
    outputs
        .iter()
        .find(|output| output.internal && output.connected)
        .cloned()
        .or_else(|| outputs.iter().find(|output| output.internal).cloned())
        .or_else(|| {
            outputs
                .iter()
                .find(|output| output.connected && !is_external_output(&output.name))
                .cloned()
        })
        .or_else(|| {
            // Some systems expose the laptop panel with an unexpected connector name.
            // If only one connected output does not look like an external connector,
            // treat it as the laptop display.
            let connected_outputs = outputs
                .iter()
                .filter(|output| output.connected)
                .cloned()
                .collect::<Vec<_>>();

            let non_external_outputs = connected_outputs
                .into_iter()
                .filter(|output| !is_external_output(&output.name))
                .collect::<Vec<_>>();

            if non_external_outputs.len() == 1 {
                non_external_outputs.into_iter().next()
            } else {
                None
            }
        })
}

fn find_anchor_output(outputs: &[DisplayOutput]) -> Option<DisplayOutput> {
    find_internal_output(outputs)
        .or_else(|| outputs.iter().find(|output| output.connected).cloned())
}

fn find_external_output(outputs: &[DisplayOutput]) -> Option<DisplayOutput> {
    outputs
        .iter()
        .find(|output| output.connected && !output.internal && is_external_output(&output.name))
        .cloned()
}

fn notify(summary: &str, body: &str) {
    Notification::new().summary(summary).body(body).show().ok();
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

    Ok(format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName=Monitor Toggle\nComment=Tray app for switching monitor input\nExec=\"{exec}\"\nIcon=video-display\nTerminal=false\nCategories=Utility;\nX-GNOME-Autostart-enabled=true\n"
    ))
}

fn autostart_enabled() -> bool {
    std::path::Path::new(&autostart_file_path()).exists()
}

fn set_autostart(enabled: bool) -> Result<(), String> {
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

fn run_command(program: &str, args: &[String]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("{program}: {err}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(if detail.is_empty() {
            format!("{program} exited with status {}", output.status)
        } else {
            format!("{program}: {detail}")
        })
    }
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

fn query_current_input() -> Result<String, String> {
    let args = vec!["getvcp".into(), INPUT_VCP_CODE.into()];
    let text = run_command("ddcutil", &args)?;
    parse_current_input(&text).ok_or_else(|| "Unable to parse current monitor input.".into())
}

fn set_input(value: &str) -> Result<(), String> {
    let args = vec!["setvcp".into(), INPUT_VCP_CODE.into(), value.into()];
    run_command("ddcutil", &args).map(|_| ())
}

fn display_backend_candidates(session: &str) -> [DisplayBackend; 2] {
    match session {
        "wayland" => [DisplayBackend::KscreenDoctor, DisplayBackend::Xrandr],
        "x11" => [DisplayBackend::Xrandr, DisplayBackend::KscreenDoctor],
        _ => [DisplayBackend::KscreenDoctor, DisplayBackend::Xrandr],
    }
}

fn probe_display_backend(backend: DisplayBackend) -> bool {
    let result = match backend {
        DisplayBackend::KscreenDoctor => run_command("kscreen-doctor", &["-o".into()])
            .map(|output| !parse_kscreen_outputs(&output).is_empty()),
        DisplayBackend::Xrandr => run_command("xrandr", &["--query".into()])
            .map(|output| !parse_xrandr_outputs(&output).is_empty()),
    };

    result.unwrap_or(false)
}

fn detect_display_backend() -> Option<DisplayBackend> {
    let session = env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_ascii_lowercase();

    display_backend_candidates(&session)
        .into_iter()
        .find(|backend| probe_display_backend(*backend))
}

fn parse_kscreen_outputs(output: &str) -> Vec<DisplayOutput> {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with("Output: ") {
                return None;
            }

            let mut parts = trimmed.split_whitespace();
            let _ = parts.next()?;
            let id = parts.next()?.to_string();
            let name = parts.next()?.to_string();

            Some(DisplayOutput {
                id,
                internal: is_internal_output(&name) || has_internal_marker(trimmed),
                connected: trimmed.contains(" connected"),
                name,
            })
        })
        .collect()
}

fn parse_xrandr_outputs(output: &str) -> Vec<DisplayOutput> {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !(trimmed.contains(" connected") || trimmed.contains(" disconnected")) {
                return None;
            }

            let mut parts = trimmed.split_whitespace();
            let name = parts.next()?.to_string();
            let state = parts.next()?;
            let connected = state == "connected";

            Some(DisplayOutput {
                id: name.clone(),
                internal: is_internal_output(&name) || has_internal_marker(trimmed),
                connected,
                name,
            })
        })
        .collect()
}

fn list_outputs(backend: DisplayBackend) -> Result<Vec<DisplayOutput>, String> {
    match backend {
        DisplayBackend::KscreenDoctor => {
            let args = vec!["-o".into()];
            Ok(parse_kscreen_outputs(&run_command(
                "kscreen-doctor",
                &args,
            )?))
        }
        DisplayBackend::Xrandr => {
            let args = vec!["--query".into()];
            Ok(parse_xrandr_outputs(&run_command("xrandr", &args)?))
        }
    }
}

fn detect_display_pair(backend: DisplayBackend) -> Result<DisplayPair, String> {
    let outputs = list_outputs(backend)?;

    let anchor = find_anchor_output(&outputs)
        .ok_or_else(|| "Could not find any active display output.".to_string())?;

    let external = find_external_output(&outputs)
        .ok_or_else(|| "Could not find a connected secondary display.".to_string())?;

    Ok(DisplayPair { anchor, external })
}

fn ensure_anchor_only(
    backend: DisplayBackend,
    anchor: &DisplayOutput,
    outputs: &[DisplayOutput],
) -> Result<(), String> {
    match backend {
        DisplayBackend::KscreenDoctor => {
            let mut args = vec![
                format!("output.{}.enable", anchor.id),
                format!("output.{}.primary", anchor.id),
                format!("output.{}.position.0,0", anchor.id),
            ];

            for output in outputs.iter().filter(|output| output.id != anchor.id) {
                args.push(format!("output.{}.disable", output.id));
            }

            run_command("kscreen-doctor", &args).map(|_| ())
        }
        DisplayBackend::Xrandr => {
            let mut args = vec![
                "--output".into(),
                anchor.name.clone(),
                "--auto".into(),
                "--primary".into(),
            ];

            for output in outputs.iter().filter(|output| output.name != anchor.name) {
                args.push("--output".into());
                args.push(output.name.clone());
                args.push("--off".into());
            }

            run_command("xrandr", &args).map(|_| ())
        }
    }
}

fn set_extended(backend: DisplayBackend, pair: &DisplayPair) -> Result<(), String> {
    match backend {
        DisplayBackend::KscreenDoctor => {
            let args = vec![
                format!("output.{}.enable", pair.anchor.id),
                format!("output.{}.primary", pair.anchor.id),
                format!("output.{}.position.0,0", pair.anchor.id),
                format!("output.{}.enable", pair.external.id),
            ];
            run_command("kscreen-doctor", &args).map(|_| ())
        }
        DisplayBackend::Xrandr => {
            let anchor_args = vec![
                "--output".into(),
                pair.anchor.name.clone(),
                "--auto".into(),
                "--primary".into(),
            ];
            run_command("xrandr", &anchor_args)?;

            let external_args = vec![
                "--output".into(),
                pair.external.name.clone(),
                "--auto".into(),
                "--right-of".into(),
                pair.anchor.name.clone(),
            ];
            run_command("xrandr", &external_args).map(|_| ())
        }
    }
}

fn apply_laptop_only_layout(backend: DisplayBackend) -> Result<(), String> {
    let outputs = list_outputs(backend)?;

    if let Some(anchor) = find_anchor_output(&outputs) {
        ensure_anchor_only(backend, &anchor, &outputs)?;
    }

    Ok(())
}

fn prepare_display_layout(target: SwitchTarget) -> Result<(), String> {
    let backend = detect_display_backend()
        .ok_or_else(|| "No supported display management tool was found.".to_string())?;

    match target {
        SwitchTarget::Hdmi1 => prepare_hdmi1_layout(backend),
        SwitchTarget::Hdmi2 => apply_laptop_only_layout(backend),
    }
}

fn is_transient_hdmi1_layout_error(err: &str) -> bool {
    err.contains("Could not find any active display output.")
        || err.contains("Could not find a connected secondary display.")
        || err.contains("BadMatch")
        || err.contains("Configure crtc")
        || err.contains("cannot find mode")
}

fn prepare_hdmi1_layout(backend: DisplayBackend) -> Result<(), String> {
    let mut last_error = "Could not find a connected secondary display.".to_string();

    for attempt in 0..HDMI1_LAYOUT_ATTEMPTS {
        let result = detect_display_pair(backend).and_then(|pair| set_extended(backend, &pair));

        match result {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = err;

                if attempt + 1 < HDMI1_LAYOUT_ATTEMPTS
                    && is_transient_hdmi1_layout_error(&last_error)
                {
                    std::thread::sleep(HDMI1_LAYOUT_RETRY_DELAY);
                    continue;
                }

                return Err(last_error);
            }
        }
    }

    Err(last_error)
}

fn switch_to(target: SwitchTarget, input_cache: &InputCache) -> Result<(), String> {
    match target {
        SwitchTarget::Hdmi1 => {
            set_input(target.value())?;
            input_cache.store(Some(target.value().into()));
            match prepare_display_layout(target) {
                Ok(()) => Ok(()),
                Err(err) if is_transient_hdmi1_layout_error(&err) => Ok(()),
                Err(err) => Err(err),
            }
        }
        SwitchTarget::Hdmi2 => {
            set_input(target.value())?;
            input_cache.store(Some(target.value().into()));
            std::thread::sleep(Duration::from_millis(700));
            prepare_display_layout(target)
        }
    }
}

fn select_target(target: SwitchTarget, input_cache: &InputCache) {
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

#[derive(Clone)]
struct QuitSignal {
    requested: Arc<AtomicBool>,
    main_thread: Thread,
}

impl QuitSignal {
    fn new() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
            main_thread: std::thread::current(),
        }
    }

    fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.main_thread.unpark();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

struct MonitorTray {
    quit_signal: QuitSignal,
    input_cache: InputCache,
    refresh_tx: Sender<()>,
}

impl MonitorTray {
    fn selected_menu_index(current_input: Option<&str>) -> usize {
        match current_input {
            Some(HDMI2) => 1,
            _ => 0,
        }
    }

    fn autostart_enabled(&self) -> bool {
        autostart_enabled()
    }

    fn toggle_autostart(&mut self) {
        let enable = !self.autostart_enabled();

        match set_autostart(enable) {
            Ok(()) => notify(
                "Monitor Toggle",
                if enable {
                    "Autostart enabled."
                } else {
                    "Autostart disabled."
                },
            ),
            Err(err) => notify(
                "Monitor Toggle",
                &format!("Failed to update autostart: {err}"),
            ),
        }

        self.request_refresh();
    }

    fn current_input(&self) -> Option<String> {
        self.input_cache.current_value()
    }

    fn request_refresh(&self) {
        self.refresh_tx.send(()).ok();
    }

    fn trigger_switch(&self, target: SwitchTarget) {
        let input_cache = self.input_cache.clone();
        let refresh_tx = self.refresh_tx.clone();

        std::thread::spawn(move || {
            select_target(target, &input_cache);
            refresh_tx.send(()).ok();
        });
    }
}

impl Tray for MonitorTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn icon_name(&self) -> String {
        match self.current_input().as_deref() {
            Some(HDMI1) => "video-display".into(),
            Some(HDMI2) => "video-television".into(),
            _ => "computer".into(),
        }
    }

    fn title(&self) -> String {
        self.current_input()
            .map(|current| format!("Monitor Toggle ({})", input_label(&current)))
            .unwrap_or_else(|| "Monitor Toggle".into())
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let next_target = match self.current_input().as_deref() {
            Some(HDMI1) => SwitchTarget::Hdmi2,
            Some(HDMI2) => SwitchTarget::Hdmi1,
            _ => SwitchTarget::Hdmi1,
        };

        self.trigger_switch(next_target);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let current_input = self.current_input();
        let current_label = current_input
            .as_deref()
            .map(input_label)
            .unwrap_or("Unavailable");

        vec![
            menu::StandardItem {
                label: format!("Current: {current_label}"),
                enabled: false,
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            menu::RadioGroup {
                selected: Self::selected_menu_index(current_input.as_deref()),
                select: Box::new(|tray: &mut MonitorTray, selected| match selected {
                    0 => tray.trigger_switch(SwitchTarget::Hdmi1),
                    1 => tray.trigger_switch(SwitchTarget::Hdmi2),
                    _ => {}
                }),
                options: vec![
                    menu::RadioItem {
                        label: "HDMI 1 (This laptop / extend)".into(),
                        ..Default::default()
                    },
                    menu::RadioItem {
                        label: "HDMI 2 (Other device / laptop only)".into(),
                        ..Default::default()
                    },
                ],
            }
            .into(),
            ksni::MenuItem::Separator,
            menu::CheckmarkItem {
                label: "Autostart".into(),
                checked: self.autostart_enabled(),
                activate: Box::new(|tray: &mut MonitorTray| tray.toggle_autostart()),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            menu::StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut MonitorTray| tray.quit_signal.request()),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn main() {
    let _instance_lock = match acquire_single_instance_lock() {
        Ok(lock) => lock,
        Err(err) => {
            notify("Monitor Toggle", &err);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, connected: bool, internal: bool) -> DisplayOutput {
        DisplayOutput {
            id: name.into(),
            name: name.into(),
            connected,
            internal,
        }
    }

    #[test]
    fn detects_internal_output_from_standard_connector_names() {
        assert!(is_internal_output("eDP-1"));
        assert!(is_internal_output("LVDS-1"));
        assert!(is_internal_output("DSI-1"));
        assert!(!is_internal_output("HDMI-A-1"));
    }

    #[test]
    fn falls_back_to_non_external_connected_output() {
        let outputs = vec![
            output("Unknown-1", true, false),
            output("HDMI-A-1", true, false),
        ];

        let internal = find_internal_output(&outputs).expect("expected laptop display");

        assert_eq!(internal.name, "Unknown-1");
    }

    #[test]
    fn prefers_explicit_internal_output_when_available() {
        let outputs = vec![output("eDP-1", true, true), output("HDMI-A-1", true, false)];

        let internal = find_internal_output(&outputs).expect("expected laptop display");

        assert_eq!(internal.name, "eDP-1");
    }

    #[test]
    fn falls_back_to_any_connected_output_as_anchor() {
        let outputs = vec![
            output("Unknown-1", true, false),
            output("HDMI-A-1", false, false),
        ];

        let anchor = find_anchor_output(&outputs).expect("expected anchor output");

        assert_eq!(anchor.name, "Unknown-1");
    }

    #[test]
    fn detects_connected_secondary_display() {
        let outputs = vec![output("eDP-1", true, true), output("HDMI-A-1", true, false)];

        let external = find_external_output(&outputs).expect("expected external output");

        assert_eq!(external.name, "HDMI-A-1");
    }

    #[test]
    fn parses_xrandr_output_lines() {
        let outputs = parse_xrandr_outputs(
            "eDP-1 connected primary 1920x1080+0+0\nHDMI-A-1 connected 2560x1440+1920+0\nDP-1 disconnected\n",
        );

        assert_eq!(outputs.len(), 3);
        assert!(outputs[0].internal);
        assert!(outputs[1].connected);
        assert!(!outputs[2].connected);
    }

    #[test]
    fn parses_kscreen_output_lines() {
        let outputs = parse_kscreen_outputs(
            "Output: 1 eDP-1 enabled connected priority 1 Panel\nOutput: 2 HDMI-A-1 enabled connected priority 2\n",
        );

        assert_eq!(outputs.len(), 2);
        assert!(outputs[0].internal);
        assert!(outputs[1].connected);
    }

    #[test]
    fn parses_kscreen_panel_marker_as_internal() {
        let outputs = parse_kscreen_outputs(
            "Output: 1 Unknown-1 enabled connected priority 1 Panel\nOutput: 2 HDMI-A-1 enabled connected priority 2\n",
        );

        assert_eq!(outputs.len(), 2);
        assert!(outputs[0].internal);
        assert!(!outputs[1].internal);
    }

    #[test]
    fn parses_current_input_from_ddcutil_output() {
        assert_eq!(
            parse_current_input(
                "VCP code 0x60 (Input Source): current value = 0x12, max value = 0x12"
            ),
            Some("0x12".into())
        );
        assert_eq!(
            parse_current_input("something sl=11 other"),
            Some("0x11".into())
        );
    }

    #[test]
    fn prefers_wayland_friendly_backend_order() {
        assert_eq!(
            display_backend_candidates("wayland"),
            [DisplayBackend::KscreenDoctor, DisplayBackend::Xrandr]
        );
        assert_eq!(
            display_backend_candidates("x11"),
            [DisplayBackend::Xrandr, DisplayBackend::KscreenDoctor]
        );
    }
}
