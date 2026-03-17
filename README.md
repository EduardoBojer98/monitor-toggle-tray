# Monitor Toggle Tray

`monitor-toggle-tray` is a small Linux tray app for quickly switching an external monitor between two inputs and updating your display layout to match.

It is built for a workflow like this:

- `HDMI 1`: use the external monitor with this laptop and keep an extended desktop
- `HDMI 2`: hand the external monitor over to another device and switch this laptop back to its internal display only

The app lives in the system tray, shows the current monitor input when it can detect it, and lets you toggle inputs with one click or from the tray menu.

## What The App Does

When you switch to `HDMI 1`, the app:

- sends a DDC/CI command with `ddcutil` to change the external monitor input to `HDMI 1`
- tries to detect your laptop display and one connected external display
- applies an extended layout
- shows a desktop notification with the result

When you switch to `HDMI 2`, the app:

- sends a DDC/CI command with `ddcutil` to change the external monitor input to `HDMI 2`
- waits briefly for the monitor switch to settle
- switches your laptop back to its internal display only
- shows a desktop notification with the result

The tray menu also includes:

- the current detected input
- an `Autostart` toggle
- `Quit`

The app only allows one running instance at a time.

## Requirements

Install Rust and Cargo, then make sure these tools are available on your system:

- `ddcutil`
- `xrandr` or `kscreen-doctor`

`ddcutil` is used to change the monitor input through DDC/CI.

`xrandr` and `kscreen-doctor` are used to manage display layout:

- on X11, the app usually prefers `xrandr`
- on Wayland, the app usually prefers `kscreen-doctor`
- if one backend is unavailable, it will try the other

Your monitor must support DDC/CI, and DDC/CI must be enabled in the monitor's on-screen settings.

## Running As A Developer

From the project directory:

```bash
cargo run
```

That starts the tray app directly from your development build.

Useful development commands:

```bash
cargo check
cargo test
cargo run
```

Notes for local testing:

- make sure your desktop session has a tray / status notifier host
- make sure the external monitor is connected
- make sure `ddcutil` can talk to the monitor on your machine
- if another copy of the app is already running, the new one will exit and show a notification

## Installing

Run:

```bash
./scripts/install.sh
```

The installer does a user-local install. It does not install system-wide and does not place files under `/usr` or `/opt`.

### What The Installer Does

The script:

- builds the app in release mode with `cargo build --release`
- creates `~/.local/bin` if needed
- creates `~/.local/share/applications` if needed
- creates `~/.local/share/icons/hicolor/scalable/apps` if needed
- stops a running tray instance before replacing the binary
- installs the binary to `~/.local/bin/monitor-toggle-tray`
- installs the app icon to `~/.local/share/icons/hicolor/scalable/apps/monitor-toggle-tray.svg`
- installs a desktop launcher to `~/.local/share/applications/monitor-toggle-tray.desktop`
- refreshes `~/.config/autostart/monitor-toggle-tray.desktop` when autostart is already enabled
- restarts the tray app after install when it was already running
- refreshes the desktop application database when `update-desktop-database` is available

After installation you can launch it with:

```bash
monitor-toggle-tray
```

If `~/.local/bin` is not in your `PATH`, run:

```bash
~/.local/bin/monitor-toggle-tray
```

You can also launch it from your applications menu as `Monitor Toggle`.

## Autostart

Autostart is not enabled by the install script.

To enable it:

1. Start the app once.
2. Open the tray menu.
3. Turn on `Autostart`.

When enabled, the app writes this file:

```text
~/.config/autostart/monitor-toggle-tray.desktop
```

That autostart entry points to the currently installed executable path.

To disable autostart, open the tray menu again and turn `Autostart` off.

## Uninstalling

Run:

```bash
./scripts/uninstall.sh
```

### What The Uninstall Script Removes

The uninstall script removes these user-local files if they exist:

- `~/.local/bin/monitor-toggle-tray`
- `~/.local/share/applications/monitor-toggle-tray.desktop`
- `~/.local/share/icons/hicolor/scalable/apps/monitor-toggle-tray.svg`
- `~/.config/autostart/monitor-toggle-tray.desktop`

It also refreshes the desktop application database when `update-desktop-database` is available.

### What It Does Not Remove

The uninstall script does not remove:

- your Rust toolchain
- Cargo build output in the project `target/` directory
- system packages such as `ddcutil`, `xrandr`, or `kscreen-doctor`
- any desktop notification history created by your environment

If you also want to remove local build artifacts from the repo, run:

```bash
cargo clean
```

## Troubleshooting

If switching does not work as expected:

- verify that `ddcutil getvcp 60` works for your monitor
- verify that DDC/CI is enabled on the monitor
- verify that `xrandr --query` or `kscreen-doctor -o` works in your session
- check whether your desktop environment supports tray icons
- make sure the external display is actually connected when switching to `HDMI 1`

If the app launches but does not change layouts, the issue is usually with display backend availability, desktop session support, or monitor control permissions rather than the tray app itself.
