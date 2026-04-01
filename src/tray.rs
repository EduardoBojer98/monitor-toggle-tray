use crate::app::{
    APP_ID, APP_NAME, QuitSignal, SharedState, autostart_enabled, capture_current_input_as_laptop,
    controlled_monitors, current_snapshot, notify, quick_switch, refresh_snapshot, resolve_primary,
    save_current_layout, set_autostart, set_laptop_input, set_primary_monitor, set_toggle_input,
    toggle_controlled_monitor, tray_icon_available, tray_icon_name, tray_icon_theme_path,
};
use crate::monitor::{InputSource, fallback_input_choices, input_label};
use ksni::{ToolTip, Tray, menu};
use std::sync::mpsc::Sender;

pub struct MonitorTray {
    pub quit_signal: QuitSignal,
    pub shared: SharedState,
    pub refresh_tx: Sender<()>,
}

impl MonitorTray {
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

    fn request_refresh(&self) {
        self.refresh_tx.send(()).ok();
    }

    fn trigger_quick_switch(&self) {
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
            "Primary: {primary}\nControlled externals: {} configured, {} active",
            controlled.len(),
            active
        )
    }

    fn monitor_submenu(
        &self,
        monitor: crate::monitor::MonitorInfo,
    ) -> ksni::MenuItem<Self> {
        let monitor_label = monitor.display_name.clone();
        let config = self.shared.config_store.current();
        let settings = config.settings(&monitor.id).cloned();
        let snapshot = current_snapshot(&self.shared).ok();
        let resolved_primary_id = snapshot
            .as_ref()
            .and_then(|snapshot| resolve_primary(snapshot, &self.shared))
            .map(|monitor| monitor.id);
        let is_primary = resolved_primary_id.as_deref() == Some(monitor.id.as_str());

        let mut submenu = vec![
            menu::StandardItem {
                label: if monitor.connected {
                    format!(
                        "Status: {}{}",
                        if monitor.active { "active" } else { "connected" },
                        if monitor.internal { ", internal" } else { "" }
                    )
                } else {
                    "Status: disconnected".into()
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
            menu::CheckmarkItem {
                label: "Primary display".into(),
                checked: is_primary,
                activate: Box::new({
                    let monitor_id = monitor.id.clone();
                    move |tray: &mut MonitorTray| {
                        match set_primary_monitor(&tray.shared, &monitor_id) {
                            Ok(()) => notify(APP_NAME, "Primary display updated."),
                            Err(err) => notify(APP_NAME, &format!("Failed to update primary: {err}")),
                        }
                        tray.request_refresh();
                    }
                }),
                ..Default::default()
            }
            .into(),
        ];

        if !monitor.internal {
            submenu.push(
                menu::CheckmarkItem {
                    label: "Include in quick switch".into(),
                    checked: settings
                        .as_ref()
                        .is_some_and(|settings| settings.include_in_quick_switch),
                    activate: Box::new({
                        let monitor_id = monitor.id.clone();
                        move |tray: &mut MonitorTray| {
                            match toggle_controlled_monitor(&tray.shared, &monitor_id) {
                                Ok(enabled) => notify(
                                    APP_NAME,
                                    if enabled {
                                        "Monitor added to quick switch."
                                    } else {
                                        "Monitor removed from quick switch."
                                    },
                                ),
                                Err(err) => notify(APP_NAME, &format!("Failed to update monitor: {err}")),
                            }
                            tray.request_refresh();
                        }
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        submenu.push(ksni::MenuItem::Separator);
        submenu.extend(self.input_submenus(monitor, settings, is_primary));

        menu::SubMenu {
            label: monitor_label,
            submenu,
            ..Default::default()
        }
        .into()
    }

    fn input_submenus(
        &self,
        monitor: crate::monitor::MonitorInfo,
        settings: Option<crate::config::MonitorSettings>,
        is_primary: bool,
    ) -> Vec<ksni::MenuItem<Self>> {
        let mut items = Vec::new();
        let ddc = monitor.ddc.as_ref();

        if let Some(ddc) = ddc {
            let choices = if ddc.supported_inputs.is_empty() {
                fallback_input_choices(ddc.current_input.as_deref())
            } else {
                ddc.supported_inputs.clone()
            };

            items.push(self.input_choice_submenu(
                monitor.id.clone(),
                "Laptop input",
                settings
                    .as_ref()
                    .and_then(|settings| settings.laptop_input.as_deref()),
                &choices,
                true,
            ));

            if !is_primary {
                items.push(self.input_choice_submenu(
                    monitor.id.clone(),
                    "Toggle-to input",
                    settings
                        .as_ref()
                        .and_then(|settings| settings.toggle_input.as_deref()),
                    &choices,
                    false,
                ));
            }

            if ddc.current_input.is_some() {
                items.push(
                    menu::StandardItem {
                        label: format!(
                            "Use current input as laptop input ({})",
                            input_label(ddc.current_input.as_deref().unwrap_or_default())
                        ),
                        activate: Box::new({
                            let monitor_id = monitor.id.clone();
                            move |tray: &mut MonitorTray| {
                                match capture_current_input_as_laptop(&tray.shared, &monitor_id) {
                                    Ok(()) => notify(APP_NAME, "Laptop input captured from current monitor state."),
                                    Err(err) => notify(APP_NAME, &format!("Could not capture current input: {err}")),
                                }
                                tray.request_refresh();
                            }
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }

            items.push(
                menu::StandardItem {
                    label: format!(
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
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            items.push(
                menu::StandardItem {
                    label: "Input switching unavailable for this monitor".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        items
    }

    fn input_choice_submenu(
        &self,
        monitor_id: String,
        label: &str,
        selected: Option<&str>,
        choices: &[InputSource],
        laptop_input: bool,
    ) -> ksni::MenuItem<Self> {
        let mut submenu = Vec::new();
        let label_text = label.to_string();
        let selected_value = selected.map(str::to_string);

        submenu.push(
            menu::StandardItem {
                label: format!(
                    "Current: {}",
                    selected
                        .map(input_label)
                        .unwrap_or_else(|| "Not set".into())
                ),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );

        for choice in choices {
            let monitor_id = monitor_id.clone();
            let value = choice.value.clone();
            let choice_label = choice.label.clone();
            let checked = selected_value.as_deref() == Some(value.as_str());
            let label_text = label_text.clone();

            submenu.push(
                menu::CheckmarkItem {
                    label: choice_label.clone(),
                    checked,
                    activate: Box::new(move |tray: &mut MonitorTray| {
                        let result = if laptop_input {
                            set_laptop_input(&tray.shared, &monitor_id, Some(&value))
                        } else {
                            set_toggle_input(&tray.shared, &monitor_id, Some(&value))
                        };

                        match result {
                            Ok(()) => notify(APP_NAME, &format!("{label_text} set to {choice_label}.")),
                            Err(err) => notify(APP_NAME, &format!("Failed to save input: {err}")),
                        }
                        tray.request_refresh();
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        submenu.push(ksni::MenuItem::Separator);
        submenu.push(
            menu::StandardItem {
                label: "Clear selection".into(),
                activate: Box::new(move |tray: &mut MonitorTray| {
                    let result = if laptop_input {
                        set_laptop_input(&tray.shared, &monitor_id, None)
                    } else {
                        set_toggle_input(&tray.shared, &monitor_id, None)
                    };

                    match result {
                        Ok(()) => notify(APP_NAME, "Input preference cleared."),
                        Err(err) => notify(APP_NAME, &format!("Failed to clear input: {err}")),
                    }
                    tray.request_refresh();
                }),
                ..Default::default()
            }
            .into(),
        );

        menu::SubMenu {
            label: label.into(),
            submenu,
            ..Default::default()
        }
        .into()
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
            description: format!(
                "{}\nLeft click runs quick switch.",
                self.current_summary()
            ),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.trigger_quick_switch();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let snapshot = current_snapshot(&self.shared)
            .or_else(|_| refresh_snapshot(&self.shared))
            .ok();

        let mut items = vec![
            menu::StandardItem {
                label: self.current_summary(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            menu::StandardItem {
                label: "Quick switch now".into(),
                activate: Box::new(|tray: &mut MonitorTray| tray.trigger_quick_switch()),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
        ];

        if let Some(snapshot) = snapshot {
            let primary_label = resolve_primary(&snapshot, &self.shared)
                .map(|monitor| monitor.display_name)
                .unwrap_or_else(|| "Unavailable".into());

            items.push(
                menu::StandardItem {
                    label: format!("Selected primary: {primary_label}"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            items.push(
                menu::SubMenu {
                    label: "Monitors".into(),
                    submenu: snapshot
                        .monitors
                        .into_iter()
                        .map(|monitor| self.monitor_submenu(monitor))
                        .collect(),
                    ..Default::default()
                }
                .into(),
            );
            items.push(ksni::MenuItem::Separator);
        } else {
            items.push(
                menu::StandardItem {
                    label: "Monitor discovery unavailable".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(
            menu::StandardItem {
                label: "Save current layout".into(),
                activate: Box::new(|tray: &mut MonitorTray| {
                    match save_current_layout(&tray.shared) {
                        Ok(message) => notify(APP_NAME, &message),
                        Err(err) => notify(APP_NAME, &format!("Could not save layout: {err}")),
                    }
                    tray.request_refresh();
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            menu::StandardItem {
                label: "Refresh monitor state".into(),
                activate: Box::new(|tray: &mut MonitorTray| {
                    match refresh_snapshot(&tray.shared) {
                        Ok(_) => notify(APP_NAME, "Monitor state refreshed."),
                        Err(err) => notify(APP_NAME, &format!("Refresh failed: {err}")),
                    }
                    tray.request_refresh();
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            menu::CheckmarkItem {
                label: "Autostart".into(),
                checked: autostart_enabled(),
                activate: Box::new(|tray: &mut MonitorTray| tray.toggle_autostart()),
                ..Default::default()
            }
            .into(),
        );
        items.push(ksni::MenuItem::Separator);
        items.push(
            menu::StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut MonitorTray| tray.quit_signal.request()),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}
