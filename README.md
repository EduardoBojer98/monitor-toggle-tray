# Monitor Toggle Tray

Small Linux tray app for switching your monitor input and adjusting the laptop/external display layout.

## Install

Run:

```bash
./scripts/install.sh
```

This will:

- build the release binary
- install it to `~/.local/bin/monitor-toggle-tray`
- install a desktop launcher to `~/.local/share/applications/monitor-toggle-tray.desktop`

After that you can launch it from your applications menu or run:

```bash
monitor-toggle-tray
```

If `~/.local/bin` is not in your `PATH`, run:

```bash
~/.local/bin/monitor-toggle-tray
```

## Dependencies

The app expects these tools to be available on your system:

- `ddcutil`
- `xrandr` or `kscreen-doctor`

## Autostart

Launch the app once, then enable `Autostart` from the tray menu.

## Uninstall

Run:

```bash
./scripts/uninstall.sh
```
