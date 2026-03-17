#!/usr/bin/env bash

set -euo pipefail

APP_ID="monitor-toggle-tray"
BIN_PATH="${HOME}/.local/bin/${APP_ID}"
DESKTOP_PATH="${HOME}/.local/share/applications/${APP_ID}.desktop"
AUTOSTART_PATH="${HOME}/.config/autostart/${APP_ID}.desktop"

rm -f "${BIN_PATH}"
rm -f "${DESKTOP_PATH}"
rm -f "${AUTOSTART_PATH}"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${HOME}/.local/share/applications" >/dev/null 2>&1 || true
fi

echo "Uninstalled ${APP_ID} from your user profile."
