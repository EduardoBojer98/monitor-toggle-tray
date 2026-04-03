#!/usr/bin/env bash

set -euo pipefail

APP_ID="monitor-toggle-tray"
APP_NAME="Monitor Input & Layout Switcher"
XDG_BIN_HOME="${XDG_BIN_HOME:-${HOME}/.local/bin}"
XDG_DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME}/.config}"
XDG_STATE_HOME="${XDG_STATE_HOME:-${HOME}/.local/state}"
BIN_PATH="${XDG_BIN_HOME}/${APP_ID}"
DESKTOP_PATH="${XDG_DATA_HOME}/applications/${APP_ID}.desktop"
AUTOSTART_PATH="${XDG_CONFIG_HOME}/autostart/${APP_ID}.desktop"
ICON_PATH="${XDG_DATA_HOME}/icons/hicolor/scalable/apps/${APP_ID}.svg"
ICON_HDMI1_PATH="${XDG_DATA_HOME}/icons/hicolor/scalable/apps/${APP_ID}-hdmi1.svg"
ICON_HDMI2_PATH="${XDG_DATA_HOME}/icons/hicolor/scalable/apps/${APP_ID}-hdmi2.svg"
CONFIG_DIR="${XDG_CONFIG_HOME}/${APP_ID}"
STATE_DIR="${XDG_STATE_HOME}/${APP_ID}"
PURGE="false"

for arg in "$@"; do
    case "$arg" in
        --purge)
            PURGE="true"
            ;;
        -h|--help)
            cat <<EOF
Usage: ./scripts/uninstall.sh [--purge]

Options:
  --purge    Also remove user config and state directories.
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done

if pgrep -x "${APP_ID}" >/dev/null 2>&1; then
    echo "Stopping running ${APP_NAME} instance..."
    pkill -x "${APP_ID}" || true
    sleep 1
fi

rm -f "${BIN_PATH}"
rm -f "${DESKTOP_PATH}"
rm -f "${AUTOSTART_PATH}"
rm -f "${ICON_PATH}"
rm -f "${ICON_HDMI1_PATH}"
rm -f "${ICON_HDMI2_PATH}"

if [[ "${PURGE}" == "true" ]]; then
    rm -rf "${CONFIG_DIR}"
    rm -rf "${STATE_DIR}"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${XDG_DATA_HOME}/applications" >/dev/null 2>&1 || true
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

if [[ "${PURGE}" == "true" ]]; then
    echo "Uninstalled ${APP_NAME} (${APP_ID}) from your user profile and removed config/state data."
else
    echo "Uninstalled ${APP_NAME} (${APP_ID}) from your user profile."
    echo "User config and logs were kept. Re-run with --purge to remove ${CONFIG_DIR} and ${STATE_DIR}."
fi
