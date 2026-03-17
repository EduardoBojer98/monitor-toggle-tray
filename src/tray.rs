use crate::app::{
    APP_ID, APP_NAME, HDMI1, HDMI2, InputCache, QuitSignal, SwitchTarget, autostart_enabled,
    input_label, notify, select_target, set_autostart, tray_icon_theme_path,
};
use crate::icon::monitor_tray_icon;
use ksni::{Icon, Tray, menu};
use std::sync::mpsc::Sender;

pub struct MonitorTray {
    pub quit_signal: QuitSignal,
    pub input_cache: InputCache,
    pub refresh_tx: Sender<()>,
}

impl MonitorTray {
    fn selected_menu_index(current_input: Option<&str>) -> usize {
        match current_input {
            Some(HDMI2) => 1,
            _ => 0,
        }
    }

    fn toggle_autostart(&mut self) {
        let enable = !autostart_enabled();

        match set_autostart(enable) {
            Ok(()) => notify(
                APP_NAME,
                if enable {
                    "Autostart enabled."
                } else {
                    "Autostart disabled."
                },
            ),
            Err(err) => notify(APP_NAME, &format!("Failed to update autostart: {err}")),
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
                checked: autostart_enabled(),
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
