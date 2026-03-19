#!/usr/bin/env bash

set -euo pipefail

APP_ID="monitor-toggle-tray"
APP_NAME="Monitor Toggle"
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
AUTOSTART_DIR="${HOME}/.config/autostart"
BIN_PATH="${BIN_DIR}/${APP_ID}"
DESKTOP_PATH="${APP_DIR}/${APP_ID}.desktop"
ICON_PATH="${ICON_DIR}/${APP_ID}.svg"
ICON_HDMI1_PATH="${ICON_DIR}/${APP_ID}-hdmi1.svg"
ICON_HDMI2_PATH="${ICON_DIR}/${APP_ID}-hdmi2.svg"
AUTOSTART_PATH="${AUTOSTART_DIR}/${APP_ID}.desktop"

write_desktop_entry() {
    local target_path="$1"
    local autostart_enabled="${2:-false}"

    cat > "${target_path}" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=${APP_NAME}
Comment=Tray app for switching monitor input
Exec=${BIN_PATH}
Icon=${ICON_PATH}
Terminal=false
Categories=Utility;
StartupNotify=false
EOF

    if [[ "${autostart_enabled}" == "true" ]]; then
        cat >> "${target_path}" <<EOF
X-GNOME-Autostart-enabled=true
EOF
    fi
}

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}" "${AUTOSTART_DIR}"

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

echo "Installing HDMI 1 tray icon to ${ICON_HDMI1_PATH}..."
install -m 644 "${PROJECT_DIR}/assets/${APP_ID}-hdmi1.svg" "${ICON_HDMI1_PATH}"

echo "Installing HDMI 2 tray icon to ${ICON_HDMI2_PATH}..."
install -m 644 "${PROJECT_DIR}/assets/${APP_ID}-hdmi2.svg" "${ICON_HDMI2_PATH}"

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
    gtk-update-icon-cache "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true
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

Once the tray icon is running, enable autostart from the tray menu if you want it to start on login.
EOF
