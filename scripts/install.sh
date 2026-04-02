#!/usr/bin/env bash

set -euo pipefail

APP_ID="monitor-toggle-tray"
APP_NAME="Monitor Input & Layout Switcher"
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
XDG_BIN_HOME="${XDG_BIN_HOME:-${HOME}/.local/bin}"
XDG_DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME}/.config}"
XDG_STATE_HOME="${XDG_STATE_HOME:-${HOME}/.local/state}"
BIN_DIR="${XDG_BIN_HOME}"
APP_DIR="${XDG_DATA_HOME}/applications"
ICON_DIR="${XDG_DATA_HOME}/icons/hicolor/scalable/apps"
AUTOSTART_DIR="${XDG_CONFIG_HOME}/autostart"
CONFIG_DIR="${XDG_CONFIG_HOME}/${APP_ID}"
STATE_DIR="${XDG_STATE_HOME}/${APP_ID}"
BIN_PATH="${BIN_DIR}/${APP_ID}"
DESKTOP_PATH="${APP_DIR}/${APP_ID}.desktop"
ICON_PATH="${ICON_DIR}/${APP_ID}.svg"
AUTOSTART_PATH="${AUTOSTART_DIR}/${APP_ID}.desktop"

escape_desktop_value() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

write_desktop_entry() {
    local target_path="$1"
    local autostart_enabled="${2:-false}"
    local escaped_exec
    escaped_exec="$(escape_desktop_value "${BIN_PATH}")"

    cat > "${target_path}" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=${APP_NAME}
Comment=Tray app for switching monitor inputs and restoring desktop layouts
Exec="${escaped_exec}"
Icon=${APP_ID}
Terminal=false
Categories=Utility;
StartupNotify=false
StartupWMClass=${APP_ID}
X-GNOME-UsesNotifications=true
EOF

    if [[ "${autostart_enabled}" == "true" ]]; then
        cat >> "${target_path}" <<EOF
X-GNOME-Autostart-enabled=true
EOF
    fi
}

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}" "${AUTOSTART_DIR}" "${CONFIG_DIR}" "${STATE_DIR}"

WAS_RUNNING="false"
if pgrep -x "${APP_ID}" >/dev/null 2>&1; then
    WAS_RUNNING="true"
    echo "Stopping running ${APP_NAME} instance..."
    pkill -x "${APP_ID}" || true
    sleep 1
fi

echo "Building ${APP_NAME}..."
cargo build --release --manifest-path "${PROJECT_DIR}/Cargo.toml"

echo "Installing binary to ${BIN_PATH}..."
install -m 755 "${PROJECT_DIR}/target/release/${APP_ID}" "${BIN_PATH}"

echo "Installing app icon to ${ICON_PATH}..."
install -m 644 "${PROJECT_DIR}/assets/${APP_ID}.svg" "${ICON_PATH}"

rm -f "${ICON_DIR}/${APP_ID}-hdmi1.svg" "${ICON_DIR}/${APP_ID}-hdmi2.svg"

echo "Installing desktop launcher to ${DESKTOP_PATH}..."
write_desktop_entry "${DESKTOP_PATH}"

if [[ -f "${AUTOSTART_PATH}" ]]; then
    echo "Refreshing autostart entry at ${AUTOSTART_PATH}..."
    write_desktop_entry "${AUTOSTART_PATH}" "true"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache "${XDG_DATA_HOME}/icons/hicolor" >/dev/null 2>&1 || true
fi

if command -v xdg-desktop-menu >/dev/null 2>&1; then
    xdg-desktop-menu forceupdate >/dev/null 2>&1 || true
fi

if command -v xdg-icon-resource >/dev/null 2>&1; then
    xdg-icon-resource forceupdate >/dev/null 2>&1 || true
fi

if [[ "${WAS_RUNNING}" == "true" ]]; then
    echo "Restarting ${APP_NAME}..."
    nohup "${BIN_PATH}" >/dev/null 2>&1 &
fi

cat <<EOF

Install complete.

Run the app with:
  ${BIN_PATH}

If ~/.local/bin is in your PATH, you can also run:
  ${APP_ID}

The launcher is available in your applications menu as:
  ${APP_NAME}

Open Settings from the tray menu to configure monitors, inputs, layout restore, and autostart.
EOF
