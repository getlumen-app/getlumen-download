#!/bin/bash
# Lumen VPN — macOS installer (TUN mode by default)
# Usage: curl -sL https://getlumen.download/install | bash
set -euo pipefail

APP_NAME="Lumen"
REPO="getlumen-app/getlumen-download"
INSTALL_DIR="/Applications"
HELPER_LABEL="io.getlumen.helper"

echo "=========================================="
echo "  Lumen VPN — One-command installer"
echo "=========================================="
echo ""

# ─── Pre-install cleanup ───
# Remove proxy remnants from Hiddify/V2Box/Clash that break Electron apps
# (Claude Desktop, ChatGPT, etc.) Verified necessary 2026-04-16 on @STmarkml
echo "[0/5] Cleaning up old VPN proxy settings..."

# 1. System proxy (scutil)
for iface in Wi-Fi Ethernet "Thunderbolt Bridge" "USB 10/100/1000 LAN"; do
  networksetup -setsocksfirewallproxystate "$iface" off 2>/dev/null || true
  networksetup -setwebproxystate "$iface" off 2>/dev/null || true
  networksetup -setsecurewebproxystate "$iface" off 2>/dev/null || true
done

# 2. launchctl GUI environment proxy vars
for var in http_proxy https_proxy all_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY no_proxy; do
  launchctl unsetenv "$var" 2>/dev/null || true
done

# 3. Shell proxy exports in .zshrc / .bash_profile
for rc in "$HOME/.zshrc" "$HOME/.bash_profile" "$HOME/.bashrc"; do
  if [ -f "$rc" ] && grep -qi 'proxy' "$rc" 2>/dev/null; then
    cp "$rc" "${rc}.bak.pre-lumen"
    sed -i.tmp '/[Pp]roxy/d; /set_proxy/d' "$rc"
    rm -f "${rc}.tmp"
    echo "  Cleaned proxy lines from $(basename "$rc")"
  fi
done

# 4. Remove set_proxy.sh and LaunchAgent (Hiddify/V2Box auto-restorer)
[ -f "$HOME/set_proxy.sh" ] && rm -f "$HOME/set_proxy.sh" && echo "  Removed set_proxy.sh"
if [ -f "$HOME/Library/LaunchAgents/com.proxy.socks.plist" ]; then
  launchctl unload "$HOME/Library/LaunchAgents/com.proxy.socks.plist" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/com.proxy.socks.plist"
  echo "  Removed com.proxy.socks LaunchAgent"
fi

# 5. Flush DNS cache (poisoned entries from ISP)
dscacheutil -flushcache 2>/dev/null || true
killall -HUP mDNSResponder 2>/dev/null || true

# 6. Remove old Lumen data (clean state)
killall Lumen 2>/dev/null || true
killall sing-box 2>/dev/null || true
rm -rf "$HOME/Library/Caches/io.getlumen.app"
rm -rf "$HOME/Library/Application Support/io.getlumen.app"
rm -rf "$HOME/Library/Saved Application State/io.getlumen.app.savedState"
rm -rf "$HOME/Library/WebKit/io.getlumen.app"

echo "  ✓ Proxy cleanup done"
echo ""

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
  arm64)  ARCH_SUFFIX="aarch64" ;;
  x86_64) ARCH_SUFFIX="x86_64" ;;
  *)
    echo "✗ Unsupported architecture: $ARCH"
    exit 1
    ;;
esac

# Find DMG URL from release assets (handles version mismatch with tag)
echo "[1/5] Fetching latest release..."
RELEASE_JSON=$(curl -sL "https://api.github.com/repos/${REPO}/releases/latest")
DOWNLOAD_URL=$(echo "$RELEASE_JSON" | grep '"browser_download_url"' | grep -E "_${ARCH_SUFFIX}\.dmg" | head -1 | sed 's/.*"browser_download_url": *"\(.*\)"/\1/')

if [ -z "$DOWNLOAD_URL" ]; then
  echo "✗ Could not find DMG for ${ARCH_SUFFIX} in latest release"
  exit 1
fi

DMG_NAME=$(basename "$DOWNLOAD_URL")
echo "[2/5] Downloading ${DMG_NAME}..."
TMPDIR_PATH=$(mktemp -d)
DMG_PATH="${TMPDIR_PATH}/${DMG_NAME}"
curl -fsSL -o "$DMG_PATH" "$DOWNLOAD_URL"

if [ ! -f "$DMG_PATH" ] || [ ! -s "$DMG_PATH" ]; then
  echo "✗ Download failed (URL: $DOWNLOAD_URL)"
  rm -rf "$TMPDIR_PATH"
  exit 1
fi

# Verify it's actually a DMG (not HTML 404 page)
FILE_TYPE=$(file -b "$DMG_PATH")
if ! echo "$FILE_TYPE" | grep -qiE "zlib|disk image|Apple"; then
  echo "✗ Downloaded file is not a DMG: $FILE_TYPE"
  echo "  URL: $DOWNLOAD_URL"
  echo "  First 200 chars: $(head -c 200 "$DMG_PATH")"
  rm -rf "$TMPDIR_PATH"
  exit 1
fi

# Mount DMG
echo "[3/5] Mounting DMG..."
MOUNT_OUTPUT=$(hdiutil attach "$DMG_PATH" -nobrowse -noautoopen 2>&1)
MOUNT_POINT=$(echo "$MOUNT_OUTPUT" | grep "/Volumes" | awk '{print $NF}')

if [ -z "$MOUNT_POINT" ]; then
  MOUNT_POINT=$(echo "$MOUNT_OUTPUT" | tail -1 | awk -F'\t' '{print $NF}')
fi

if [ -z "$MOUNT_POINT" ] || [ ! -d "$MOUNT_POINT" ]; then
  echo "✗ Failed to mount DMG"
  rm -rf "$TMPDIR_PATH"
  exit 1
fi

# Copy app
echo "[4/5] Installing to ${INSTALL_DIR}..."
if [ -d "${INSTALL_DIR}/${APP_NAME}.app" ]; then
  rm -rf "${INSTALL_DIR}/${APP_NAME}.app"
fi
cp -R "${MOUNT_POINT}/${APP_NAME}.app" "${INSTALL_DIR}/"
hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true

# We need sudo for: xattr quarantine remove + helper install
echo "[5/5] Setting up VPN helper for TUN mode (faster, kernel-level routing)"
echo "      You'll be asked for your Mac password (one-time setup)"
echo ""

HELPER_BIN="${INSTALL_DIR}/${APP_NAME}.app/Contents/Resources/_up_/bin/lumen-helper"
INSTALLER_BIN="${INSTALL_DIR}/${APP_NAME}.app/Contents/Resources/_up_/bin/lumen-installer"

# Single sudo block — does both quarantine + helper install
sudo bash -c "
  set -e
  # Remove quarantine flag (allows app to run unsigned)
  xattr -rd com.apple.quarantine '${INSTALL_DIR}/${APP_NAME}.app' 2>/dev/null || true
  # Install + start the privileged helper daemon
  if [ -x '${INSTALLER_BIN}' ] && [ -f '${HELPER_BIN}' ]; then
    '${INSTALLER_BIN}' install '${HELPER_BIN}'
    echo '  ✓ TUN helper installed and started'
  else
    echo '  ⚠ Helper binaries not found, falling back to system proxy mode'
  fi
" || {
  echo "  ⚠ Sudo setup failed — falling back to xattr only (proxy mode)"
  xattr -cr "${INSTALL_DIR}/${APP_NAME}.app" 2>/dev/null || true
}

# Cleanup
rm -rf "$TMPDIR_PATH"

echo ""
echo "=========================================="
echo "  ${APP_NAME} installed successfully"
echo "=========================================="
echo ""
echo "  • Open Lumen and paste your subscription key"
echo "  • TUN mode auto-enabled if helper installed"
echo ""

# Auto-launch
open "${INSTALL_DIR}/${APP_NAME}.app"
