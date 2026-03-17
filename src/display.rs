use crate::app::{SwitchTarget, log_event, run_command, strip_ansi_escape_sequences, target_label};
use std::env;
use std::time::Duration;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayBackend {
    KscreenDoctor,
    Xrandr,
}

#[derive(Clone)]
pub struct DisplayOutput {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub internal: bool,
    pub current_mode: Option<(u32, u32)>,
}

#[derive(Clone)]
struct DisplayPair {
    anchor: DisplayOutput,
    external: DisplayOutput,
}

pub fn prepare_display_layout(target: SwitchTarget) -> Result<(), String> {
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
            // Some sessions expose the laptop panel with an odd connector name.
            // If there is exactly one connected output that does not look external,
            // treat it as the internal panel.
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

            // Activate the second output below the laptop first so the desktop
            // expands cleanly before we place it in its final position.
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
                    // A few consecutive stable reads help avoid reporting success
                    // while the desktop stack is still removing the external output.
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
}
