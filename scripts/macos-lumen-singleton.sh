#!/bin/bash
# Ensure macOS exposes a single Lumen.app bundle to Finder/Spotlight.
set -euo pipefail

FIX=0
APP_NAME="Lumen"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_APP="/Applications/${APP_NAME}.app"
BUILD_APP="${ROOT_DIR}/src-tauri/target/release/bundle/macos/${APP_NAME}.app"
ARCHIVE_ROOT="${HOME}/Library/Application Support/Lumen/app-bundle-archive"

usage() {
  cat <<'EOF'
Usage: scripts/macos-lumen-singleton.sh [--fix]

Checks that macOS exposes exactly one Lumen.app bundle. With --fix, archives
/Applications/Lumen.app.backup-* bundles and removes the generated Tauri
build app bundle from src-tauri/target.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --fix) FIX=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

archive_application_backups() {
  local backups=()
  local path
  for path in /Applications/${APP_NAME}.app.backup-*; do
    [ -e "$path" ] || continue
    backups+=("$path")
  done

  if [ "${#backups[@]}" -eq 0 ]; then
    return 0
  fi

  local stamp archive_dir tarball
  stamp="$(date +%Y%m%d_%H%M%S)"
  archive_dir="${ARCHIVE_ROOT}/${stamp}"
  tarball="${archive_dir}.tar.gz"
  mkdir -p "$archive_dir"

  for path in "${backups[@]}"; do
    mv "$path" "${archive_dir}/$(basename "$path")"
  done

  tar -C "$(dirname "$archive_dir")" -czf "$tarball" "$(basename "$archive_dir")"
  rm -rf "$archive_dir"
  echo "Archived ${#backups[@]} backup bundle(s) to $tarball"
}

refresh_launch_services() {
  local lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
  [ -x "$lsregister" ] || return 0
  "$lsregister" -u "$BUILD_APP" >/dev/null 2>&1 || true
  [ -d "$INSTALL_APP" ] && "$lsregister" -f "$INSTALL_APP" >/dev/null 2>&1 || true
}

if [ "$FIX" -eq 1 ]; then
  archive_application_backups
  if [ -d "$BUILD_APP" ]; then
    rm -rf "$BUILD_APP"
    echo "Removed generated build app bundle: $BUILD_APP"
  fi
  refresh_launch_services
fi

filesystem_list="$(mktemp)"
indexed_list="$(mktemp)"
trap 'rm -f "$filesystem_list" "$indexed_list"' EXIT

find /Applications "$HOME/Applications" -maxdepth 2 -iname "${APP_NAME}.app" -print 2>/dev/null | sort -u >"$filesystem_list"
mdfind "kMDItemContentType == 'com.apple.application-bundle' && kMDItemFSName == '${APP_NAME}.app'" 2>/dev/null | sort -u >"$indexed_list"

filesystem_count="$(wc -l <"$filesystem_list" | tr -d ' ')"
indexed_count="$(wc -l <"$indexed_list" | tr -d ' ')"
filesystem_first="$(sed -n '1p' "$filesystem_list")"
indexed_first="$(sed -n '1p' "$indexed_list")"

echo "Filesystem Lumen.app bundles:"
if [ "$filesystem_count" -eq 0 ]; then
  echo "  <none>"
else
  sed 's/^/  /' "$filesystem_list"
fi
echo "Spotlight Lumen.app bundles:"
if [ "$indexed_count" -eq 0 ]; then
  echo "  <none>"
else
  sed 's/^/  /' "$indexed_list"
fi

if [ "$filesystem_count" -ne 1 ] || [ "$filesystem_first" != "$INSTALL_APP" ]; then
  echo "Expected exactly one filesystem bundle at $INSTALL_APP" >&2
  exit 1
fi

if [ "$indexed_count" -ne 1 ] || [ "$indexed_first" != "$INSTALL_APP" ]; then
  echo "Expected exactly one Spotlight bundle at $INSTALL_APP" >&2
  echo "Run with --fix, then give Spotlight a moment if it still reports stale entries." >&2
  exit 1
fi

if [ -f "$INSTALL_APP/Contents/Info.plist" ]; then
  version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$INSTALL_APP/Contents/Info.plist" 2>/dev/null || true)"
  build="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$INSTALL_APP/Contents/Info.plist" 2>/dev/null || true)"
  echo "Installed Lumen version: ${version:-unknown} (${build:-unknown})"
fi

echo "macOS Lumen app hygiene OK"
