use fs2::FileExt;
use ksni::{Icon, Tray, TrayService, menu};
use notify_rust::Notification;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread::Thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const APP_ID: &str = "monitor-toggle-tray";
const APP_NAME: &str = "Monitor Toggle";
const INPUT_VCP_CODE: &str = "60";
const HDMI1: &str = "0x11";
const HDMI2: &str = "0x12";
const INPUT_CACHE_TTL: Duration = Duration::from_secs(2);
const HDMI1_LAYOUT_ATTEMPTS: usize = 10;
const HDMI1_LAYOUT_RETRY_DELAY: Duration = Duration::from_millis(700);
const HDMI2_LAYOUT_ATTEMPTS: usize = 10;
const HDMI2_LAYOUT_RETRY_DELAY: Duration = Duration::from_millis(700);
const HDMI2_LAYOUT_STABILITY_CHECKS: usize = 4;
const SAVED_HDMI1_EXTERNAL_NAME: &str = "HDMI-A-1";
const SAVED_HDMI1_EXTERNAL_X: u32 = 0;
const SAVED_HDMI1_EXTERNAL_Y: u32 = 0;
const SAVED_HDMI1_INTERNAL_NAME: &str = "eDP-1";
const SAVED_HDMI1_INTERNAL_X: u32 = 459;
const SAVED_HDMI1_INTERNAL_Y: u32 = 1440;
const MODE_RESOLVE_ATTEMPTS: usize = 5;
const MODE_RESOLVE_RETRY_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Color {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone, Copy)]
struct IconPalette {
    background: Color,
    screen: Color,
    accent: Color,
}

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
    current_mode: Option<(u32, u32)>,
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

fn log_event(message: impl AsRef<str>) {
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

fn should_log_command_start_and_success(program: &str, args: &[String]) -> bool {
    match program {
        "ddcutil" => args.first().map(|arg| arg.as_str()) == Some("setvcp"),
        "xrandr" => !args.iter().any(|arg| arg == "--query"),
        "kscreen-doctor" => !args.iter().any(|arg| arg == "-o"),
        _ => true,
    }
}

fn describe_outputs(outputs: &[DisplayOutput]) -> String {
    outputs
        .iter()
        .map(|output| {
            format!(
                "{}(id={}, connected={}, internal={}, mode={})",
                output.name,
                output.id,
                output.connected,
                output.internal,
                output
                    .current_mode
                    .map(|(width, height)| format!("{width}x{height}"))
                    .unwrap_or_else(|| "none".into())
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
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

fn desktop_icon_value() -> String {
    let installed_icon = app_icon_install_path();

    if installed_icon.exists() {
        installed_icon.to_string_lossy().into_owned()
    } else {
        "video-display".into()
    }
}

fn tray_icon_theme_path() -> String {
    let icon_dir = app_icon_install_dir();

    if icon_dir.exists() {
        icon_dir.to_string_lossy().into_owned()
    } else {
        String::new()
    }
}

fn icon_palette(input: Option<&str>) -> IconPalette {
    match input {
        Some(HDMI1) => IconPalette {
            background: Color::rgb(15, 92, 86),
            screen: Color::rgb(10, 33, 38),
            accent: Color::rgb(94, 234, 212),
        },
        Some(HDMI2) => IconPalette {
            background: Color::rgb(148, 76, 22),
            screen: Color::rgb(55, 25, 9),
            accent: Color::rgb(251, 191, 36),
        },
        _ => IconPalette {
            background: Color::rgb(71, 85, 105),
            screen: Color::rgb(15, 23, 42),
            accent: Color::rgb(226, 232, 240),
        },
    }
}

fn monitor_tray_icon(input: Option<&str>, size: i32) -> Icon {
    let size = size.max(16);
    let palette = icon_palette(input);
    let mut data = vec![Color::rgba(0, 0, 0, 0); (size * size) as usize];
    let inset = (size / 12).max(1);
    let radius = (size / 4).max(2);
    let panel_height = size - inset * 2;
    let panel_width = size - inset * 2;

    fill_round_rect(
        &mut data,
        size,
        inset,
        inset,
        panel_width,
        panel_height,
        radius,
        palette.background,
    );

    let screen_margin_x = (size / 5).max(2);
    let screen_top = (size / 4).max(3);
    let screen_width = size - screen_margin_x * 2;
    let screen_height = (size * 6 / 16).max(5);
    let bezel = (size / 16).max(1);

    fill_round_rect(
        &mut data,
        size,
        screen_margin_x,
        screen_top,
        screen_width,
        screen_height,
        (size / 10).max(2),
        Color::rgb(248, 250, 252),
    );
    fill_round_rect(
        &mut data,
        size,
        screen_margin_x + bezel,
        screen_top + bezel,
        screen_width - bezel * 2,
        screen_height - bezel * 2,
        (size / 12).max(1),
        palette.screen,
    );

    let stem_width = (size / 8).max(2);
    let stem_height = (size / 8).max(2);
    let stem_x = (size - stem_width) / 2;
    let stem_y = screen_top + screen_height;
    fill_rect(
        &mut data,
        size,
        stem_x,
        stem_y,
        stem_width,
        stem_height,
        Color::rgb(248, 250, 252),
    );

    let base_width = (size / 3).max(5);
    let base_height = (size / 14).max(1);
    let base_x = (size - base_width) / 2;
    let base_y = stem_y + stem_height;
    fill_round_rect(
        &mut data,
        size,
        base_x,
        base_y,
        base_width,
        base_height + 1,
        (size / 20).max(1),
        Color::rgb(248, 250, 252),
    );

    let badge_size = (size / 3).max(5);
    let badge_x = size - inset - badge_size;
    let badge_y = size - inset - badge_size;
    fill_round_rect(
        &mut data,
        size,
        badge_x,
        badge_y,
        badge_size,
        badge_size,
        (badge_size / 3).max(2),
        palette.accent,
    );

    let digit_color = palette.screen;
    match input {
        Some(HDMI1) => draw_digit_one(&mut data, size, badge_x, badge_y, badge_size, digit_color),
        Some(HDMI2) => draw_digit_two(&mut data, size, badge_x, badge_y, badge_size, digit_color),
        _ => draw_dot(&mut data, size, badge_x, badge_y, badge_size, digit_color),
    }

    let mut argb = Vec::with_capacity((size * size * 4) as usize);
    for pixel in data {
        argb.push(pixel.a);
        argb.push(pixel.r);
        argb.push(pixel.g);
        argb.push(pixel.b);
    }

    Icon {
        width: size,
        height: size,
        data: argb,
    }
}

fn fill_rect(
    canvas: &mut [Color],
    canvas_size: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    color: Color,
) {
    let x_start = x.max(0);
    let y_start = y.max(0);
    let x_end = (x + width).min(canvas_size);
    let y_end = (y + height).min(canvas_size);

    for py in y_start..y_end {
        for px in x_start..x_end {
            canvas[(py * canvas_size + px) as usize] = color;
        }
    }
}

fn fill_round_rect(
    canvas: &mut [Color],
    canvas_size: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    color: Color,
) {
    if width <= 0 || height <= 0 {
        return;
    }

    let radius = radius.max(0).min(width / 2).min(height / 2);
    let x_end = (x + width).min(canvas_size);
    let y_end = (y + height).min(canvas_size);

    for py in y.max(0)..y_end {
        for px in x.max(0)..x_end {
            let dx = if px < x + radius {
                x + radius - px - 1
            } else if px >= x + width - radius {
                px - (x + width - radius)
            } else {
                0
            };

            let dy = if py < y + radius {
                y + radius - py - 1
            } else if py >= y + height - radius {
                py - (y + height - radius)
            } else {
                0
            };

            if dx * dx + dy * dy <= radius * radius {
                canvas[(py * canvas_size + px) as usize] = color;
            }
        }
    }
}

fn draw_digit_one(
    canvas: &mut [Color],
    canvas_size: i32,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let stroke = (badge_size / 6).max(1);
    let center = badge_x + badge_size / 2;
    let top = badge_y + (badge_size / 5);
    let height = badge_size - (badge_size / 3);

    fill_rect(
        canvas,
        canvas_size,
        center - stroke / 2,
        top,
        stroke,
        height,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        center - stroke,
        top + stroke / 2,
        stroke,
        stroke,
        color,
    );
}

fn draw_digit_two(
    canvas: &mut [Color],
    canvas_size: i32,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let stroke = (badge_size / 6).max(1);
    let left = badge_x + (badge_size / 5);
    let top = badge_y + (badge_size / 5);
    let width = badge_size - (badge_size / 3);
    let middle = badge_y + badge_size / 2;
    let bottom = badge_y + badge_size - (badge_size / 4);

    fill_rect(canvas, canvas_size, left, top, width, stroke, color);
    fill_rect(
        canvas,
        canvas_size,
        left + width - stroke,
        top,
        stroke,
        middle - top,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        left,
        middle - stroke / 2,
        width,
        stroke,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        left,
        middle,
        stroke,
        bottom - middle,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        left,
        bottom - stroke,
        width,
        stroke,
        color,
    );
}

fn draw_dot(
    canvas: &mut [Color],
    canvas_size: i32,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let dot_size = (badge_size / 4).max(1);
    let dot_x = badge_x + (badge_size - dot_size) / 2;
    let dot_y = badge_y + (badge_size - dot_size) / 2;
    fill_round_rect(
        canvas,
        canvas_size,
        dot_x,
        dot_y,
        dot_size,
        dot_size,
        (dot_size / 2).max(1),
        color,
    );
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

fn find_known_external_output(outputs: &[DisplayOutput]) -> Option<DisplayOutput> {
    outputs
        .iter()
        .find(|output| !output.internal && is_external_output(&output.name))
        .cloned()
}

fn has_connected_external_output(outputs: &[DisplayOutput]) -> bool {
    outputs.iter().any(|output| {
        output.connected
            && !output.internal
            && is_external_output(&output.name)
            && output.current_mode.is_some()
    })
}

fn parse_output_mode(token: &str) -> Option<(u32, u32)> {
    let geometry = token
        .split_once('+')
        .map(|(value, _)| value)
        .unwrap_or(token);
    let (width, height) = geometry.split_once('x')?;

    Some((width.parse().ok()?, height.parse().ok()?))
}

fn resolve_output_mode(output: &DisplayOutput) -> Option<(u32, u32)> {
    output.current_mode.or_else(|| {
        list_outputs(DisplayBackend::Xrandr)
            .ok()
            .and_then(|outputs| {
                outputs
                    .iter()
                    .find(|candidate| candidate.name == output.name)
                    .and_then(|candidate| candidate.current_mode)
                    .or_else(|| {
                        if output.internal {
                            outputs
                                .iter()
                                .find(|candidate| candidate.internal)
                                .and_then(|candidate| candidate.current_mode)
                        } else {
                            None
                        }
                    })
            })
    })
}

fn resolve_output_mode_with_retries(output: &DisplayOutput) -> Option<(u32, u32)> {
    for attempt in 0..MODE_RESOLVE_ATTEMPTS {
        if let Some(mode) = resolve_output_mode(output) {
            return Some(mode);
        }

        if attempt + 1 < MODE_RESOLVE_ATTEMPTS {
            std::thread::sleep(MODE_RESOLVE_RETRY_DELAY);
        }
    }

    None
}

fn saved_hdmi1_layout_positions(pair: &DisplayPair) -> Option<((u32, u32), (u32, u32))> {
    if pair.anchor.name == SAVED_HDMI1_INTERNAL_NAME
        && pair.external.name == SAVED_HDMI1_EXTERNAL_NAME
    {
        Some((
            (SAVED_HDMI1_EXTERNAL_X, SAVED_HDMI1_EXTERNAL_Y),
            (SAVED_HDMI1_INTERNAL_X, SAVED_HDMI1_INTERNAL_Y),
        ))
    } else {
        None
    }
}

fn strip_ansi_escape_sequences(text: &str) -> String {
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
    let icon = desktop_icon_value()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    Ok(format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={APP_NAME}\nComment=Tray app for switching monitor input\nExec=\"{exec}\"\nIcon={icon}\nTerminal=false\nCategories=Utility;\nX-GNOME-Autostart-enabled=true\n"
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

fn available_display_backends() -> Vec<DisplayBackend> {
    let session = env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let available = display_backend_candidates(&session)
        .into_iter()
        .collect::<Vec<_>>();
    log_event(format!(
        "available_display_backends: session={} backends={available:?}",
        session
    ));
    available
}

fn parse_kscreen_outputs(output: &str) -> Vec<DisplayOutput> {
    output
        .lines()
        .filter_map(|line| {
            let cleaned = strip_ansi_escape_sequences(line);
            let trimmed = cleaned.trim();
            if !trimmed.starts_with("Output") {
                return None;
            }

            let parts = trimmed.split_whitespace().collect::<Vec<_>>();
            let output_index = parts
                .iter()
                .position(|part| part.starts_with("Output"))
                .unwrap_or(0);
            let id = parts
                .get(output_index + 1)?
                .trim_end_matches(':')
                .to_string();
            let name = parts
                .get(output_index + 2)?
                .trim_end_matches(':')
                .to_string();
            let current_mode = parts.iter().find_map(|part| parse_output_mode(part));
            let connected = if trimmed.contains(" disconnected") {
                false
            } else if trimmed.contains(" connected") || trimmed.contains(" enabled") {
                true
            } else {
                current_mode.is_some()
            };

            Some(DisplayOutput {
                id,
                internal: is_internal_output(&name) || has_internal_marker(trimmed),
                connected,
                name,
                current_mode,
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
            let current_mode = parts.find_map(parse_output_mode);

            Some(DisplayOutput {
                id: name.clone(),
                internal: is_internal_output(&name) || has_internal_marker(trimmed),
                connected,
                name,
                current_mode,
            })
        })
        .collect()
}

fn list_outputs(backend: DisplayBackend) -> Result<Vec<DisplayOutput>, String> {
    let outputs = match backend {
        DisplayBackend::KscreenDoctor => {
            let args = vec!["-o".into()];
            let raw_output = run_command("kscreen-doctor", &args)?;
            let outputs = parse_kscreen_outputs(&raw_output);

            if outputs.is_empty() && !raw_output.trim().is_empty() {
                let preview = strip_ansi_escape_sequences(&raw_output)
                    .lines()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ");
                log_event(format!(
                    "list_outputs: backend=KscreenDoctor parse failed preview={preview}"
                ));
                return Err("Unable to parse kscreen-doctor output.".into());
            }

            Ok::<Vec<DisplayOutput>, String>(outputs)
        }
        DisplayBackend::Xrandr => {
            let args = vec!["--query".into()];
            Ok::<Vec<DisplayOutput>, String>(parse_xrandr_outputs(&run_command("xrandr", &args)?))
        }
    }?;

    log_event(format!(
        "list_outputs: backend={backend:?} outputs=[{}]",
        describe_outputs(&outputs)
    ));
    Ok(outputs)
}

fn detect_display_pair(backend: DisplayBackend) -> Result<DisplayPair, String> {
    let outputs = list_outputs(backend)?;

    let anchor = find_anchor_output(&outputs)
        .ok_or_else(|| "Could not find any active display output.".to_string())?;

    let external = match backend {
        DisplayBackend::KscreenDoctor => find_external_output(&outputs)
            .or_else(|| find_known_external_output(&outputs))
            .ok_or_else(|| "Could not find a connected secondary display.".to_string())?,
        DisplayBackend::Xrandr => find_external_output(&outputs)
            .ok_or_else(|| "Could not find a connected secondary display.".to_string())?,
    };

    log_event(format!(
        "detect_display_pair: backend={backend:?} anchor={} external={}",
        anchor.name, external.name
    ));
    Ok(DisplayPair { anchor, external })
}

fn detect_internal_anchor(
    backend: DisplayBackend,
) -> Result<(DisplayOutput, Vec<DisplayOutput>), String> {
    let outputs = list_outputs(backend)?;
    let internal = find_internal_output(&outputs)
        .ok_or_else(|| "Could not find the laptop's internal display.".to_string())?;

    log_event(format!(
        "detect_internal_anchor: backend={backend:?} internal={}",
        internal.name
    ));
    Ok((internal, outputs))
}

fn ensure_anchor_only(
    backend: DisplayBackend,
    anchor: &DisplayOutput,
    outputs: &[DisplayOutput],
) -> Result<(), String> {
    log_event(format!(
        "ensure_anchor_only: backend={backend:?} anchor={} outputs=[{}]",
        anchor.name,
        describe_outputs(outputs)
    ));
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
            let mut disable_args = Vec::new();

            for output in outputs.iter().filter(|output| output.name != anchor.name) {
                disable_args.push("--output".into());
                disable_args.push(output.name.clone());
                disable_args.push("--off".into());
            }

            if let Some((width, height)) = anchor.current_mode {
                disable_args.push("--fb".into());
                disable_args.push(format!("{width}x{height}"));
            }

            if !disable_args.is_empty() {
                log_event(format!(
                    "ensure_anchor_only: xrandr disable pass for anchor={} fb={:?}",
                    anchor.name, anchor.current_mode
                ));
                run_command("xrandr", &disable_args)?;
            }

            let enable_args = vec![
                "--output".into(),
                anchor.name.clone(),
                "--auto".into(),
                "--primary".into(),
            ];
            log_event(format!(
                "ensure_anchor_only: xrandr enable pass for anchor={}",
                anchor.name
            ));
            run_command("xrandr", &enable_args).map(|_| ())
        }
    }
}

fn set_extended(backend: DisplayBackend, pair: &DisplayPair) -> Result<(), String> {
    log_event(format!(
        "set_extended: backend={backend:?} anchor={} external={}",
        pair.anchor.name, pair.external.name
    ));
    match backend {
        DisplayBackend::KscreenDoctor => {
            if let Some(((external_x, external_y), (anchor_x, anchor_y))) =
                saved_hdmi1_layout_positions(pair)
            {
                let saved_layout_args = vec![
                    format!("output.{}.enable", pair.external.id),
                    format!(
                        "output.{}.position.{external_x},{external_y}",
                        pair.external.id
                    ),
                    format!("output.{}.enable", pair.anchor.id),
                    format!("output.{}.primary", pair.anchor.id),
                    format!("output.{}.position.{anchor_x},{anchor_y}", pair.anchor.id),
                ];
                return run_command("kscreen-doctor", &saved_layout_args).map(|_| ());
            }

            let anchor_height =
                resolve_output_mode_with_retries(&pair.anchor).map(|(_, height)| height);
            let Some(anchor_height) = anchor_height else {
                return Err("Could not determine laptop display mode.".into());
            };

            // Start with a guaranteed non-overlapping layout so the external output
            // becomes active without temporarily falling into mirror mode.
            let provisional_args = vec![
                format!("output.{}.enable", pair.anchor.id),
                format!("output.{}.primary", pair.anchor.id),
                format!("output.{}.position.0,0", pair.anchor.id),
                format!("output.{}.enable", pair.external.id),
                format!("output.{}.position.0,{anchor_height}", pair.external.id),
            ];
            run_command("kscreen-doctor", &provisional_args)?;

            let external_height =
                resolve_output_mode_with_retries(&pair.external).map(|(_, height)| height);
            let Some(external_height) = external_height else {
                return Err("Could not determine secondary display mode.".into());
            };

            let positioned_args = vec![
                format!("output.{}.enable", pair.external.id),
                format!("output.{}.position.0,0", pair.external.id),
                format!("output.{}.enable", pair.anchor.id),
                format!("output.{}.primary", pair.anchor.id),
                format!("output.{}.position.0,{external_height}", pair.anchor.id),
            ];
            run_command("kscreen-doctor", &positioned_args).map(|_| ())
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
                "--above".into(),
                pair.anchor.name.clone(),
            ];
            run_command("xrandr", &external_args).map(|_| ())
        }
    }
}

fn apply_laptop_only_layout(backend: DisplayBackend) -> Result<(), String> {
    log_event(format!("apply_laptop_only_layout: backend={backend:?}"));
    let (anchor, outputs) = detect_internal_anchor(backend)?;
    ensure_anchor_only(backend, &anchor, &outputs)
}

fn prepare_display_layout(target: SwitchTarget) -> Result<(), String> {
    let backends = available_display_backends();
    if backends.is_empty() {
        return Err("No supported display management tool was found.".to_string());
    }

    let mut last_error = "No supported display management tool was found.".to_string();

    for backend in backends {
        log_event(format!(
            "prepare_display_layout: target={} trying backend={backend:?}",
            target_label(target)
        ));

        let result = match target {
            SwitchTarget::Hdmi1 => prepare_hdmi1_layout(backend),
            SwitchTarget::Hdmi2 => prepare_hdmi2_layout(backend),
        };

        match result {
            Ok(()) => return Ok(()),
            Err(err) => {
                log_event(format!(
                    "prepare_display_layout: target={} backend={backend:?} failed: {err}",
                    target_label(target)
                ));
                last_error = err;
            }
        }
    }

    Err(last_error)
}

fn is_transient_hdmi1_layout_error(err: &str) -> bool {
    err.contains("Could not find any active display output.")
        || err.contains("Could not find a connected secondary display.")
        || err.contains("Unable to parse kscreen-doctor output.")
        || err.contains("BadMatch")
        || err.contains("Configure crtc")
        || err.contains("cannot find mode")
}

fn is_transient_hdmi2_layout_error(err: &str) -> bool {
    err.contains("Could not find the laptop's internal display.")
        || err.contains("Unable to parse kscreen-doctor output.")
        || err.contains("BadMatch")
        || err.contains("Configure crtc")
        || err.contains("cannot find mode")
}

fn prepare_hdmi1_layout(backend: DisplayBackend) -> Result<(), String> {
    let mut last_error = "Could not find a connected secondary display.".to_string();

    for attempt in 0..HDMI1_LAYOUT_ATTEMPTS {
        log_event(format!(
            "prepare_hdmi1_layout: attempt={} backend={backend:?}",
            attempt + 1
        ));
        let result = detect_display_pair(backend).and_then(|pair| set_extended(backend, &pair));

        match result {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = err;
                log_event(format!(
                    "prepare_hdmi1_layout: attempt={} err={}",
                    attempt + 1,
                    last_error
                ));

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

fn prepare_hdmi2_layout(backend: DisplayBackend) -> Result<(), String> {
    let mut last_error = "Could not find the laptop's internal display.".to_string();
    let mut stable_checks = 0;
    let mut finalized_topology_disable = backend != DisplayBackend::KscreenDoctor;

    for attempt in 0..HDMI2_LAYOUT_ATTEMPTS {
        log_event(format!(
            "prepare_hdmi2_layout: attempt={} backend={backend:?} stable_checks={stable_checks}",
            attempt + 1
        ));
        let result = if attempt == 0 {
            apply_laptop_only_layout(backend)
        } else {
            list_outputs(backend).and_then(|outputs| {
                if has_connected_external_output(&outputs) {
                    stable_checks = 0;
                    log_event(format!(
                        "prepare_hdmi2_layout: external still connected, reapplying layout; outputs=[{}]",
                        describe_outputs(&outputs)
                    ));
                    apply_laptop_only_layout(backend)
                } else if backend == DisplayBackend::KscreenDoctor
                    && !finalized_topology_disable
                    && outputs.len() > 1
                {
                    stable_checks = 0;
                    log_event(format!(
                        "prepare_hdmi2_layout: finalizing KScreenDoctor disable pass; outputs=[{}]",
                        describe_outputs(&outputs)
                    ));
                    match apply_laptop_only_layout(backend) {
                        Ok(()) => {
                            finalized_topology_disable = true;
                            Ok(())
                        }
                        Err(err) => Err(err),
                    }
                } else {
                    stable_checks += 1;
                    Ok(())
                }
            })
        };

        match result {
            Ok(()) if stable_checks >= HDMI2_LAYOUT_STABILITY_CHECKS => {
                log_event("prepare_hdmi2_layout: stabilized successfully");
                return Ok(());
            }
            Ok(()) => {
                if attempt + 1 < HDMI2_LAYOUT_ATTEMPTS {
                    std::thread::sleep(HDMI2_LAYOUT_RETRY_DELAY);
                    continue;
                }
            }
            Err(err) => {
                last_error = err;
                log_event(format!(
                    "prepare_hdmi2_layout: attempt={} err={}",
                    attempt + 1,
                    last_error
                ));

                if attempt + 1 < HDMI2_LAYOUT_ATTEMPTS
                    && is_transient_hdmi2_layout_error(&last_error)
                {
                    std::thread::sleep(HDMI2_LAYOUT_RETRY_DELAY);
                    continue;
                }

                return Err(last_error);
            }
        }
    }

    if stable_checks > 0 {
        Ok(())
    } else {
        Err(last_error)
    }
}

fn switch_to(target: SwitchTarget, input_cache: &InputCache) -> Result<(), String> {
    log_event(format!("switch_to start: target={}", target_label(target)));
    match target {
        SwitchTarget::Hdmi1 => {
            set_input(target.value())?;
            input_cache.store(Some(target.value().into()));
            std::thread::sleep(Duration::from_millis(700));
            let result = prepare_display_layout(target);
            log_event(format!(
                "switch_to done: target={} result={result:?}",
                target_label(target)
            ));
            result
        }
        SwitchTarget::Hdmi2 => {
            set_input(target.value())?;
            input_cache.store(Some(target.value().into()));
            std::thread::sleep(Duration::from_millis(700));
            let result = prepare_display_layout(target);
            log_event(format!(
                "switch_to done: target={} result={result:?}",
                target_label(target)
            ));
            result
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

    fn icon_theme_path(&self) -> String {
        tray_icon_theme_path()
    }

    fn icon_name(&self) -> String {
        APP_ID.into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let current_input = self.current_input();

        [16, 24, 32, 48]
            .into_iter()
            .map(|size| monitor_tray_icon(current_input.as_deref(), size))
            .collect()
    }

    fn title(&self) -> String {
        self.current_input()
            .map(|current| format!("{APP_NAME} ({})", input_label(&current)))
            .unwrap_or_else(|| APP_NAME.into())
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
    log_event(format!(
        "main: starting app, log_file={}",
        log_file_path().display()
    ));
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
            current_mode: None,
        }
    }

    fn active_output(
        name: &str,
        connected: bool,
        internal: bool,
        width: u32,
        height: u32,
    ) -> DisplayOutput {
        DisplayOutput {
            id: name.into(),
            name: name.into(),
            connected,
            internal,
            current_mode: Some((width, height)),
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
    fn laptop_only_layout_requires_internal_display() {
        let outputs = vec![output("HDMI-A-1", true, false)];

        assert!(find_internal_output(&outputs).is_none());
    }

    #[test]
    fn detects_connected_secondary_display() {
        let outputs = vec![output("eDP-1", true, true), output("HDMI-A-1", true, false)];

        let external = find_external_output(&outputs).expect("expected external output");

        assert_eq!(external.name, "HDMI-A-1");
    }

    #[test]
    fn falls_back_to_known_external_output_for_wayland_style_topology() {
        let outputs = vec![
            output("eDP-1", false, true),
            output("HDMI-A-1", false, false),
        ];

        let external =
            find_known_external_output(&outputs).expect("expected known external output");

        assert_eq!(external.name, "HDMI-A-1");
    }

    #[test]
    fn detects_when_connected_external_output_is_present() {
        let outputs = vec![
            active_output("eDP-1", true, true, 1920, 1200),
            active_output("HDMI-A-1", true, false, 2944, 1656),
        ];

        assert!(has_connected_external_output(&outputs));
    }

    #[test]
    fn ignores_disconnected_external_output_in_stability_check() {
        let outputs = vec![
            active_output("eDP-1", true, true, 1920, 1200),
            output("HDMI-A-1", false, false),
        ];

        assert!(!has_connected_external_output(&outputs));
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
        assert_eq!(outputs[0].current_mode, Some((1920, 1080)));
        assert_eq!(outputs[1].current_mode, Some((2560, 1440)));
    }

    #[test]
    fn parses_output_mode_from_xrandr_geometry() {
        assert_eq!(parse_output_mode("1920x1080+0+0"), Some((1920, 1080)));
        assert_eq!(parse_output_mode("2560x1440"), Some((2560, 1440)));
        assert_eq!(parse_output_mode("primary"), None);
    }

    #[test]
    fn prefers_existing_output_mode_when_available() {
        let output = active_output("eDP-1", true, true, 1920, 1200);

        assert_eq!(resolve_output_mode(&output), Some((1920, 1200)));
    }

    #[test]
    fn resolve_output_mode_with_retries_returns_existing_mode_immediately() {
        let output = active_output("HDMI-A-1", true, false, 2944, 1656);

        assert_eq!(
            resolve_output_mode_with_retries(&output),
            Some((2944, 1656))
        );
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
    fn treats_enabled_kscreen_outputs_as_connected() {
        let outputs = parse_kscreen_outputs(
            "Output: 1 eDP-1 enabled priority 1 Panel\nOutput: 2 HDMI-A-1 enabled priority 2\n",
        );

        assert_eq!(outputs.len(), 2);
        assert!(outputs[0].connected);
        assert!(outputs[1].connected);
    }

    #[test]
    fn parses_kscreen_output_lines_with_geometry_and_ansi_sequences() {
        let outputs = parse_kscreen_outputs(
            "\u{1b}[32mOutput:\u{1b}[0m 1 eDP-1 enabled connected 1920x1200+0+0 priority 1 Panel\n",
        );

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "eDP-1");
        assert_eq!(outputs[0].current_mode, Some((1920, 1200)));
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
