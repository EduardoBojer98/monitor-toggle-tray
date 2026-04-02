use crate::app::{log_event, run_command};
use crate::display::{self, DisplayOutput};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const INPUT_VCP_CODE: &str = "60";
const MONITOR_CACHE_TTL: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default)]
pub struct MonitorSnapshot {
    pub monitors: Vec<MonitorInfo>,
}

#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub id: String,
    pub display_name: String,
    pub output_name: String,
    pub connected: bool,
    pub active: bool,
    pub internal: bool,
    pub position: Option<(i32, i32)>,
    pub current_mode: Option<(u32, u32)>,
    pub ddc: Option<DdcMonitorInfo>,
}

#[derive(Clone, Debug)]
pub struct DdcMonitorInfo {
    pub display_number: u32,
    pub current_input: Option<String>,
    pub supported_inputs: Vec<InputSource>,
    pub capabilities_known: bool,
    pub input_switching_supported: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputSource {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Default)]
pub struct MonitorCache {
    state: Arc<Mutex<CachedSnapshot>>,
}

#[derive(Default)]
struct CachedSnapshot {
    snapshot: Option<MonitorSnapshot>,
    last_refresh: Option<Instant>,
}

impl MonitorCache {
    pub fn get(&self) -> Result<MonitorSnapshot, String> {
        if self.needs_refresh() {
            self.refresh()
        } else {
            self.peek()
                .ok_or_else(|| "No monitor snapshot available.".to_string())
        }
    }

    pub fn refresh(&self) -> Result<MonitorSnapshot, String> {
        let snapshot = discover_monitors()?;
        let mut state = self.state.lock().unwrap();
        state.snapshot = Some(snapshot.clone());
        state.last_refresh = Some(Instant::now());
        Ok(snapshot)
    }

    pub fn invalidate(&self) {
        let mut state = self.state.lock().unwrap();
        state.last_refresh = None;
    }

    fn peek(&self) -> Option<MonitorSnapshot> {
        self.state.lock().unwrap().snapshot.clone()
    }

    fn needs_refresh(&self) -> bool {
        let state = self.state.lock().unwrap();

        match state.last_refresh {
            Some(last_refresh) => last_refresh.elapsed() >= MONITOR_CACHE_TTL,
            None => true,
        }
    }
}

pub fn discover_monitors() -> Result<MonitorSnapshot, String> {
    let (_, outputs) = display::discover_outputs()?;
    let ddc_monitors = discover_ddc_monitors();
    let ddc_index = index_ddc_monitors(&ddc_monitors);
    let output_sysfs = build_output_sysfs_index(&outputs);
    let single_external_match = single_external_match(&outputs, &ddc_monitors);
    let mut monitors = Vec::new();

    for output in outputs {
        let sysfs = output_sysfs.get(&output.name);
        let stable_id = monitor_id_for_output(&output, sysfs);
        let matched_ddc = if output.internal {
            None
        } else {
            match_ddc_monitor(&output, sysfs, &ddc_index).or_else(|| single_external_match.clone())
        };

        let display_name = output_label(&output, sysfs, matched_ddc.as_ref());
        let ddc = matched_ddc.map(|monitor| monitor.to_info());

        monitors.push(MonitorInfo {
            id: stable_id,
            display_name,
            output_name: output.name.clone(),
            connected: output.connected,
            active: output.connected && output.current_mode.is_some(),
            internal: output.internal,
            position: output.position,
            current_mode: output.current_mode,
            ddc,
        });
    }

    monitors.sort_by(|left, right| {
        (left.internal, &left.display_name, &left.output_name)
            .cmp(&(right.internal, &right.display_name, &right.output_name))
            .reverse()
    });

    Ok(MonitorSnapshot { monitors })
}

pub fn set_input_for_monitor(display_number: u32, value: &str) -> Result<(), String> {
    let args = vec![
        "--display".into(),
        display_number.to_string(),
        "setvcp".into(),
        INPUT_VCP_CODE.into(),
        value.into(),
    ];
    run_command("ddcutil", &args).map(|_| ())
}

pub fn input_label(value: &str) -> String {
    match normalize_hex_value(value).as_deref() {
        Some("0x01") => "VGA 1".into(),
        Some("0x02") => "VGA 2".into(),
        Some("0x03") => "DVI 1".into(),
        Some("0x04") => "DVI 2".into(),
        Some("0x05") => "Composite 1".into(),
        Some("0x06") => "Composite 2".into(),
        Some("0x07") => "S-Video 1".into(),
        Some("0x08") => "S-Video 2".into(),
        Some("0x09") => "Tuner 1".into(),
        Some("0x0a") => "Tuner 2".into(),
        Some("0x0b") => "Tuner 3".into(),
        Some("0x0c") => "Component 1".into(),
        Some("0x0d") => "Component 2".into(),
        Some("0x0e") => "Component 3".into(),
        Some("0x0f") => "DisplayPort 1".into(),
        Some("0x10") => "DisplayPort 2".into(),
        Some("0x11") => "HDMI 1".into(),
        Some("0x12") => "HDMI 2".into(),
        Some(value) => format!("Input {}", value.to_uppercase()),
        None => "Unknown input".into(),
    }
}

pub fn fallback_input_choices(current_input: Option<&str>) -> Vec<InputSource> {
    let mut inputs = vec![
        InputSource {
            value: "0x0f".into(),
            label: "DisplayPort 1".into(),
        },
        InputSource {
            value: "0x10".into(),
            label: "DisplayPort 2".into(),
        },
        InputSource {
            value: "0x11".into(),
            label: "HDMI 1".into(),
        },
        InputSource {
            value: "0x12".into(),
            label: "HDMI 2".into(),
        },
    ];

    if let Some(current_input) = current_input.and_then(normalize_hex_value) {
        if !inputs.iter().any(|input| input.value == current_input) {
            inputs.push(InputSource {
                label: input_label(&current_input),
                value: current_input,
            });
        }
    }

    inputs
}

fn discover_ddc_monitors() -> Vec<DiscoveredDdcMonitor> {
    let args = vec!["detect".into(), "--brief".into()];
    let output = match run_command("ddcutil", &args) {
        Ok(output) => output,
        Err(err) => {
            log_event(format!("discover_ddc_monitors: detect failed: {err}"));
            return Vec::new();
        }
    };

    parse_ddc_detect(&output)
        .into_iter()
        .map(enrich_ddc_monitor)
        .collect()
}

fn enrich_ddc_monitor(mut monitor: DiscoveredDdcMonitor) -> DiscoveredDdcMonitor {
    let current_args = vec![
        "--display".into(),
        monitor.display_number.to_string(),
        "getvcp".into(),
        INPUT_VCP_CODE.into(),
        "--brief".into(),
    ];

    monitor.current_input = run_command("ddcutil", &current_args)
        .ok()
        .and_then(|text| parse_getvcp_input(&text));
    monitor.input_switching_supported = monitor.current_input.is_some();

    let capabilities_args = vec![
        "--display".into(),
        monitor.display_number.to_string(),
        "capabilities".into(),
    ];

    if let Ok(text) = run_command("ddcutil", &capabilities_args) {
        let inputs = parse_capabilities_inputs(&text);
        if !inputs.is_empty() {
            monitor.capabilities_known = true;
            monitor.supported_inputs = dedupe_inputs(inputs);
        }
    }

    if monitor.supported_inputs.is_empty() {
        monitor.supported_inputs = fallback_input_choices(monitor.current_input.as_deref());
    }

    monitor
}

fn parse_getvcp_input(output: &str) -> Option<String> {
    for marker in ["current value = ", "sl=", "SNC x", "SNC X"] {
        if let Some(start) = output.find(marker) {
            let value = output[start + marker.len()..]
                .chars()
                .skip_while(|ch| ch.is_whitespace())
                .take_while(|ch| ch.is_ascii_hexdigit() || *ch == 'x' || *ch == 'X')
                .collect::<String>();
            if let Some(normalized) = normalize_hex_value(&value) {
                return Some(normalized);
            }
        }
    }

    None
}

fn parse_capabilities_inputs(output: &str) -> Vec<InputSource> {
    let mut in_feature = false;
    let mut in_values = false;
    let mut inputs = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Feature: 60") || trimmed.starts_with("Feature: 0x60") {
            in_feature = true;
            in_values = false;
            continue;
        }

        if in_feature && trimmed.starts_with("Feature:") {
            break;
        }

        if !in_feature {
            continue;
        }

        if trimmed.starts_with("Values") {
            in_values = true;
            continue;
        }

        if !in_values {
            continue;
        }

        let Some((raw_value, raw_label)) = trimmed.split_once(':') else {
            continue;
        };
        let Some(value) = normalize_hex_value(raw_value.trim()) else {
            continue;
        };
        let label = raw_label.trim();
        if label.is_empty() {
            continue;
        }

        inputs.push(InputSource {
            value,
            label: label.replace('-', " "),
        });
    }

    inputs
}

fn parse_ddc_detect(output: &str) -> Vec<DiscoveredDdcMonitor> {
    let mut monitors = Vec::new();
    let mut current: Option<DiscoveredDdcMonitor> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some(monitor) = current.take() {
                monitors.push(monitor);
            }
            continue;
        }

        if let Some(number) = trimmed
            .strip_prefix("Display ")
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            if let Some(monitor) = current.take() {
                monitors.push(monitor);
            }
            current = Some(DiscoveredDdcMonitor {
                display_number: number,
                ..Default::default()
            });
            continue;
        }

        let Some(monitor) = current.as_mut() else {
            continue;
        };

        if let Some(bus) = trimmed.strip_prefix("I2C bus:") {
            monitor.bus = Some(bus.trim().to_string());
        } else if let Some(summary) = trimmed.strip_prefix("Monitor:") {
            let parts = summary.trim().split(':').collect::<Vec<_>>();
            monitor.manufacturer = parts.first().copied().unwrap_or_default().trim().into();
            monitor.model = parts.get(1).copied().unwrap_or_default().trim().into();
            monitor.serial = parts.get(2).copied().unwrap_or_default().trim().into();
        }
    }

    if let Some(monitor) = current {
        monitors.push(monitor);
    }

    monitors
}

fn dedupe_inputs(inputs: Vec<InputSource>) -> Vec<InputSource> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();

    for input in inputs {
        if seen.insert(input.value.clone()) {
            deduped.push(input);
        }
    }

    deduped
}

fn single_external_match(
    outputs: &[DisplayOutput],
    ddc_monitors: &[DiscoveredDdcMonitor],
) -> Option<DiscoveredDdcMonitor> {
    let external_outputs = outputs
        .iter()
        .filter(|output| output.connected && !output.internal)
        .count();

    if external_outputs == 1 && ddc_monitors.len() == 1 {
        ddc_monitors.first().cloned()
    } else {
        None
    }
}

fn index_ddc_monitors(
    monitors: &[DiscoveredDdcMonitor],
) -> BTreeMap<String, Vec<DiscoveredDdcMonitor>> {
    let mut index = BTreeMap::<String, Vec<DiscoveredDdcMonitor>>::new();

    for monitor in monitors {
        for key in ddc_match_keys(monitor) {
            index.entry(key).or_default().push(monitor.clone());
        }
    }

    index
}

fn ddc_match_keys(monitor: &DiscoveredDdcMonitor) -> Vec<String> {
    let mut keys = Vec::new();
    let manufacturer = normalize_key_part(&monitor.manufacturer);
    let model = normalize_key_part(&monitor.model);
    let serial = normalize_key_part(&monitor.serial);

    if !manufacturer.is_empty() && !model.is_empty() && !serial.is_empty() {
        keys.push(format!("{manufacturer}:{model}:{serial}"));
    }
    if !manufacturer.is_empty() && !model.is_empty() {
        keys.push(format!("{manufacturer}:{model}"));
    }

    keys
}

fn match_ddc_monitor(
    output: &DisplayOutput,
    sysfs: Option<&OutputSysfsInfo>,
    index: &BTreeMap<String, Vec<DiscoveredDdcMonitor>>,
) -> Option<DiscoveredDdcMonitor> {
    let Some(sysfs) = sysfs else {
        return None;
    };

    let mut keys = Vec::new();
    if let (Some(manufacturer), Some(model), Some(serial)) =
        (&sysfs.manufacturer, &sysfs.model, &sysfs.serial)
    {
        keys.push(format!(
            "{}:{}:{}",
            normalize_key_part(manufacturer),
            normalize_key_part(model),
            normalize_key_part(serial)
        ));
    }
    if let (Some(manufacturer), Some(model)) = (&sysfs.manufacturer, &sysfs.model) {
        keys.push(format!(
            "{}:{}",
            normalize_key_part(manufacturer),
            normalize_key_part(model)
        ));
    }

    for key in keys {
        let Some(matches) = index.get(&key) else {
            continue;
        };
        if matches.len() == 1 {
            return matches.first().cloned();
        }

        if let Some(bus_match) = matches.iter().find(|monitor| {
            monitor
                .bus
                .as_ref()
                .is_some_and(|bus| bus.ends_with(&output.name))
        }) {
            return Some(bus_match.clone());
        }
    }

    None
}

fn build_output_sysfs_index(outputs: &[DisplayOutput]) -> BTreeMap<String, OutputSysfsInfo> {
    let mut index = BTreeMap::new();

    for output in outputs {
        if let Some(info) = find_output_sysfs_info(&output.name) {
            index.insert(output.name.clone(), info);
        }
    }

    index
}

fn find_output_sysfs_info(output_name: &str) -> Option<OutputSysfsInfo> {
    let drm_root = Path::new("/sys/class/drm");
    let entries = fs::read_dir(drm_root).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path.file_name()?.to_string_lossy();

        if !file_name.ends_with(output_name) {
            continue;
        }

        let edid_path = path.join("edid");
        let edid = fs::read(&edid_path).ok()?;
        return Some(parse_output_sysfs_info(&edid));
    }

    None
}

fn parse_output_sysfs_info(edid: &[u8]) -> OutputSysfsInfo {
    let manufacturer = parse_edid_manufacturer(edid);
    let model = parse_edid_text_descriptor(edid, 0xfc);
    let serial = parse_edid_text_descriptor(edid, 0xff)
        .or_else(|| parse_edid_numeric_serial(edid).map(|value| value.to_string()));

    OutputSysfsInfo {
        manufacturer,
        model,
        serial,
    }
}

fn parse_edid_manufacturer(edid: &[u8]) -> Option<String> {
    if edid.len() < 10 {
        return None;
    }

    let raw = u16::from_be_bytes([edid[8], edid[9]]);
    let first = (((raw >> 10) & 0x1f) as u8 + b'A' - 1) as char;
    let second = (((raw >> 5) & 0x1f) as u8 + b'A' - 1) as char;
    let third = ((raw & 0x1f) as u8 + b'A' - 1) as char;

    Some(format!("{first}{second}{third}"))
}

fn parse_edid_numeric_serial(edid: &[u8]) -> Option<u32> {
    if edid.len() < 16 {
        return None;
    }

    let value = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
    if value == 0 { None } else { Some(value) }
}

fn parse_edid_text_descriptor(edid: &[u8], descriptor_type: u8) -> Option<String> {
    if edid.len() < 126 {
        return None;
    }

    for chunk in edid[54..126].chunks_exact(18) {
        if chunk[0] == 0x00 && chunk[1] == 0x00 && chunk[2] == 0x00 && chunk[3] == descriptor_type {
            let text = String::from_utf8_lossy(&chunk[5..18])
                .trim_matches(char::from(0))
                .trim()
                .trim_end_matches('\n')
                .to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }

    None
}

fn monitor_id_for_output(output: &DisplayOutput, sysfs: Option<&OutputSysfsInfo>) -> String {
    if output.internal {
        return format!("internal:{}", output.name);
    }

    if let Some(sysfs) = sysfs {
        if let (Some(manufacturer), Some(model), Some(serial)) =
            (&sysfs.manufacturer, &sysfs.model, &sysfs.serial)
        {
            return format!(
                "{}:{}:{}",
                normalize_key_part(manufacturer),
                normalize_key_part(model),
                normalize_key_part(serial)
            );
        }
    }

    format!("output:{}", output.name)
}

fn output_label(
    output: &DisplayOutput,
    sysfs: Option<&OutputSysfsInfo>,
    ddc: Option<&DiscoveredDdcMonitor>,
) -> String {
    if output.internal {
        return format!("Built-in display ({})", output.name);
    }

    if let Some(sysfs) = sysfs {
        if let Some(model) = &sysfs.model {
            return format!("{model} ({})", output.name);
        }
    }

    if let Some(ddc) = ddc {
        if !ddc.model.is_empty() {
            return format!("{} ({})", ddc.model, output.name);
        }
    }

    output.name.clone()
}

fn normalize_hex_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed.trim_start_matches("0x").trim_start_matches("0X");
    if normalized.is_empty() || !normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }

    Some(format!(
        "0x{:02x}",
        u8::from_str_radix(normalized, 16).ok()?
    ))
}

fn normalize_key_part(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[derive(Clone, Debug, Default)]
struct DiscoveredDdcMonitor {
    display_number: u32,
    bus: Option<String>,
    manufacturer: String,
    model: String,
    serial: String,
    current_input: Option<String>,
    supported_inputs: Vec<InputSource>,
    capabilities_known: bool,
    input_switching_supported: bool,
}

impl DiscoveredDdcMonitor {
    fn to_info(&self) -> DdcMonitorInfo {
        DdcMonitorInfo {
            display_number: self.display_number,
            current_input: self.current_input.clone(),
            supported_inputs: self.supported_inputs.clone(),
            capabilities_known: self.capabilities_known,
            input_switching_supported: self.input_switching_supported,
        }
    }
}

#[derive(Clone, Debug)]
struct OutputSysfsInfo {
    manufacturer: Option<String>,
    model: Option<String>,
    serial: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_brief_detect_output() {
        let parsed = parse_ddc_detect(
            "Display 1\n   I2C bus:             /dev/i2c-7\n   Monitor:             DEL:DELL U2720Q:ABC123\n",
        );

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].display_number, 1);
        assert_eq!(parsed[0].manufacturer, "DEL");
        assert_eq!(parsed[0].model, "DELL U2720Q");
        assert_eq!(parsed[0].serial, "ABC123");
    }

    #[test]
    fn parses_getvcp_input_from_brief_output() {
        assert_eq!(parse_getvcp_input("VCP 60 SNC x11"), Some("0x11".into()));
        assert_eq!(
            parse_getvcp_input("VCP code 0x60 (Input Source): HDMI-1 (sl=0x11)"),
            Some("0x11".into())
        );
    }

    #[test]
    fn parses_capabilities_values() {
        let inputs = parse_capabilities_inputs(
            "Feature: 60 (Input Source)\n   Values (  parsed):\n      0f: DisplayPort-1\n      11: HDMI-1\n      12: HDMI-2\nFeature: 62 (Audio speaker volume)\n",
        );

        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].value, "0x0f");
        assert_eq!(inputs[1].label, "HDMI 1");
    }
}
