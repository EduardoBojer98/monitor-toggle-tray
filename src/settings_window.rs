use crate::app::{
    APP_NAME, SettingsMonitorUpdate, SettingsUpdate, SharedState, app_icon_path, apply_settings,
    load_settings_view, notify, save_current_layout,
};
use gtk::glib::{self, ControlFlow};
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};

#[derive(Clone)]
pub struct SettingsWindowHandle {
    tx: Sender<SettingsCommand>,
}

impl SettingsWindowHandle {
    pub fn open(&self) {
        self.tx.send(SettingsCommand::Present).ok();
    }
}

enum SettingsCommand {
    Present,
}

struct MonitorWidgets {
    id: String,
    display_name: String,
    internal: bool,
    include_in_quick_switch: Option<gtk::CheckButton>,
    laptop_input: Option<gtk::ComboBoxText>,
    toggle_input: Option<gtk::ComboBoxText>,
}

pub fn spawn(shared: SharedState, refresh_tx: Sender<()>) -> SettingsWindowHandle {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        if let Err(err) = gtk::init() {
            notify(
                APP_NAME,
                &format!("Could not initialize settings window: {err}"),
            );
            return;
        }

        let window = gtk::Window::builder()
            .title(format!("{APP_NAME} Settings"))
            .default_width(1160)
            .default_height(820)
            .build();
        install_styles();
        window.set_titlebar(Some(&build_titlebar()));
        window.connect_close_request(|window| {
            window.hide();
            glib::Propagation::Stop
        });

        let command_shared = shared.clone();
        let command_refresh_tx = refresh_tx.clone();
        let command_window = window.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            while let Ok(command) = rx.try_recv() {
                match command {
                    SettingsCommand::Present => {
                        if let Err(err) =
                            rebuild_window(&command_window, &command_shared, &command_refresh_tx)
                        {
                            notify(APP_NAME, &format!("Could not open settings: {err}"));
                        } else {
                            command_window.present();
                        }
                    }
                }
            }

            ControlFlow::Continue
        });

        glib::MainLoop::new(None, false).run();
    });

    SettingsWindowHandle { tx }
}

fn rebuild_window(
    window: &gtk::Window,
    shared: &SharedState,
    refresh_tx: &Sender<()>,
) -> Result<(), String> {
    let view = load_settings_view(shared)?;
    let monitor_widgets = Rc::new(RefCell::new(Vec::<MonitorWidgets>::new()));
    let (window_width, window_height) = current_window_size(window);
    let sidebar_width = target_sidebar_width(window_width);

    let viewport = gtk::ScrolledWindow::new();
    viewport.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    viewport.set_hexpand(true);
    viewport.set_vexpand(true);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);
    root.set_hexpand(true);
    root.set_vexpand(true);

    let hero = gtk::Frame::new(None);
    hero.add_css_class("hero-card");
    let hero_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    hero_box.set_margin_top(12);
    hero_box.set_margin_bottom(12);
    hero_box.set_margin_start(14);
    hero_box.set_margin_end(14);

    let eyebrow = gtk::Label::new(Some("Monitor Input & Layout Switcher"));
    eyebrow.set_xalign(0.0);
    eyebrow.add_css_class("caption-heading");
    hero_box.append(&eyebrow);

    let heading = gtk::Label::new(Some("Settings"));
    heading.set_xalign(0.0);
    heading.add_css_class("title-1");
    hero_box.append(&heading);

    let subtitle = gtk::Label::new(Some(
        "A dedicated place to set up primary display behavior, quick-switch monitors, inputs, layout restore, and autostart.",
    ));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.add_css_class("dim-label");
    hero_box.append(&subtitle);
    hero.set_child(Some(&hero_box));
    root.append(&hero);

    let status_frame = gtk::Frame::new(None);
    status_frame.add_css_class("status-card");
    let status_box = gtk::Box::new(gtk::Orientation::Vertical, 3);
    status_box.set_margin_top(10);
    status_box.set_margin_bottom(10);
    status_box.set_margin_start(12);
    status_box.set_margin_end(12);
    let status_title = gtk::Label::new(Some("Current Status"));
    status_title.set_xalign(0.0);
    status_title.add_css_class("heading");
    status_box.append(&status_title);

    let status_label = gtk::Label::new(Some(&view.status_text));
    status_label.set_xalign(0.0);
    status_label.add_css_class("dim-label");
    status_label.set_wrap(true);
    status_box.append(&status_label);
    status_frame.set_child(Some(&status_box));
    root.append(&status_frame);

    let content = gtk::Paned::new(gtk::Orientation::Horizontal);
    content.set_wide_handle(true);
    content.set_position(sidebar_width);
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_resize_start_child(true);
    content.set_resize_end_child(true);
    content.set_shrink_start_child(true);
    content.set_shrink_end_child(true);

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.set_hexpand(true);
    sidebar.set_vexpand(true);

    let controls_frame = gtk::Frame::new(Some("Actions"));
    controls_frame.add_css_class("sidebar-card");
    controls_frame.set_hexpand(true);
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 6);
    controls.set_margin_top(10);
    controls.set_margin_bottom(10);
    controls.set_margin_start(10);
    controls.set_margin_end(10);
    let save_button = gtk::Button::with_label("Save Changes");
    save_button.add_css_class("suggested-action");
    save_button.set_hexpand(true);
    let reset_button = gtk::Button::with_label("Reset");
    let refresh_button = gtk::Button::with_label("Refresh Monitor State");
    let layout_button = gtk::Button::with_label("Save Current Layout");
    controls.append(&save_button);
    controls.append(&reset_button);
    controls.append(&refresh_button);
    controls.append(&layout_button);
    controls_frame.set_child(Some(&controls));
    sidebar.append(&controls_frame);

    let general_frame = gtk::Frame::new(Some("General"));
    general_frame.add_css_class("sidebar-card");
    general_frame.set_hexpand(true);
    let general_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    general_box.set_margin_top(10);
    general_box.set_margin_bottom(10);
    general_box.set_margin_start(10);
    general_box.set_margin_end(10);

    let primary_row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let primary_label = gtk::Label::new(Some("Primary display"));
    primary_label.set_xalign(0.0);
    let primary_combo = gtk::ComboBoxText::new();
    primary_combo.set_hexpand(true);
    primary_combo.append(Some("__auto__"), "Automatic / built-in display");
    for monitor in &view.monitors {
        primary_combo.append(
            Some(&monitor.id),
            &format!(
                "{} ({}){}",
                monitor.display_name,
                monitor.output_name,
                if monitor.connected {
                    ""
                } else {
                    ", disconnected"
                }
            ),
        );
    }
    primary_combo.set_active_id(view.primary_monitor_id.as_deref().or(Some("__auto__")));
    primary_row.append(&primary_label);
    primary_row.append(&primary_combo);
    general_box.append(&primary_row);

    let autostart_check = gtk::CheckButton::with_label("Start automatically on login");
    autostart_check.set_active(view.autostart_enabled);
    general_box.append(&autostart_check);

    general_frame.set_child(Some(&general_box));
    sidebar.append(&general_frame);

    let quick_help_frame = gtk::Frame::new(Some("How To Use"));
    quick_help_frame.add_css_class("sidebar-card");
    let quick_help_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    quick_help_box.set_margin_top(10);
    quick_help_box.set_margin_bottom(10);
    quick_help_box.set_margin_start(10);
    quick_help_box.set_margin_end(10);
    for line in [
        "1. Pick the primary display.",
        "2. Choose which external monitors quick switch controls.",
        "3. Set Laptop input and Toggle-to input for those monitors.",
        "4. Arrange displays, then save the current layout.",
        "5. Save changes once everything looks right.",
    ] {
        let label = gtk::Label::new(Some(line));
        label.set_xalign(0.0);
        label.set_wrap(true);
        quick_help_box.append(&label);
    }
    quick_help_frame.set_child(Some(&quick_help_box));
    sidebar.append(&quick_help_frame);

    let diagnostics_expander = gtk::Expander::builder()
        .label("Diagnostics")
        .expanded(false)
        .build();
    let diagnostics_label = gtk::Label::new(Some(&view.diagnostics.join("\n")));
    diagnostics_label.set_xalign(0.0);
    diagnostics_label.set_yalign(0.0);
    diagnostics_label.set_selectable(true);
    diagnostics_label.set_wrap(true);
    diagnostics_expander.set_child(Some(&diagnostics_label));
    sidebar.append(&diagnostics_expander);

    let sidebar_scroll = gtk::ScrolledWindow::new();
    sidebar_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    sidebar_scroll.set_hexpand(true);
    sidebar_scroll.set_vexpand(true);
    sidebar_scroll.set_min_content_width(sidebar_width.clamp(280, 420));
    sidebar_scroll.set_child(Some(&sidebar));
    content.set_start_child(Some(&sidebar_scroll));

    let monitors_frame = gtk::Frame::new(None);
    monitors_frame.add_css_class("monitor-list");
    monitors_frame.set_hexpand(true);
    monitors_frame.set_vexpand(true);
    let monitors_scroll = gtk::ScrolledWindow::new();
    monitors_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    monitors_scroll.set_hexpand(true);
    monitors_scroll.set_vexpand(true);
    let monitors_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    monitors_box.set_margin_top(10);
    monitors_box.set_margin_bottom(10);
    monitors_box.set_margin_start(10);
    monitors_box.set_margin_end(10);

    let monitors_title = gtk::Label::new(Some("Monitors"));
    monitors_title.set_xalign(0.0);
    monitors_title.add_css_class("heading");
    monitors_box.append(&monitors_title);

    for monitor in &view.monitors {
        let frame = gtk::Frame::new(None);
        frame.add_css_class("monitor-card");
        frame.set_hexpand(true);
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
        box_.set_margin_top(10);
        box_.set_margin_bottom(10);
        box_.set_margin_start(10);
        box_.set_margin_end(10);
        box_.set_hexpand(true);

        let top_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        top_row.set_hexpand(true);
        let title_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        title_box.set_hexpand(true);
        let title = gtk::Label::new(Some(&monitor.display_name));
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.add_css_class("heading");
        title_box.append(&title);
        let output = gtk::Label::new(Some(&format!(
            "{}{}",
            monitor.output_name,
            if monitor.internal {
                " • Built-in"
            } else {
                ""
            }
        )));
        output.set_xalign(0.0);
        output.set_wrap(true);
        output.add_css_class("dim-label");
        title_box.append(&output);
        top_row.append(&title_box);

        let badge = gtk::Label::new(Some(if monitor.active {
            "Active"
        } else if monitor.connected {
            "Connected"
        } else {
            "Disconnected"
        }));
        badge.add_css_class("pill");
        top_row.append(&badge);
        box_.append(&top_row);

        let status = gtk::Label::new(Some(&format!(
            "Screen state: {}{}",
            if monitor.connected {
                if monitor.active {
                    "active"
                } else {
                    "connected"
                }
            } else {
                "disconnected"
            },
            if monitor.is_primary {
                " • Selected primary"
            } else {
                ""
            }
        )));
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.add_css_class("dim-label");
        box_.append(&status);

        let include_check = if monitor.internal {
            None
        } else {
            let check = gtk::CheckButton::with_label("Include in quick switch");
            check.set_active(monitor.include_in_quick_switch);
            box_.append(&check);
            Some(check)
        };

        let capability = gtk::Label::new(Some(&monitor.ddc_status));
        capability.set_xalign(0.0);
        capability.set_wrap(true);
        capability.add_css_class("dim-label");
        box_.append(&capability);

        let current_input_label = gtk::Label::new(Some(&format!(
            "Current detected input: {}",
            monitor
                .current_input
                .as_deref()
                .map(crate::monitor::input_label)
                .unwrap_or_else(|| "Unavailable".into())
        )));
        current_input_label.set_xalign(0.0);
        current_input_label.set_wrap(true);
        box_.append(&current_input_label);

        let laptop_input = if monitor.available_inputs.is_empty() {
            let label = gtk::Label::new(Some("Laptop input: unavailable"));
            label.set_xalign(0.0);
            label.set_wrap(true);
            box_.append(&label);
            None
        } else {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_hexpand(true);
            let label = gtk::Label::new(Some("Laptop input"));
            label.set_xalign(0.0);
            label.set_width_chars(18);
            let combo = gtk::ComboBoxText::new();
            combo.set_hexpand(true);
            combo.append(Some(""), "Not set");
            for input in &monitor.available_inputs {
                combo.append(Some(&input.value), &input.label);
            }
            combo.set_active_id(monitor.laptop_input.as_deref().or(Some("")));
            row.append(&label);
            row.append(&combo);

            if let Some(current_input) = monitor.current_input.clone() {
                let capture_button = gtk::Button::with_label("Use Current");
                let combo_clone = combo.clone();
                capture_button.connect_clicked(move |_| {
                    combo_clone.set_active_id(Some(&current_input));
                });
                row.append(&capture_button);
            }

            box_.append(&row);
            Some(combo)
        };

        let toggle_input = if monitor.internal || monitor.available_inputs.is_empty() {
            if !monitor.internal {
                let label = gtk::Label::new(Some("Toggle-to input: unavailable"));
                label.set_xalign(0.0);
                box_.append(&label);
            }
            None
        } else {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.set_hexpand(true);
            let label = gtk::Label::new(Some("Toggle-to input"));
            label.set_xalign(0.0);
            label.set_width_chars(18);
            let combo = gtk::ComboBoxText::new();
            combo.set_hexpand(true);
            combo.append(Some(""), "Not set");
            for input in &monitor.available_inputs {
                combo.append(Some(&input.value), &input.label);
            }
            combo.set_active_id(monitor.toggle_input.as_deref().or(Some("")));
            row.append(&label);
            row.append(&combo);
            box_.append(&row);
            Some(combo)
        };

        if !monitor.internal && monitor.available_inputs.is_empty() {
            let help = gtk::Label::new(Some(
                "No input choices were detected for this monitor. Layout changes can still work even if input switching is unavailable.",
            ));
            help.set_xalign(0.0);
            help.set_wrap(true);
            help.add_css_class("dim-label");
            box_.append(&help);
        }

        monitor_widgets.borrow_mut().push(MonitorWidgets {
            id: monitor.id.clone(),
            display_name: monitor.display_name.clone(),
            internal: monitor.internal,
            include_in_quick_switch: include_check,
            laptop_input,
            toggle_input,
        });

        frame.set_child(Some(&box_));
        monitors_box.append(&frame);
    }

    monitors_scroll.set_child(Some(&monitors_box));
    monitors_frame.set_child(Some(&monitors_scroll));
    content.set_end_child(Some(&monitors_frame));

    root.append(&content);
    viewport.set_child(Some(&root));

    window.set_child(Some(&viewport));
    window.set_default_size(window_width, window_height);

    {
        let shared = shared.clone();
        let refresh_tx = refresh_tx.clone();
        let status_label = status_label.clone();
        let primary_combo = primary_combo.clone();
        let autostart_check = autostart_check.clone();
        let monitor_widgets = monitor_widgets.clone();
        save_button.connect_clicked(move |_| {
            let update = SettingsUpdate {
                primary_monitor_id: primary_combo.active_id().and_then(|id| {
                    let value = id.to_string();
                    if value == "__auto__" {
                        None
                    } else {
                        Some(value)
                    }
                }),
                autostart_enabled: autostart_check.is_active(),
                monitors: monitor_widgets
                    .borrow()
                    .iter()
                    .map(|monitor| SettingsMonitorUpdate {
                        id: monitor.id.clone(),
                        display_name: monitor.display_name.clone(),
                        internal: monitor.internal,
                        include_in_quick_switch: monitor
                            .include_in_quick_switch
                            .as_ref()
                            .is_some_and(|check| check.is_active()),
                        laptop_input: combo_value(monitor.laptop_input.as_ref()),
                        toggle_input: combo_value(monitor.toggle_input.as_ref()),
                    })
                    .collect(),
            };

            match apply_settings(&shared, update) {
                Ok(message) => {
                    status_label.set_text(&message);
                    refresh_tx.send(()).ok();
                    notify(APP_NAME, &message);
                }
                Err(err) => {
                    let message = format!("Could not save settings: {err}");
                    status_label.set_text(&message);
                    notify(APP_NAME, &message);
                }
            }
        });
    }

    {
        let window = window.clone();
        let shared = shared.clone();
        let refresh_tx = refresh_tx.clone();
        reset_button.connect_clicked(move |_| {
            if let Err(err) = rebuild_window(&window, &shared, &refresh_tx) {
                notify(APP_NAME, &format!("Could not reset settings: {err}"));
            }
        });
    }

    {
        let window = window.clone();
        let shared = shared.clone();
        let refresh_tx = refresh_tx.clone();
        refresh_button.connect_clicked(move |_| {
            if let Err(err) = rebuild_window(&window, &shared, &refresh_tx) {
                notify(APP_NAME, &format!("Could not refresh settings: {err}"));
            } else {
                refresh_tx.send(()).ok();
            }
        });
    }

    {
        let shared = shared.clone();
        let refresh_tx = refresh_tx.clone();
        let status_label = status_label.clone();
        layout_button.connect_clicked(move |_| match save_current_layout(&shared) {
            Ok(message) => {
                status_label.set_text(&message);
                notify(APP_NAME, &message);
                refresh_tx.send(()).ok();
            }
            Err(err) => notify(APP_NAME, &format!("Could not save layout: {err}")),
        });
    }

    Ok(())
}

fn combo_value(combo: Option<&gtk::ComboBoxText>) -> Option<String> {
    combo.and_then(|combo| {
        combo.active_id().and_then(|id| {
            let value = id.to_string();
            if value.is_empty() { None } else { Some(value) }
        })
    })
}

fn current_window_size(window: &gtk::Window) -> (i32, i32) {
    let width = window.width();
    let height = window.height();

    (
        if width > 0 { width } else { 1160 },
        if height > 0 { height } else { 820 },
    )
}

fn target_sidebar_width(window_width: i32) -> i32 {
    ((window_width as f32) * 0.30).round() as i32
}

fn build_header_icon() -> gtk::Widget {
    if let Some(icon_path) = app_icon_path()
        && let Some(path) = icon_path.to_str()
    {
        let image = gtk::Image::from_file(path);
        image.set_pixel_size(32);
        let frame = gtk::Frame::new(None);
        frame.add_css_class("header-icon-frame");
        frame.set_size_request(44, 44);
        frame.set_child(Some(&image));
        frame.set_hexpand(false);
        return frame.upcast();
    }

    let image = gtk::Image::from_icon_name("video-display");
    image.set_pixel_size(32);
    image.upcast()
}

fn build_titlebar() -> gtk::HeaderBar {
    let header = gtk::HeaderBar::new();
    header.set_show_title_buttons(true);
    header.set_decoration_layout(Some(":minimize,maximize,close"));

    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    title_box.set_halign(gtk::Align::Center);
    let title = gtk::Label::new(Some(APP_NAME));
    title.add_css_class("heading");
    title.set_xalign(0.5);
    let subtitle = gtk::Label::new(Some("Settings"));
    subtitle.add_css_class("dim-label");
    subtitle.set_xalign(0.5);
    title_box.append(&title);
    title_box.append(&subtitle);
    header.set_title_widget(Some(&title_box));

    let icon = build_header_icon();
    header.pack_start(&icon);

    header
}

fn install_styles() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "
        .hero-card {
            background: linear-gradient(135deg, rgba(27, 79, 114, 0.10), rgba(21, 67, 96, 0.04));
            border-radius: 16px;
        }
        .status-card, .sidebar-card, .monitor-card, .monitor-list, .header-icon-frame {
            border-radius: 14px;
        }
        .pill {
            background: rgba(39, 174, 96, 0.16);
            color: @theme_fg_color;
            padding: 4px 10px;
            border-radius: 999px;
            font-weight: 700;
        }
        .header-icon-frame {
            padding: 4px;
            background: rgba(27, 79, 114, 0.10);
        }
        ",
    );

    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
