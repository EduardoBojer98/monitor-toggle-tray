use crate::config::{ConfigStore, QuickSwitchState};
use crate::display;
use crate::monitor::{self, MonitorCache, MonitorInfo, MonitorSnapshot};
use crate::settings_window;
use crate::tray::MonitorTray;
use fs2::FileExt;
use ksni::TrayService;
use notify_rust::Notification;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread::Thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const APP_ID: &str = "monitor-toggle-tray";
pub const APP_NAME: &str = "Monitor Input & Layout Switcher";
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STARTUP_STATUS: &str = "Starting up: refreshing monitor state in the background.";

static LAST_COMMAND_FAILURE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

#[derive(Clone)]
pub struct SharedState {
    pub config_store: ConfigStore,
    pub monitor_cache: MonitorCache,
    pub switch_in_progress: Arc<AtomicBool>,
    pub last_status: Arc<Mutex<Option<String>>>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsReport {
    pub lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SettingsView {
    pub primary_monitor_id: Option<String>,
    pub autostart_enabled: bool,
    pub status_text: String,
    pub diagnostics: Vec<String>,
    pub monitors: Vec<SettingsMonitorView>,
}

#[derive(Clone, Debug)]
pub struct SettingsMonitorView {
    pub id: String,
    pub display_name: String,
    pub output_name: String,
    pub connected: bool,
    pub active: bool,
    pub internal: bool,
    pub is_primary: bool,
    pub include_in_quick_switch: bool,
    pub laptop_input: Option<String>,
    pub toggle_input: Option<String>,
    pub current_input: Option<String>,
    pub available_inputs: Vec<monitor::InputSource>,
    pub ddc_status: String,
}

#[derive(Clone, Debug)]
pub struct SettingsUpdate {
    pub primary_monitor_id: Option<String>,
    pub autostart_enabled: bool,
    pub monitors: Vec<SettingsMonitorUpdate>,
}

#[derive(Clone, Debug)]
pub struct SettingsMonitorUpdate {
    pub id: String,
    pub display_name: String,
    pub internal: bool,
    pub include_in_quick_switch: bool,
    pub laptop_input: Option<String>,
    pub toggle_input: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuickSwitchReport {
    pub state: QuickSwitchState,
    pub controlled_monitors: usize,
    pub output_count: usize,
    pub layout_changed: bool,
    pub input_attempts: usize,
    pub switched_inputs: usize,
    pub notes: Vec<String>,
}

impl QuickSwitchReport {
    pub fn message(&self) -> String {
        let action_label = match self.state {
            QuickSwitchState::ControlledMonitorsOff => "Handed",
            QuickSwitchState::ControlledMonitorsOn => "Brought back",
        };
        let destination = match self.state {
            QuickSwitchState::ControlledMonitorsOff => "to the other device",
            QuickSwitchState::ControlledMonitorsOn => "to the laptop",
        };
        let layout_action = match self.state {
            QuickSwitchState::ControlledMonitorsOff => "disabled",
            QuickSwitchState::ControlledMonitorsOn => "restored",
        };
        let mut parts = vec![format!(
            "Quick switch complete: {action_label} {} controlled monitor(s) {destination}.",
            self.controlled_monitors
        )];
        parts.push(if self.layout_changed {
            format!(
                "Layout {layout_action} for {} output(s).",
                self.output_count
            )
        } else {
            "Layout unchanged.".into()
        });
        parts.push(if self.input_attempts > 0 {
            format!(
                "Input switches: {}/{} command(s) succeeded.",
                self.switched_inputs, self.input_attempts
            )
        } else {
            "Input switches: none attempted.".into()
        });
        if !self.notes.is_empty() {
            parts.push(format!("Issues: {}", self.notes.join(" | ")));
        }
        parts.join(" ")
    }
}

fn set_last_status(shared: &SharedState, status: impl Into<String>) {
    *shared.last_status.lock().unwrap() = Some(status.into());
}

fn startup_ready_message(shared: &SharedState, snapshot: &MonitorSnapshot) -> String {
    let primary = resolve_primary(snapshot, shared)
        .map(|monitor| monitor.display_name)
        .unwrap_or_else(|| "Unavailable".into());
    let controlled_count = controlled_monitors(snapshot, shared).len();

    if snapshot.monitors.is_empty() {
        "Ready. No monitors were detected yet.".into()
    } else {
        format!("Ready. Primary: {primary}. Controlled monitors configured: {controlled_count}.")
    }
}

fn spawn_startup_refresh(shared: SharedState, refresh_tx: Sender<()>) {
    std::thread::spawn(move || {
        match refresh_snapshot(&shared) {
            Ok(snapshot) => {
                let message = startup_ready_message(&shared, &snapshot);
                log_event(format!(
                    "main: initial monitor refresh completed: {message}"
                ));
                set_last_status(&shared, message);
            }
            Err(err) => {
                let message = format!("Startup refresh failed: {err}");
                log_event(format!("main: initial monitor refresh failed: {err}"));
                set_last_status(&shared, message);
            }
        }

        refresh_tx.send(()).ok();
    });
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

pub fn app_icon_path() -> Option<PathBuf> {
    let installed = app_icon_install_path();
    if installed.exists() {
        return Some(installed);
    }

    let bundled = bundled_icon_dir().join(format!("{APP_ID}.svg"));
    if bundled.exists() {
        return Some(bundled);
    }

    None
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

fn last_command_failure_cell() -> &'static Mutex<Option<String>> {
    LAST_COMMAND_FAILURE.get_or_init(|| Mutex::new(None))
}

fn record_last_command_failure(message: impl Into<String>) {
    *last_command_failure_cell().lock().unwrap() = Some(message.into());
}

fn last_command_failure() -> Option<String> {
    last_command_failure_cell().lock().unwrap().clone()
}

fn command_timeout(program: &str, args: &[String]) -> Duration {
    match program {
        "ddcutil" if args.iter().any(|arg| arg == "getvcp") => Duration::from_secs(3),
        "ddcutil" if args.iter().any(|arg| arg == "setvcp") => Duration::from_secs(5),
        "ddcutil" => Duration::from_secs(8),
        "xrandr" | "kscreen-doctor" => Duration::from_secs(5),
        "qdbus6" => Duration::from_secs(3),
        _ => Duration::from_secs(10),
    }
}

fn default_command_timeout(program: &str) -> Duration {
    command_timeout(program, &[])
}

fn read_child_stream<R: Read>(stream: &mut Option<R>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut reader) = stream.take() {
        let _ = reader.read_to_end(&mut buffer);
    }
    buffer
}

pub fn run_command(program: &str, args: &[String]) -> Result<String, String> {
    if should_log_command_start_and_success(program, args) {
        log_event(format!("run_command start: {program} {}", args.join(" ")));
    }
    let timeout = command_timeout(program, args);
    let started = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            let message = format!("{program}: {err}");
            record_last_command_failure(message.clone());
            message
        })?;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = read_child_stream(&mut child.stdout);
                let stderr = read_child_stream(&mut child.stderr);

                if status.success() {
                    let stdout_text = String::from_utf8_lossy(&stdout).into_owned();
                    if should_log_command_start_and_success(program, args) {
                        log_event(format!(
                            "run_command ok: {program} status={} elapsed_ms={} stdout={} bytes stderr={} bytes",
                            status,
                            started.elapsed().as_millis(),
                            stdout_text.len(),
                            stderr.len()
                        ));
                    }
                    return Ok(stdout_text);
                }

                let stderr_text = String::from_utf8_lossy(&stderr).trim().to_string();
                let stdout_text = String::from_utf8_lossy(&stdout).trim().to_string();
                let detail = if !stderr_text.is_empty() {
                    stderr_text
                } else {
                    stdout_text
                };
                log_event(format!(
                    "run_command err: {program} status={} elapsed_ms={} detail={detail}",
                    status,
                    started.elapsed().as_millis()
                ));
                let message = if detail.is_empty() {
                    format!("{program} exited with status {status}")
                } else {
                    format!("{program}: {detail}")
                };
                record_last_command_failure(message.clone());
                return Err(message);
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = String::from_utf8_lossy(&read_child_stream(&mut child.stderr))
                    .trim()
                    .to_string();
                let stdout = String::from_utf8_lossy(&read_child_stream(&mut child.stdout))
                    .trim()
                    .to_string();
                let detail = if !stderr.is_empty() { stderr } else { stdout };
                let message = if detail.is_empty() {
                    format!("{program}: timed out after {} ms", timeout.as_millis())
                } else {
                    format!(
                        "{program}: timed out after {} ms ({detail})",
                        timeout.as_millis()
                    )
                };
                log_event(format!(
                    "run_command timeout: {program} elapsed_ms={} args={}",
                    started.elapsed().as_millis(),
                    args.join(" ")
                ));
                record_last_command_failure(message.clone());
                return Err(message);
            }
            Ok(None) => std::thread::sleep(COMMAND_POLL_INTERVAL),
            Err(err) => {
                let message = format!("{program}: {err}");
                record_last_command_failure(message.clone());
                return Err(message);
            }
        }
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
    monitor::invalidate_ddc_input_cache();
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
    let config = shared.config_store.current();

    if let Some(primary) = configured_primary(snapshot, config.primary_monitor_id.as_deref()) {
        return Some(primary);
    }

    if let Some(internal_monitor) = snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.connected && monitor.internal)
    {
        return Some(internal_monitor.clone());
    }

    snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.connected)
        .cloned()
}

fn configured_primary(snapshot: &MonitorSnapshot, primary_id: Option<&str>) -> Option<MonitorInfo> {
    let primary_id = primary_id?;

    snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.id == primary_id && monitor.connected)
        .cloned()
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

pub fn current_switch_state(
    shared: &SharedState,
    snapshot: &MonitorSnapshot,
) -> Option<QuickSwitchState> {
    shared
        .config_store
        .current()
        .last_quick_switch_state
        .or_else(|| inferred_switch_state(snapshot))
}

pub fn diagnostics_report(shared: &SharedState) -> DiagnosticsReport {
    let snapshot = current_snapshot(shared).ok();
    let config = shared.config_store.current();
    let mut lines = Vec::new();
    let ddc_cache = monitor::ddc_cache_status();

    lines.push(format!(
        "Quick switch busy: {}",
        if shared.switch_in_progress.load(Ordering::SeqCst) {
            "yes"
        } else {
            "no"
        }
    ));
    lines.push(format!(
        "Monitor snapshot cache age: {}",
        shared
            .monitor_cache
            .last_refresh_age()
            .map(format_duration)
            .unwrap_or_else(|| "empty".into())
    ));
    lines.push(format!(
        "DDC cache: {} monitors, discovery age {}, input age {}",
        ddc_cache.monitor_count,
        ddc_cache
            .discovery_age
            .map(format_duration)
            .unwrap_or_else(|| "empty".into()),
        ddc_cache
            .input_age
            .map(format_duration)
            .unwrap_or_else(|| "empty".into())
    ));

    match display::discover_outputs() {
        Ok((backend, outputs)) => lines.push(format!(
            "Display backend: {backend:?} ({} outputs detected)",
            outputs.len()
        )),
        Err(err) => lines.push(format!("Display backend: unavailable ({err})")),
    }

    if let Some(snapshot) = snapshot.as_ref() {
        let controlled = controlled_monitors(snapshot, shared);
        lines.push(format!("Detected monitors: {}", snapshot.monitors.len()));
        lines.push(format!("Controlled monitors: {}", controlled.len()));
        lines.push(format!(
            "Last known switch state: {}",
            current_switch_state(shared, snapshot)
                .map(quick_switch_state_label)
                .unwrap_or("unknown")
        ));
        let saved_layout_count = controlled
            .iter()
            .filter(|monitor| {
                config.settings(&monitor.id).is_some_and(|settings| {
                    settings.saved_position_x.is_some()
                        && settings.saved_position_y.is_some()
                        && settings.saved_width.is_some()
                        && settings.saved_height.is_some()
                })
            })
            .count();
        lines.push(format!(
            "Saved layout coverage: {saved_layout_count}/{} controlled monitors",
            controlled.len()
        ));
    } else {
        lines.push("Detected monitors: unavailable".into());
        lines.push("Controlled monitors: unavailable".into());
        lines.push("Last known switch state: unavailable".into());
        lines.push("Saved layout coverage: unavailable".into());
    }

    for program in ["ddcutil", "xrandr", "kscreen-doctor"] {
        lines.push(format!(
            "{program}: {} (timeout {})",
            if command_exists(program) {
                "available"
            } else {
                "missing"
            },
            format_duration(default_command_timeout(program))
        ));
    }

    lines.push(format!(
        "Tray icon asset: {}",
        if tray_icon_available() {
            "available"
        } else {
            "missing"
        }
    ));
    lines.push(format!(
        "Autostart: {}",
        if autostart_enabled() {
            "enabled"
        } else {
            "disabled"
        }
    ));
    lines.push(format!(
        "Last command failure: {}",
        last_command_failure().unwrap_or_else(|| "none".into())
    ));
    lines.push(format!("Log file: {}", log_file_path().to_string_lossy()));

    DiagnosticsReport { lines }
}

pub fn load_settings_view(shared: &SharedState) -> Result<SettingsView, String> {
    let snapshot = refresh_snapshot(shared)?;
    let config = shared.config_store.current();
    let resolved_primary_id = resolve_primary(&snapshot, shared).map(|monitor| monitor.id);
    let diagnostics = diagnostics_report(shared).lines;
    let status_text = current_status_text(shared);

    let monitors = snapshot
        .monitors
        .iter()
        .map(|monitor| {
            let settings = config.settings(&monitor.id);
            let available_inputs = monitor
                .ddc
                .as_ref()
                .map(|ddc| {
                    if ddc.supported_inputs.is_empty() {
                        monitor::fallback_input_choices(ddc.current_input.as_deref())
                    } else {
                        ddc.supported_inputs.clone()
                    }
                })
                .unwrap_or_default();
            let ddc_status = match monitor.ddc.as_ref() {
                Some(ddc) => format!(
                    "DDC/CI: {}{}",
                    if ddc.input_switching_supported {
                        "available"
                    } else {
                        "limited"
                    },
                    if ddc.capabilities_known {
                        ", inputs detected"
                    } else {
                        ", using fallback choices"
                    }
                ),
                None => "DDC/CI: unavailable".into(),
            };

            SettingsMonitorView {
                id: monitor.id.clone(),
                display_name: monitor.display_name.clone(),
                output_name: monitor.output_name.clone(),
                connected: monitor.connected,
                active: monitor.active,
                internal: monitor.internal,
                is_primary: resolved_primary_id.as_deref() == Some(monitor.id.as_str()),
                include_in_quick_switch: settings
                    .is_some_and(|settings| settings.include_in_quick_switch),
                laptop_input: settings.and_then(|settings| settings.laptop_input.clone()),
                toggle_input: settings.and_then(|settings| settings.toggle_input.clone()),
                current_input: monitor
                    .ddc
                    .as_ref()
                    .and_then(|ddc| ddc.current_input.clone()),
                available_inputs,
                ddc_status,
            }
        })
        .collect::<Vec<_>>();

    Ok(SettingsView {
        primary_monitor_id: resolved_primary_id,
        autostart_enabled: autostart_enabled(),
        status_text,
        diagnostics,
        monitors,
    })
}

pub fn apply_settings(shared: &SharedState, update: SettingsUpdate) -> Result<String, String> {
    shared.config_store.update(|config| {
        config.primary_monitor_id = update.primary_monitor_id.clone();

        for monitor in &update.monitors {
            let settings = config.settings_mut_or_insert(&monitor.id, &monitor.display_name);
            settings.include_in_quick_switch = if monitor.internal {
                false
            } else {
                monitor.include_in_quick_switch
            };
            settings.laptop_input = monitor.laptop_input.clone();
            settings.toggle_input = if monitor.internal {
                None
            } else {
                monitor.toggle_input.clone()
            };
        }
    })?;

    let autostart_was_enabled = autostart_enabled();
    if autostart_was_enabled != update.autostart_enabled {
        set_autostart(update.autostart_enabled)?;
    }

    shared.monitor_cache.invalidate();
    *shared.last_status.lock().unwrap() = Some("Settings saved.".into());
    Ok("Settings saved.".into())
}

pub fn quick_switch(shared: &SharedState) -> Result<String, String> {
    if shared
        .switch_in_progress
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A quick switch is already running.".into());
    }

    let result = quick_switch_inner(shared).map(|report| report.message());
    shared.switch_in_progress.store(false, Ordering::SeqCst);
    result
}

fn quick_switch_inner(shared: &SharedState) -> Result<QuickSwitchReport, String> {
    let snapshot = refresh_snapshot(shared)?;
    let primary = resolve_primary(&snapshot, shared)
        .ok_or_else(|| "No primary display is currently available.".to_string())?;
    let controlled = controlled_monitors(&snapshot, shared);

    if controlled.is_empty() {
        return Err("No external monitors are selected for quick switch.".into());
    }

    let direction = infer_desired_switch_direction(shared, &snapshot, &controlled);
    let config = shared.config_store.current();
    let mut notes = Vec::new();
    let mut input_attempts = 0;
    let mut switched_inputs = 0;

    let outputs = controlled
        .iter()
        .map(|monitor| display::OutputLayout {
            name: monitor.output_name.clone(),
            position: config.settings(&monitor.id).and_then(|settings| {
                match (settings.saved_position_x, settings.saved_position_y) {
                    (Some(x), Some(y)) => Some((x, y)),
                    _ => None,
                }
            }),
            size: config.settings(&monitor.id).and_then(|settings| {
                match (settings.saved_width, settings.saved_height) {
                    (Some(width), Some(height)) => Some((width, height)),
                    _ => None,
                }
            }),
        })
        .collect::<Vec<_>>();
    let mut layout_changed = false;

    if direction == QuickSwitchState::ControlledMonitorsOff {
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
        layout_changed = true;
        if let Err(err) =
            wait_for_output_activity_state(&output_names, false, Duration::from_secs(3))
        {
            notes.push(format!("layout verification after disable: {err}"));
        }
        refresh_plasma_task_manager_after_primary_only_switch();
    }

    for monitor in &controlled {
        let settings = config.settings(&monitor.id);
        let desired_input = match direction {
            QuickSwitchState::ControlledMonitorsOff => {
                settings.and_then(|settings| settings.toggle_input.as_deref())
            }
            QuickSwitchState::ControlledMonitorsOn => {
                settings.and_then(|settings| settings.laptop_input.as_deref())
            }
        };

        if let (Some(ddc), Some(input)) = (monitor.ddc.as_ref(), desired_input) {
            input_attempts += 1;
            if let Err(err) = monitor::set_input_for_monitor(ddc.display_number, input) {
                notes.push(format!("{}: {err}", monitor.display_name));
            } else {
                switched_inputs += 1;
                if let Err(err) =
                    wait_for_monitor_input(ddc.display_number, input, Duration::from_secs(2))
                {
                    notes.push(format!(
                        "{}: switch command sent but verification failed: {err}",
                        monitor.display_name
                    ));
                }
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
                if direction == QuickSwitchState::ControlledMonitorsOff {
                    "toggle-to"
                } else {
                    "laptop"
                }
            ));
        }
    }

    if direction == QuickSwitchState::ControlledMonitorsOn {
        let primary_position = config.settings(&primary.id).and_then(|settings| {
            match (settings.saved_position_x, settings.saved_position_y) {
                (Some(x), Some(y)) => Some((x, y)),
                _ => primary.position,
            }
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
        layout_changed = true;
        let output_names = outputs
            .iter()
            .map(|layout| layout.name.clone())
            .collect::<Vec<_>>();
        if let Err(err) =
            wait_for_output_activity_state(&output_names, true, Duration::from_secs(5))
        {
            notes.push(format!("layout verification after restore: {err}"));
        }
    } else {
        log_event("quick_switch: controlled monitors switched away from laptop inputs");
    }

    shared.config_store.update(|config| {
        config.last_quick_switch_state = Some(direction);
    })?;
    shared.monitor_cache.invalidate();
    let report = QuickSwitchReport {
        state: direction,
        controlled_monitors: controlled.len(),
        output_count: outputs.len(),
        layout_changed,
        input_attempts,
        switched_inputs,
        notes,
    };
    set_last_status(shared, report.message());
    Ok(report)
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
        switch_in_progress: Arc::new(AtomicBool::new(false)),
        last_status: Arc::new(Mutex::new(Some(STARTUP_STATUS.into()))),
    };

    let quit_signal = QuitSignal::new();
    let (refresh_tx, refresh_rx) = std::sync::mpsc::channel();
    let settings_window = settings_window::spawn(shared.clone(), refresh_tx.clone());
    let service = TrayService::new(MonitorTray {
        quit_signal: quit_signal.clone(),
        shared: shared.clone(),
        refresh_tx: refresh_tx.clone(),
        settings_window,
    });
    let handle = service.handle();
    service.spawn();
    spawn_startup_refresh(shared.clone(), refresh_tx.clone());

    let refresh_state = shared.clone();
    let periodic_refresh_tx = refresh_tx.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(15));
            if !refresh_state.switch_in_progress.load(Ordering::SeqCst) {
                refresh_state.monitor_cache.invalidate();
            }
            periodic_refresh_tx.send(()).ok();
        }
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

fn sync_config_with_snapshot(
    shared: &SharedState,
    snapshot: &MonitorSnapshot,
) -> Result<(), String> {
    shared.config_store.update(|config| {
        config.primary_monitor_id =
            configured_primary(snapshot, config.primary_monitor_id.as_deref())
                .map(|monitor| monitor.id)
                .or_else(|| {
                    snapshot
                        .monitors
                        .iter()
                        .find(|monitor| monitor.connected && monitor.internal)
                        .map(|monitor| monitor.id.clone())
                })
                .or_else(|| {
                    snapshot
                        .monitors
                        .iter()
                        .find(|monitor| monitor.connected)
                        .map(|monitor| monitor.id.clone())
                });

        for monitor in &snapshot.monitors {
            let settings = config.settings_mut_or_insert(&monitor.id, &monitor.display_name);
            if settings.laptop_input.is_none()
                && monitor.active
                && let Some(current_input) = monitor
                    .ddc
                    .as_ref()
                    .and_then(|ddc| ddc.current_input.clone())
            {
                settings.laptop_input = Some(current_input);
            }
        }
    })?;

    Ok(())
}

pub fn current_status_text(shared: &SharedState) -> String {
    if shared.switch_in_progress.load(Ordering::SeqCst) {
        return "Quick switch in progress".into();
    }

    if let Some(status) = shared.last_status.lock().unwrap().clone() {
        return status;
    }

    let snapshot = match current_snapshot(shared) {
        Ok(snapshot) => snapshot,
        Err(_) => return "Monitor state unavailable".into(),
    };

    current_switch_state(shared, &snapshot)
        .map(|state| format!("Last known state: {}", quick_switch_state_label(state)))
        .unwrap_or_else(|| "Quick switch not run yet".into())
}

pub fn save_current_layout(shared: &SharedState) -> Result<String, String> {
    let snapshot = refresh_snapshot(shared)?;
    save_current_layout_snapshot(shared, &snapshot)
}

fn save_current_layout_snapshot(
    shared: &SharedState,
    snapshot: &MonitorSnapshot,
) -> Result<String, String> {
    if let Some(saved_layout) = load_kwin_output_layout(snapshot)? {
        shared.config_store.update(|config| {
            for entry in &saved_layout {
                let settings =
                    config.settings_mut_or_insert(&entry.monitor_id, &entry.display_name);
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
                    entry.output_name,
                    entry.position.0,
                    entry.position.1,
                    entry.size.0,
                    entry.size.1
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        log_event(format!(
            "save_current_layout: saved positions from kwinoutputconfig={summary}"
        ));
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

fn load_kwin_output_layout(
    snapshot: &MonitorSnapshot,
) -> Result<Option<Vec<SavedLayoutEntry>>, String> {
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

fn command_exists(program: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .any(|dir| dir.join(program).exists())
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn inferred_switch_state(snapshot: &MonitorSnapshot) -> Option<QuickSwitchState> {
    let any_external_active = snapshot
        .monitors
        .iter()
        .any(|monitor| !monitor.internal && monitor.active);

    Some(if any_external_active {
        QuickSwitchState::ControlledMonitorsOn
    } else {
        QuickSwitchState::ControlledMonitorsOff
    })
}

fn infer_desired_switch_direction(
    shared: &SharedState,
    snapshot: &MonitorSnapshot,
    controlled: &[MonitorInfo],
) -> QuickSwitchState {
    let any_controlled_active = controlled.iter().any(|monitor| monitor.active);
    let last_state = shared.config_store.current().last_quick_switch_state;

    match (any_controlled_active, last_state) {
        (true, _) => QuickSwitchState::ControlledMonitorsOff,
        (false, Some(QuickSwitchState::ControlledMonitorsOff)) => {
            QuickSwitchState::ControlledMonitorsOn
        }
        (false, Some(QuickSwitchState::ControlledMonitorsOn)) => {
            QuickSwitchState::ControlledMonitorsOn
        }
        (false, None) => inferred_switch_state(snapshot)
            .map(|state| match state {
                QuickSwitchState::ControlledMonitorsOn => QuickSwitchState::ControlledMonitorsOff,
                QuickSwitchState::ControlledMonitorsOff => QuickSwitchState::ControlledMonitorsOn,
            })
            .unwrap_or(QuickSwitchState::ControlledMonitorsOn),
    }
}

fn quick_switch_state_label(state: QuickSwitchState) -> &'static str {
    match state {
        QuickSwitchState::ControlledMonitorsOn => "controlled monitors on",
        QuickSwitchState::ControlledMonitorsOff => "controlled monitors off",
    }
}

fn wait_for_monitor_input(
    display_number: u32,
    expected_input: &str,
    timeout: Duration,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        let args = vec![
            "--display".into(),
            display_number.to_string(),
            "getvcp".into(),
            monitor::INPUT_VCP_CODE.into(),
            "--brief".into(),
        ];
        if let Ok(output) = run_command("ddcutil", &args)
            && let Some(current) = parse_current_input(&output)
            && current == expected_input
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    Err("Timed out waiting for monitor input change.".into())
}

fn wait_for_output_activity_state(
    output_names: &[String],
    expected_active: bool,
    timeout: Duration,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        let (_, outputs) = display::discover_outputs()?;
        let all_match = output_names.iter().all(|name| {
            outputs
                .iter()
                .find(|output| &output.name == name)
                .is_some_and(|output| {
                    output.connected && output.current_mode.is_some() == expected_active
                })
        });

        if all_match {
            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(250));
    }

    Err("Timed out waiting for display state change.".into())
}

fn parse_current_input(output: &str) -> Option<String> {
    for marker in ["current value = ", "sl=", "SNC x", "SNC X"] {
        if let Some(start) = output.find(marker) {
            let value = output[start + marker.len()..]
                .chars()
                .skip_while(|ch| ch.is_whitespace())
                .take_while(|ch| ch.is_ascii_hexdigit() || *ch == 'x' || *ch == 'X')
                .collect::<String>();
            let normalized = value
                .trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            if let Ok(number) = u8::from_str_radix(normalized, 16) {
                return Some(format!("0x{number:02x}"));
            }
        }
    }
    None
}

fn refresh_plasma_task_manager_after_primary_only_switch() {
    if !is_kde_wayland_session() {
        return;
    }

    if !command_exists("qdbus6") {
        log_event("refresh_plasma_task_manager_after_primary_only_switch: qdbus6 is not available");
        return;
    }

    log_event(
        "refresh_plasma_task_manager_after_primary_only_switch: refreshing Plasma task-manager applets",
    );

    let script = r#"
const taskManagerTypes = [
  "org.kde.plasma.icontasks",
  "org.kde.plasma.taskmanager"
];

function refreshTaskManagersForContainment(containment) {
  if (!containment || typeof containment.widgets !== "function") {
    return 0;
  }

  let refreshed = 0;
  const widgets = containment.widgets();

  for (let i = 0; i < widgets.length; ++i) {
    const widget = widgets[i];
    if (!widget || taskManagerTypes.indexOf(widget.type) === -1) {
      continue;
    }

    if (typeof widget.reloadConfig === "function") {
      widget.reloadConfig();
      ++refreshed;
      continue;
    }

    if (typeof widget.currentConfigGroup === "function" &&
        typeof widget.writeConfig === "function" &&
        typeof widget.readConfig === "function") {
      const originalGroup = widget.currentConfigGroup();
      widget.currentConfigGroup = ["General"];
      const filterByScreen = widget.readConfig("filterByScreen", "");
      widget.writeConfig("filterByScreen", filterByScreen);
      widget.currentConfigGroup = originalGroup;
      ++refreshed;
    }
  }

  return refreshed;
}

let refreshed = 0;
const plasmaPanels = panels();
for (let i = 0; i < plasmaPanels.length; ++i) {
  refreshed += refreshTaskManagersForContainment(plasmaPanels[i]);
}

const plasmaDesktops = desktops();
for (let i = 0; i < plasmaDesktops.length; ++i) {
  refreshed += refreshTaskManagersForContainment(plasmaDesktops[i]);
}

print("refreshed-task-managers=" + refreshed);
"#;
    let args = vec![
        "org.kde.plasmashell".into(),
        "/PlasmaShell".into(),
        "org.kde.PlasmaShell.evaluateScript".into(),
        script.into(),
    ];

    if let Err(err) = run_command("qdbus6", &args) {
        log_event(format!(
            "refresh_plasma_task_manager_after_primary_only_switch: targeted refresh failed: {err}"
        ));
    }
}

fn is_kde_wayland_session() -> bool {
    let session_type = env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let current_desktop = env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();

    session_type == "wayland" && current_desktop.contains("kde")
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
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={APP_NAME}\nComment=Tray app for switching monitor inputs and restoring desktop layouts\nExec=\"{exec}\"\nIcon={icon}\nTerminal=false\nCategories=Utility;\nX-GNOME-Autostart-enabled=true\n"
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
        .map_err(|_| format!("Another instance of {APP_NAME} is already running."))?;

    file.set_len(0)
        .map_err(|err| format!("Could not initialize lock file {path}: {err}"))?;
    write!(file, "{}", std::process::id())
        .map_err(|err| format!("Could not write lock file {path}: {err}"))?;

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::MonitorInfo;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_shared_state(last_quick_switch_state: Option<QuickSwitchState>) -> SharedState {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let shared = SharedState {
            config_store: ConfigStore::new_for_tests(PathBuf::from(format!(
                "/tmp/monitor-toggle-tray-test-{}-{unique}.toml",
                std::process::id(),
            ))),
            monitor_cache: MonitorCache::default(),
            switch_in_progress: Arc::new(AtomicBool::new(false)),
            last_status: Arc::new(Mutex::new(None)),
        };
        shared
            .config_store
            .update(|config| {
                config
                    .monitor_settings
                    .retain(|settings| settings.monitor_id != "external:HDMI-A-1");
                config
                    .monitor_settings
                    .push(crate::config::MonitorSettings {
                        monitor_id: "external:HDMI-A-1".into(),
                        display_name: "Dell".into(),
                        include_in_quick_switch: true,
                        ..Default::default()
                    });
                config.last_quick_switch_state = last_quick_switch_state;
            })
            .unwrap();
        shared
    }

    fn snapshot_with_controlled_active(active: bool) -> MonitorSnapshot {
        MonitorSnapshot {
            monitors: vec![
                MonitorInfo {
                    id: "internal:eDP-1".into(),
                    display_name: "Built-in display".into(),
                    output_name: "eDP-1".into(),
                    connected: true,
                    active: true,
                    internal: true,
                    position: Some((0, 0)),
                    current_mode: Some((1920, 1080)),
                    ddc: None,
                },
                MonitorInfo {
                    id: "external:HDMI-A-1".into(),
                    display_name: "Dell".into(),
                    output_name: "HDMI-A-1".into(),
                    connected: true,
                    active,
                    internal: false,
                    position: Some((0, 0)),
                    current_mode: active.then_some((2560, 1440)),
                    ddc: None,
                },
            ],
        }
    }

    #[test]
    fn resolve_primary_prefers_connected_configured_monitor() {
        let shared = test_shared_state(None);
        shared
            .config_store
            .update(|config| {
                config.primary_monitor_id = Some("external:HDMI-A-1".into());
            })
            .unwrap();

        let resolved = resolve_primary(&snapshot_with_controlled_active(true), &shared)
            .map(|monitor| monitor.id);

        assert_eq!(resolved, Some("external:HDMI-A-1".into()));
    }

    #[test]
    fn sync_config_preserves_connected_primary_selection() {
        let shared = test_shared_state(None);
        shared
            .config_store
            .update(|config| {
                config.primary_monitor_id = Some("external:HDMI-A-1".into());
            })
            .unwrap();

        sync_config_with_snapshot(&shared, &snapshot_with_controlled_active(true)).unwrap();

        assert_eq!(
            shared.config_store.current().primary_monitor_id,
            Some("external:HDMI-A-1".into())
        );
    }

    #[test]
    fn parses_current_input_from_multiple_formats() {
        assert_eq!(parse_current_input("VCP 60 SNC x11"), Some("0x11".into()));
        assert_eq!(
            parse_current_input("VCP code 0x60 (Input Source): HDMI-1 (sl=0x11)"),
            Some("0x11".into())
        );
    }

    #[test]
    fn infers_switch_direction_from_active_controlled_monitors() {
        let shared = test_shared_state(None);
        let snapshot = snapshot_with_controlled_active(true);
        let controlled = controlled_monitors(&snapshot, &shared);

        assert_eq!(
            infer_desired_switch_direction(&shared, &snapshot, &controlled),
            QuickSwitchState::ControlledMonitorsOff
        );
    }

    #[test]
    fn defaults_to_turning_monitors_on_when_all_controlled_outputs_are_off() {
        let shared = test_shared_state(Some(QuickSwitchState::ControlledMonitorsOff));
        let snapshot = snapshot_with_controlled_active(false);
        let controlled = controlled_monitors(&snapshot, &shared);

        assert_eq!(
            infer_desired_switch_direction(&shared, &snapshot, &controlled),
            QuickSwitchState::ControlledMonitorsOn
        );
    }

    #[test]
    fn quick_switch_report_message_describes_action_and_counts() {
        let report = QuickSwitchReport {
            state: QuickSwitchState::ControlledMonitorsOff,
            controlled_monitors: 2,
            output_count: 2,
            layout_changed: true,
            input_attempts: 1,
            switched_inputs: 1,
            notes: vec!["Dell: no toggle-to input is configured".into()],
        };

        assert_eq!(
            report.message(),
            "Quick switch complete: Handed 2 controlled monitor(s) to the other device. Layout disabled for 2 output(s). Input switches: 1/1 command(s) succeeded. Issues: Dell: no toggle-to input is configured"
        );
    }
}
