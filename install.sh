#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Lumen"
REPO="getlumen-app/getlumen-download"
INSTALL_DIR="/Applications"
DRY_RUN=0

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: install.sh [--dry-run]" >&2
      exit 2
      ;;
  esac
done

log() {
  printf '%s\n' "$*"
}

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

require_cmd curl
require_cmd file
require_cmd hdiutil

case "$(uname -s)" in
  Darwin) ;;
  *) fail "Lumen installer currently supports macOS only" ;;
esac

case "$(uname -m)" in
  arm64) ARCH_SUFFIX="aarch64" ;;
  x86_64) ARCH_SUFFIX="x86_64" ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

TMPDIR_PATH="$(mktemp -d)"
MOUNT_POINT=""

cleanup() {
  if [ -n "$MOUNT_POINT" ]; then
    hdiutil detach "$MOUNT_POINT" -quiet >/dev/null 2>&1 || true
  fi
  rm -rf "$TMPDIR_PATH"
}
trap cleanup EXIT

log "Lumen installer"
log "Repository: $REPO"
log "Architecture: $ARCH_SUFFIX"

release_json="$TMPDIR_PATH/latest.json"
curl -fsSL --connect-timeout 10 --max-time 30 \
  "https://api.github.com/repos/${REPO}/releases/latest" \
  -o "$release_json"

download_url="$(
  grep '"browser_download_url"' "$release_json" \
    | grep -E "_${ARCH_SUFFIX}\\.dmg\"" \
    | head -1 \
    | sed 's/.*"browser_download_url": *"\([^"]*\)".*/\1/'
)"

[ -n "$download_url" ] || fail "could not find ${ARCH_SUFFIX} DMG in latest release"

dmg_name="$(basename "$download_url")"
dmg_path="$TMPDIR_PATH/$dmg_name"

log "Downloading: $dmg_name"
curl -fL --connect-timeout 10 --max-time 300 --retry 2 \
  -o "$dmg_path" \
  "$download_url"

[ -s "$dmg_path" ] || fail "downloaded DMG is empty"

file_type="$(file -b "$dmg_path")"
if ! printf '%s' "$file_type" | grep -qiE 'zlib|disk image|Apple'; then
  fail "downloaded file is not a DMG: $file_type"
fi

log "Verified DMG: $file_type"

if [ "$DRY_RUN" = "1" ]; then
  log "Dry run complete. No files were installed."
  exit 0
fi

log "Mounting DMG"
if ! mount_output="$(hdiutil attach "$dmg_path" -nobrowse -noautoopen 2>&1)"; then
  printf '%s\n' "$mount_output" >&2
  fail "failed to attach DMG"
fi
MOUNT_POINT="$(printf '%s\n' "$mount_output" | awk -F '\t' 'index($0, "/Volumes/") {print $NF; exit}')"

[ -n "$MOUNT_POINT" ] && [ -d "$MOUNT_POINT" ] || {
  printf '%s\n' "$mount_output" >&2
  fail "failed to find mounted DMG volume"
}
[ -d "$MOUNT_POINT/$APP_NAME.app" ] || fail "DMG does not contain $APP_NAME.app"

log "Installing to $INSTALL_DIR/$APP_NAME.app"
osascript -e "tell application \"$APP_NAME\" to quit" >/dev/null 2>&1 || true

if [ -d "$INSTALL_DIR/$APP_NAME.app" ]; then
  rm -rf "$INSTALL_DIR/$APP_NAME.app"
fi

ditto "$MOUNT_POINT/$APP_NAME.app" "$INSTALL_DIR/$APP_NAME.app"
xattr -cr "$INSTALL_DIR/$APP_NAME.app" >/dev/null 2>&1 || true

installed_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$INSTALL_DIR/$APP_NAME.app/Contents/Info.plist" 2>/dev/null || true)"

log "Installed $APP_NAME ${installed_version:-unknown}"
log "Open $INSTALL_DIR/$APP_NAME.app"
