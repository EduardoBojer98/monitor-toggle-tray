# Monitor Toggle Tray

`monitor-toggle-tray` is a Linux tray app for laptops that share one or more external monitors with another device.

It combines two jobs behind one tray icon:

- switch a monitor input with DDC/CI through `ddcutil`
- switch your desktop layout with `xrandr` or `kscreen-doctor`

Typical workflow:

- while working on the laptop, keep the external monitor on the laptop input and use your preferred multi-monitor layout
- when you want to hand that monitor to another machine, run a quick switch and the app changes the monitor input and turns the controlled external displays off for the laptop
- run the quick switch again to bring those displays back and restore the saved layout

The app is designed for user-local installation and lives in the system tray / status notifier area.

## Features

- Left click the tray icon to run a quick switch immediately
- Choose which display is treated as the primary laptop display
- Choose which external monitors participate in quick switch
- Store two per-monitor input preferences
- `Laptop input` is the input to use when switching the monitor back to this laptop
- `Toggle-to input` is the input to use when handing the monitor to another device
- Capture the current detected input and save it as the laptop input
- Save the current monitor layout so restored displays return to the expected position and size
- Enable or disable autostart from the tray menu
- Prevent multiple app instances from running at the same time
- Write a debug log under the XDG state directory

## How Quick Switch Works

When quick switch runs, the app:

1. Detects the current monitor state.
2. Resolves the primary display and the configured controlled external monitors.
3. Decides whether it is switching controlled monitors off or back on.

If any controlled external monitor is currently active, the app treats that as the "turn them off" direction:

- external controlled outputs are disabled from the laptop layout
- each controlled monitor is switched to its configured `Toggle-to input`, when DDC/CI input switching is available

If none of the controlled external monitors is active, the app treats that as the "turn them on" direction:

- each controlled monitor is switched to its configured `Laptop input`
- the saved display layout is restored with the detected backend

If a monitor does not expose DDC/CI input switching, or an input preference is not configured, the app still attempts the layout change and includes a note in the completion message.

## Tray Menu

The tray menu includes:

- a summary of the current primary display and controlled monitor count
- `Quick switch now`
- `Monitors`, with one submenu per detected monitor
- `Save current layout`
- `Refresh monitor state`
- `Autostart`
- `Quit`

Per-monitor submenu actions include:

- viewing connection status
- marking the monitor as the primary display
- including or excluding external monitors from quick switch
- setting `Laptop input`
- setting `Toggle-to input` for non-primary external monitors
- clearing saved input selections
- capturing the current input as the saved laptop input when available

## Requirements

### Supported environment

- Linux desktop session
- system tray / status notifier host available in your desktop environment
- at least one connected display
- for the input-switching part: a monitor that supports DDC/CI and has DDC/CI enabled in its on-screen menu

### Required tools

- Rust and Cargo
- `ddcutil`
- `xrandr` or `kscreen-doctor`

Display backend selection is automatic:

- on Wayland, the app prefers `kscreen-doctor` first and then falls back to `xrandr`
- on X11, the app prefers `xrandr` first and then falls back to `kscreen-doctor`

Notes:

- `ddcutil` is used both to detect current monitor inputs and to switch them
- the app can still manage layout even when DDC/CI input switching is unavailable for a monitor
- depending on your distro and hardware, `ddcutil` may require appropriate permissions to access I2C/DDC devices

## Building And Running

From the project root:

```bash
cargo run
```

Useful development commands:

```bash
cargo check
cargo test
cargo run
```

The app uses a single-instance lock. If another instance is already running, a second launch exits with a notification instead of starting another tray icon.

## Installation

Install with:

```bash
./scripts/install.sh
```

The installer performs a user-local install only. It does not write to `/usr`, `/usr/local`, or `/opt`.

What the installer does:

- builds a release binary with `cargo build --release`
- creates user-local XDG-style directories when needed
- stops a currently running app instance before replacing the binary
- installs the binary to `~/.local/bin/monitor-toggle-tray`
- installs the icon to `~/.local/share/icons/hicolor/scalable/apps/monitor-toggle-tray.svg`
- installs the desktop launcher to `~/.local/share/applications/monitor-toggle-tray.desktop`
- refreshes the autostart desktop file if autostart was already enabled
- restarts the app if it had been running before install
- refreshes desktop and icon caches when the helper tools are available

Launch after install with either:

```bash
monitor-toggle-tray
```

or:

```bash
~/.local/bin/monitor-toggle-tray
```

You can also launch it from the applications menu as `Monitor Toggle`.

## First-Time Setup

After the tray icon appears:

1. Open the `Monitors` submenu.
2. Confirm the correct primary display.
3. For each external monitor you want to control, enable `Include in quick switch`.
4. Set that monitor's `Laptop input`.
5. Set `Toggle-to input` if you want the monitor to switch to another device during quick switch off.
6. Arrange your displays the way you want them restored.
7. Click `Save current layout`.

If the monitor is already on the correct laptop input, you can use `Use current input as laptop input` instead of selecting it manually.

## Configuration And Stored Files

The app follows XDG directories.

Config file:

```text
${XDG_CONFIG_HOME:-~/.config}/monitor-toggle-tray/settings.toml
```

State directory:

```text
${XDG_STATE_HOME:-~/.local/state}/monitor-toggle-tray/
```

Debug log:

```text
${XDG_STATE_HOME:-~/.local/state}/monitor-toggle-tray/debug.log
```

Autostart entry:

```text
${XDG_CONFIG_HOME:-~/.config}/autostart/monitor-toggle-tray.desktop
```

The config stores:

- selected primary monitor id
- per-monitor quick-switch inclusion
- saved laptop and toggle-to input values
- saved monitor position and size for layout restore

On KDE Plasma, `Save current layout` can also read `kwinoutputconfig.json` to capture positions and sizes for the active set of displays.

## Autostart

Autostart is controlled from the tray menu and is disabled by default.

When enabled, the app writes a `.desktop` file to the XDG autostart directory that points to the currently installed executable path.

If you reinstall later with `./scripts/install.sh`, the installer refreshes that autostart file so it still points at the installed binary.

## Uninstall

Remove the app with:

```bash
./scripts/uninstall.sh
```

The uninstall script:

- stops a running instance if needed
- removes the binary, desktop launcher, icon, autostart entry, config directory, and state directory from your user profile
- refreshes desktop and icon caches when helper tools are available

It does not remove:

- your Rust toolchain
- project build artifacts under `target/`
- system packages such as `ddcutil`, `xrandr`, or `kscreen-doctor`

If you also want to remove local build artifacts from the repository:

```bash
cargo clean
```

## Troubleshooting

If quick switch or monitor detection is not working:

- verify `ddcutil detect --brief` works on your machine
- verify `ddcutil getvcp 60 --brief` works for the target display
- verify DDC/CI is enabled in the monitor's settings
- verify `xrandr --query` or `kscreen-doctor -o` works in the current session
- verify your desktop environment exposes a tray / status notifier host
- verify the external monitor is physically connected when restoring the laptop layout

If the tray app starts but input switching does nothing:

- the monitor may not support DDC/CI input switching
- `ddcutil` may not have the permissions it needs on this system
- the app may have fallen back to layout-only behavior for that monitor

If restoring the layout does not place monitors correctly:

- arrange the displays in your desktop settings first
- then use `Save current layout` from the tray menu
- on some setups, restoring without saved positions will still work but may not use the layout you expect

If you need more detail, check the debug log:

```text
${XDG_STATE_HOME:-~/.local/state}/monitor-toggle-tray/debug.log
```
