use crate::app::{
    APP_ID, APP_NAME, QuitSignal, SharedState, controlled_monitors, current_snapshot,
    current_status_text, notify, quick_switch, resolve_primary, tray_icon_available,
    tray_icon_name, tray_icon_theme_path,
};
use crate::settings_window::SettingsWindowHandle;
use ksni::{ToolTip, Tray, menu};
use std::sync::mpsc::Sender;

pub struct MonitorTray {
    pub quit_signal: QuitSignal,
    pub shared: SharedState,
    pub refresh_tx: Sender<()>,
    pub settings_window: SettingsWindowHandle,
}

impl MonitorTray {
    fn trigger_quick_switch(&self) {
        if self
            .shared
            .switch_in_progress
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            notify(APP_NAME, "A quick switch is already running.");
            return;
        }

        let shared = self.shared.clone();
        let refresh_tx = self.refresh_tx.clone();

        std::thread::spawn(move || {
            match quick_switch(&shared) {
                Ok(message) => notify(APP_NAME, &message),
                Err(err) => notify(APP_NAME, &format!("Quick switch failed: {err}")),
            }
            refresh_tx.send(()).ok();
        });
    }

    fn current_summary(&self) -> String {
        let snapshot = current_snapshot(&self.shared).ok();
        let Some(snapshot) = snapshot else {
            return "Monitor state unavailable".into();
        };

        let primary = resolve_primary(&snapshot, &self.shared)
            .map(|monitor| monitor.display_name)
            .unwrap_or_else(|| "Unavailable".into());
        let controlled = controlled_monitors(&snapshot, &self.shared);
        let active = controlled.iter().filter(|monitor| monitor.active).count();

        format!(
            "Primary: {primary}\nControlled externals: {} configured, {} active\nStatus: {}",
            controlled.len(),
            active,
            current_status_text(&self.shared)
        )
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
        tray_icon_name().into()
    }

    fn title(&self) -> String {
        APP_NAME.into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: if tray_icon_available() {
                tray_icon_name().into()
            } else {
                String::new()
            },
            title: APP_NAME.into(),
            description: format!("{}\nLeft click runs quick switch.", self.current_summary()),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.trigger_quick_switch();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            menu::StandardItem {
                label: "Quick switch now".into(),
                activate: Box::new(|tray: &mut MonitorTray| tray.trigger_quick_switch()),
                ..Default::default()
            }
            .into(),
            menu::StandardItem {
                label: "Settings".into(),
                activate: Box::new(|tray: &mut MonitorTray| tray.settings_window.open()),
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
