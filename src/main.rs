use ksni::{Tray, TrayService};
use notify_rust::Notification;
use std::process::Command;

const HDMI1: &str = "0x11";
const HDMI2: &str = "0x12";

fn get_input() -> Option<String> {
    let output = Command::new("ddcutil")
        .args(["getvcp", "60"])
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);

    for part in text.split_whitespace() {
        if part.contains("sl=0x") {
            return Some(part.replace("sl=", "").replace(")", ""));
        }
    }

    None
}

fn set_input(val: &str) {
    Command::new("ddcutil")
        .args(["setvcp", "60", val])
        .output()
        .ok();

    Notification::new()
        .summary("Monitor Input")
        .body(&format!("Switched to {}", val))
        .show()
        .ok();
}

struct MonitorTray;

impl Tray for MonitorTray {
    fn icon_name(&self) -> String {
        match get_input().as_deref() {
            Some(HDMI1) => "video-display".into(),
            Some(HDMI2) => "video-television".into(),
            _ => "computer".into(),
        }
    }

    fn title(&self) -> String {
        "Monitor Toggle".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        match get_input().as_deref() {
            Some(HDMI1) => set_input(HDMI2),
            _ => set_input(HDMI1),
        }
    }
}

fn main() {
    let tray = MonitorTray;
    let service = TrayService::new(tray);
    service.spawn();
    loop {
        std::thread::park();
    }
}