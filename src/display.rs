use crate::app::{log_event, run_command, strip_ansi_escape_sequences};
use std::collections::BTreeMap;
use std::env;
use std::time::Duration;

const MODE_RESOLVE_ATTEMPTS: usize = 5;
const MODE_RESOLVE_RETRY_DELAY: Duration = Duration::from_millis(250);
const OUTPUT_REAPPEAR_ATTEMPTS: usize = 12;
const OUTPUT_REAPPEAR_RETRY_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayBackend {
    KscreenDoctor,
    Xrandr,
}

#[derive(Clone, Debug)]
pub struct DisplayOutput {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub internal: bool,
    pub current_mode: Option<(u32, u32)>,
    pub position: Option<(i32, i32)>,
    pub scale: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct OutputLayout {
    pub name: String,
    pub position: Option<(i32, i32)>,
    pub size: Option<(u32, u32)>,
}

pub fn discover_outputs() -> Result<(DisplayBackend, Vec<DisplayOutput>), String> {
    let backends = available_display_backends();
    let mut last_error = "No supported display management tool was found.".to_string();
    let mut discovered = Vec::new();

    for backend in backends {
        match list_outputs(backend) {
            Ok(outputs) if !outputs.is_empty() => discovered.push((backend, outputs)),
            Ok(_) => last_error = "No display outputs were detected.".into(),
            Err(err) => {
                log_event(format!(
                    "discover_outputs: backend={backend:?} failed: {err}"
                ));
                last_error = err;
            }
        }
    }

    if let Some((preferred_backend, preferred_outputs)) = discovered.first().cloned() {
        let merged_outputs = discovered
            .iter()
            .find(|(backend, _)| *backend == DisplayBackend::Xrandr)
            .map(|(_, outputs)| merge_output_data(preferred_outputs.clone(), outputs))
            .unwrap_or(preferred_outputs);

        log_event(format!(
            "discover_outputs: selected backend={preferred_backend:?} outputs=[{}]",
            describe_outputs(&merged_outputs)
        ));
        return Ok((preferred_backend, merged_outputs));
    }

    Err(last_error)
}

pub fn disable_outputs(primary_output: &str, outputs_to_disable: &[String]) -> Result<(), String> {
    let (backend, outputs) = discover_outputs()?;
    let primary = find_output(&outputs, primary_output)
        .ok_or_else(|| format!("Primary display {primary_output} is no longer available."))?;
    let solo_primary_position = primary_only_position(&primary);

    let targets = outputs
        .iter()
        .filter(|output| outputs_to_disable.iter().any(|name| name == &output.name))
        .cloned()
        .collect::<Vec<_>>();

    log_event(format!(
        "disable_outputs: backend={backend:?} primary={} targets={}",
        primary.name,
        targets
            .iter()
            .map(|output| output.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    match backend {
        DisplayBackend::KscreenDoctor => {
            let mut args = vec![
                format!("output.{}.enable", primary.id),
                format!("output.{}.primary", primary.id),
                format!(
                    "output.{}.position.{},{}",
                    primary.id, solo_primary_position.0, solo_primary_position.1
                ),
            ];

            for output in targets.iter().filter(|output| output.name != primary.name) {
                args.push(format!("output.{}.disable", output.id));
            }

            run_command("kscreen-doctor", &args).map(|_| ())
        }
        DisplayBackend::Xrandr => {
            let mut args = vec![
                "--output".into(),
                primary.name.clone(),
                "--auto".into(),
                "--primary".into(),
                "--pos".into(),
                format!("{}x{}", solo_primary_position.0, solo_primary_position.1),
            ];

            for output in targets.iter().filter(|output| output.name != primary.name) {
                args.push("--output".into());
                args.push(output.name.clone());
                args.push("--off".into());
            }

            run_command("xrandr", &args).map(|_| ())
        }
    }
}

fn primary_only_position(primary: &DisplayOutput) -> (i32, i32) {
    let _ = primary;
    (0, 0)
}

pub fn enable_outputs(
    primary_output: &str,
    primary_position: Option<(i32, i32)>,
    outputs_to_enable: &[OutputLayout],
) -> Result<(), String> {
    let (backend, outputs) = discover_outputs()?;
    let primary = find_output(&outputs, primary_output)
        .ok_or_else(|| format!("Primary display {primary_output} is no longer available."))?;
    let requested_targets = outputs_to_enable
        .iter()
        .filter(|layout| layout.name != primary_output)
        .map(|layout| layout.name.clone())
        .collect::<Vec<_>>();
    let mut ordered_targets = resolve_enable_targets(primary_output, &requested_targets)?;
    let positions_by_name = outputs_to_enable
        .iter()
        .map(|layout| (layout.name.clone(), layout.position))
        .collect::<BTreeMap<_, _>>();
    let sizes_by_name = outputs_to_enable
        .iter()
        .map(|layout| (layout.name.clone(), layout.size))
        .collect::<BTreeMap<_, _>>();

    for target in &mut ordered_targets {
        if let Some(position) = positions_by_name.get(&target.name).copied().flatten() {
            target.position = Some(position);
        }
        if let Some(size) = sizes_by_name.get(&target.name).copied().flatten() {
            target.current_mode = Some(size);
        }
    }

    let adjusted_primary_position =
        normalize_stacked_primary_position(primary_output, primary_position, &ordered_targets);

    ordered_targets.sort_by(|left, right| left.name.cmp(&right.name));

    log_event(format!(
        "enable_outputs: backend={backend:?} primary={} targets={}",
        primary.name,
        ordered_targets
            .iter()
            .map(|output| output.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    match backend {
        DisplayBackend::KscreenDoctor => {
            enable_outputs_kscreen_doctor(&primary, adjusted_primary_position, ordered_targets)
        }
        DisplayBackend::Xrandr => {
            enable_outputs_xrandr(&primary, adjusted_primary_position, ordered_targets)
        }
    }
}

fn normalize_stacked_primary_position(
    primary_output: &str,
    primary_position: Option<(i32, i32)>,
    outputs_to_enable: &[DisplayOutput],
) -> Option<(i32, i32)> {
    let Some((primary_x, primary_y)) = primary_position else {
        return primary_position;
    };

    let topmost = outputs_to_enable
        .iter()
        .filter_map(|output| {
            output
                .position
                .zip(output.current_mode)
                .map(|((x, y), (width, height))| (output.name.as_str(), x, y, width, height))
        })
        .min_by_key(|(_, _, y, _, _)| *y);

    let Some((top_name, top_x, top_y, top_width, top_height)) = topmost else {
        return primary_position;
    };

    if primary_output == top_name || primary_y <= top_y {
        return primary_position;
    }

    let touches_vertically = (primary_y - top_y).abs() <= (top_height as i32 + 400);
    let overlaps_horizontally = primary_x < top_x + top_width as i32;

    if !touches_vertically || !overlaps_horizontally {
        return primary_position;
    }

    let adjusted_y = top_y + top_height as i32;
    Some((primary_x, adjusted_y))
}

fn resolve_enable_targets(
    primary_output: &str,
    requested_targets: &[String],
) -> Result<Vec<DisplayOutput>, String> {
    if requested_targets.is_empty() {
        return Ok(Vec::new());
    }

    let mut last_seen = Vec::new();

    for attempt in 0..OUTPUT_REAPPEAR_ATTEMPTS {
        let (_, outputs) = discover_outputs()?;
        let primary_available = outputs.iter().any(|output| output.name == primary_output);
        let resolved_targets = requested_targets
            .iter()
            .filter_map(|name| find_output(&outputs, name))
            .collect::<Vec<_>>();

        last_seen = outputs;

        if primary_available && resolved_targets.len() == requested_targets.len() {
            log_event(format!(
                "resolve_enable_targets: all targets available after attempt={} targets={}",
                attempt + 1,
                resolved_targets
                    .iter()
                    .map(|output| format!("{}(connected={})", output.name, output.connected))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            return Ok(resolved_targets);
        }

        if attempt + 1 < OUTPUT_REAPPEAR_ATTEMPTS {
            std::thread::sleep(OUTPUT_REAPPEAR_RETRY_DELAY);
        }
    }

    let missing = requested_targets
        .iter()
        .filter(|name| !last_seen.iter().any(|output| &output.name == *name))
        .cloned()
        .collect::<Vec<_>>();

    log_event(format!(
        "resolve_enable_targets: missing targets after retries requested={} seen=[{}]",
        requested_targets.join(", "),
        describe_outputs(&last_seen)
    ));

    Err(if missing.is_empty() {
        "Requested outputs never became available for re-enabling.".into()
    } else {
        format!(
            "Requested outputs did not reappear in time: {}",
            missing.join(", ")
        )
    })
}

fn enable_outputs_kscreen_doctor(
    primary: &DisplayOutput,
    primary_position: Option<(i32, i32)>,
    outputs_to_enable: Vec<DisplayOutput>,
) -> Result<(), String> {
    let mut cursor_x = current_desktop_right_edge()?.max(0);
    let mut args = Vec::new();
    for output in outputs_to_enable {
        let position = output.position.unwrap_or((cursor_x, 0));
        args.push(format!("output.{}.enable", output.id));
        args.push(format!(
            "output.{}.position.{},{}",
            output.id, position.0, position.1
        ));

        if output.position.is_none() {
            if let Some(width) = resolve_output_mode_width(&output.name)? {
                cursor_x += width as i32;
            }
        }
    }

    args.push(format!("output.{}.enable", primary.id));
    args.push(format!("output.{}.primary", primary.id));
    if let Some((x, y)) = primary_position {
        args.push(format!("output.{}.position.{x},{y}", primary.id));
    }

    run_command("kscreen-doctor", &args)?;

    Ok(())
}

fn enable_outputs_xrandr(
    primary: &DisplayOutput,
    primary_position: Option<(i32, i32)>,
    outputs_to_enable: Vec<DisplayOutput>,
) -> Result<(), String> {
    let mut primary_args = vec![
        "--output".into(),
        primary.name.clone(),
        "--auto".into(),
        "--primary".into(),
    ];
    if let Some((x, y)) = primary_position {
        primary_args.push("--pos".into());
        primary_args.push(format!("{x}x{y}"));
    }
    run_command("xrandr", &primary_args)?;

    let mut cursor_x = current_desktop_right_edge()?.max(0);

    for output in outputs_to_enable {
        let position = output.position.unwrap_or((cursor_x, 0));
        let args = vec![
            "--output".into(),
            output.name.clone(),
            "--auto".into(),
            "--pos".into(),
            format!("{}x{}", position.0, position.1),
        ];
        run_command("xrandr", &args)?;

        if output.position.is_none() {
            if let Some(width) = resolve_output_mode_width(&output.name)? {
                cursor_x += width as i32;
            }
        }
    }

    Ok(())
}

fn current_desktop_right_edge() -> Result<i32, String> {
    let (_, outputs) = discover_outputs()?;
    let right_edge = outputs
        .iter()
        .filter(|output| output.connected && output.current_mode.is_some())
        .map(|output| {
            let x = output.position.map(|(x, _)| x).unwrap_or(0);
            let width = output
                .current_mode
                .map(|(width, _)| width as i32)
                .unwrap_or(0);
            x + width
        })
        .max()
        .unwrap_or(0);

    Ok(right_edge)
}

fn find_output(outputs: &[DisplayOutput], output_name: &str) -> Option<DisplayOutput> {
    outputs
        .iter()
        .find(|output| output.name == output_name)
        .cloned()
}

fn resolve_output_mode_width(output_name: &str) -> Result<Option<u32>, String> {
    for attempt in 0..MODE_RESOLVE_ATTEMPTS {
        let (_, outputs) = discover_outputs()?;
        if let Some(width) = outputs
            .iter()
            .find(|output| output.name == output_name)
            .and_then(|output| output.current_mode.map(|(width, _)| width))
        {
            return Ok(Some(width));
        }

        if attempt + 1 < MODE_RESOLVE_ATTEMPTS {
            std::thread::sleep(MODE_RESOLVE_RETRY_DELAY);
        }
    }

    Ok(None)
}

fn merge_output_data(
    preferred_outputs: Vec<DisplayOutput>,
    fallback_outputs: &[DisplayOutput],
) -> Vec<DisplayOutput> {
    let mut merged = preferred_outputs;
    let fallback_by_name = fallback_outputs
        .iter()
        .map(|output| (output.name.clone(), output.clone()))
        .collect::<BTreeMap<_, _>>();

    for output in &mut merged {
        let Some(fallback) = fallback_by_name.get(&output.name) else {
            continue;
        };

        output.connected |= fallback.connected;
        output.internal |= fallback.internal;
        if output.current_mode.is_none() {
            output.current_mode = fallback.current_mode;
        }
        if output.position.is_none() {
            output.position = fallback.position;
        }
    }

    for fallback in fallback_outputs {
        if merged.iter().any(|output| output.name == fallback.name) {
            continue;
        }

        if fallback.connected || fallback.internal {
            merged.push(fallback.clone());
        }
    }

    merged
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
                "{}(id={}, connected={}, internal={}, mode={}, scale={})",
                output.name,
                output.id,
                output.connected,
                output.internal,
                output
                    .current_mode
                    .map(|(width, height)| format!("{width}x{height}"))
                    .unwrap_or_else(|| "none".into()),
                output
                    .scale
                    .map(|scale| format!("{scale:.2}"))
                    .unwrap_or_else(|| "n/a".into())
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

fn parse_geometry(token: &str) -> Option<((u32, u32), (i32, i32))> {
    let (geometry, position) = token.split_once('+')?;
    let (width, height) = geometry.split_once('x')?;
    let (x, y) = position.split_once('+')?;

    Some((
        (width.parse().ok()?, height.parse().ok()?),
        (x.parse().ok()?, y.parse().ok()?),
    ))
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
            let geometry = parts.iter().find_map(|part| parse_geometry(part));
            let scale = parse_kscreen_scale(trimmed);
            let connected = if trimmed.contains(" disconnected") {
                false
            } else if trimmed.contains(" connected") || trimmed.contains(" enabled") {
                true
            } else {
                geometry.is_some()
            };
            let current_mode = geometry.map(|(mode, _)| mode);
            let position = geometry.map(|(_, position)| position);
            let normalized_mode = normalize_geometry_mode(current_mode, scale);
            let normalized_position = normalize_geometry_position(position, scale);

            Some(DisplayOutput {
                id,
                internal: is_internal_output(&name) || has_internal_marker(trimmed),
                connected,
                name,
                current_mode: normalized_mode,
                position: normalized_position,
                scale,
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

            let parts = trimmed.split_whitespace().collect::<Vec<_>>();
            let name = parts.first()?.to_string();
            let connected = parts.get(1).copied() == Some("connected");
            let geometry = parts.iter().find_map(|part| parse_geometry(part));

            Some(DisplayOutput {
                id: name.clone(),
                internal: is_internal_output(&name) || has_internal_marker(trimmed),
                connected,
                name,
                current_mode: geometry.map(|(mode, _)| mode),
                position: geometry.map(|(_, position)| position),
                scale: None,
            })
        })
        .collect()
}

fn parse_kscreen_scale(line: &str) -> Option<f32> {
    let (prefix, suffix) = line.split_once("Scale:")?;
    let _ = prefix;
    suffix
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<f32>().ok())
}

fn normalize_geometry_mode(mode: Option<(u32, u32)>, scale: Option<f32>) -> Option<(u32, u32)> {
    let (width, height) = mode?;
    let scale = scale?;

    if (scale - 1.0).abs() < f32::EPSILON {
        return Some((width, height));
    }

    Some((
        ((width as f32) / scale).round() as u32,
        ((height as f32) / scale).round() as u32,
    ))
}

fn normalize_geometry_position(
    position: Option<(i32, i32)>,
    scale: Option<f32>,
) -> Option<(i32, i32)> {
    let (x, y) = position?;
    let scale = scale?;

    if (scale - 1.0).abs() < f32::EPSILON {
        return Some((x, y));
    }

    Some((
        ((x as f32) / scale).round() as i32,
        ((y as f32) / scale).round() as i32,
    ))
}

fn list_outputs(backend: DisplayBackend) -> Result<Vec<DisplayOutput>, String> {
    let outputs = match backend {
        DisplayBackend::KscreenDoctor => {
            let args = vec!["-o".into()];
            let raw_output = run_command("kscreen-doctor", &args)?;
            let outputs = parse_kscreen_outputs(&raw_output);

            if outputs.is_empty() && !raw_output.trim().is_empty() {
                return Err("Unable to parse kscreen-doctor output.".into());
            }

            outputs
        }
        DisplayBackend::Xrandr => {
            let args = vec!["--query".into()];
            parse_xrandr_outputs(&run_command("xrandr", &args)?)
        }
    };

    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xrandr_geometry_and_position() {
        let outputs = parse_xrandr_outputs(
            "eDP-1 connected primary 1920x1080+0+1080\nHDMI-A-1 connected 2560x1440+0+0\n",
        );

        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].current_mode, Some((1920, 1080)));
        assert_eq!(outputs[0].position, Some((0, 1080)));
        assert_eq!(outputs[1].position, Some((0, 0)));
    }

    #[test]
    fn merges_fallback_connection_state_and_modes() {
        let preferred = vec![
            DisplayOutput {
                id: "1".into(),
                name: "eDP-1".into(),
                connected: false,
                internal: true,
                current_mode: None,
                position: None,
                scale: None,
            },
            DisplayOutput {
                id: "2".into(),
                name: "HDMI-A-1".into(),
                connected: false,
                internal: false,
                current_mode: None,
                position: None,
                scale: None,
            },
        ];
        let fallback = vec![
            DisplayOutput {
                id: "eDP-1".into(),
                name: "eDP-1".into(),
                connected: true,
                internal: true,
                current_mode: Some((1920, 1200)),
                position: Some((0, 0)),
                scale: None,
            },
            DisplayOutput {
                id: "HDMI-A-1".into(),
                name: "HDMI-A-1".into(),
                connected: true,
                internal: false,
                current_mode: Some((2944, 1656)),
                position: Some((0, 0)),
                scale: None,
            },
        ];

        let merged = merge_output_data(preferred, &fallback);

        assert!(merged.iter().any(|output| {
            output.name == "eDP-1" && output.connected && output.current_mode == Some((1920, 1200))
        }));
        assert!(merged.iter().any(|output| {
            output.name == "HDMI-A-1"
                && output.connected
                && output.current_mode == Some((2944, 1656))
        }));
    }

    #[test]
    fn parses_kscreen_output_with_scale() {
        let outputs =
            parse_kscreen_outputs("Output: 1 eDP-1 connected enabled 3840x2400+0+0 Scale: 2\n");

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].current_mode, Some((1920, 1200)));
        assert_eq!(outputs[0].position, Some((0, 0)));
        assert_eq!(outputs[0].scale, Some(2.0));
    }

    #[test]
    fn resets_primary_to_origin_for_primary_only_layout() {
        let primary = DisplayOutput {
            id: "1".into(),
            name: "eDP-1".into(),
            connected: true,
            internal: true,
            current_mode: Some((1920, 1080)),
            position: Some((0, 1080)),
            scale: None,
        };

        assert_eq!(primary_only_position(&primary), (0, 0));
    }
}
