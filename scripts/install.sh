#!/usr/bin/env bash

set -euo pipefail

APP_ID="monitor-toggle-tray"
APP_NAME="Monitor Toggle"
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
BIN_PATH="${BIN_DIR}/${APP_ID}"
DESKTOP_PATH="${APP_DIR}/${APP_ID}.desktop"

mkdir -p "${BIN_DIR}" "${APP_DIR}"

echo "Building ${APP_NAME}..."
cargo build --release --manifest-path "${PROJECT_DIR}/Cargo.toml"

echo "Installing binary to ${BIN_PATH}..."
install -m 755 "${PROJECT_DIR}/target/release/${APP_ID}" "${BIN_PATH}"

echo "Installing desktop launcher to ${DESKTOP_PATH}..."
cat > "${DESKTOP_PATH}" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=${APP_NAME}
Comment=Tray app for switching monitor input
Exec=${BIN_PATH}
Icon=video-display
Terminal=false
Categories=Utility;
StartupNotify=false
EOF

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" >/dev/null 2>&1 || true
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
