import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const lib = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
const cargo = await readFile(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
const tunWindows = await readFile(
  new URL("../src-tauri/src/tun_windows.rs", import.meta.url),
  "utf8",
);
const config = await readFile(
  new URL("../src-tauri/src/config.rs", import.meta.url),
  "utf8",
);
const clashApi = await readFile(
  new URL("../src-tauri/src/clash_api.rs", import.meta.url),
  "utf8",
);
const app = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");
const settings = await readFile(
  new URL("../src/pages/Settings.tsx", import.meta.url),
  "utf8",
);
const workflow = await readFile(
  new URL("../.github/workflows/release-guard.yml", import.meta.url),
  "utf8",
);

assert.match(
  lib,
  /cfg\(any\(target_os = "macos", target_os = "windows"\)\)\]\s*mod tun_commands;/,
  "the shared TUN command module must compile on Windows",
);
assert.match(
  lib,
  /cfg\(target_os = "windows"\)\]\s*mod tun_windows;/,
  "Windows must have a dedicated elevated TUN runtime",
);
for (const command of [
  "tun_status",
  "tun_install_helper",
  "tun_uninstall_helper",
  "tun_start",
  "tun_stop",
  "tun_connect",
  "tun_disconnect",
]) {
  assert.match(
    lib,
    new RegExp(
      `cfg\\(any\\(target_os = "macos", target_os = "windows"\\)\\)\\]\\s*tun_commands::${command}`,
    ),
    `${command} must be registered on Windows`,
  );
}
assert.match(
  cargo,
  /windows-sys\s*=\s*\{[\s\S]*Win32_UI_Shell/,
  "Windows elevation must use the typed Windows API dependency",
);
assert.match(
  cargo,
  /windows-sys\s*=\s*\{[\s\S]*Win32_Security/,
  "elevation detection needs the Windows token API",
);
assert.match(
  workflow,
  /cargo test --manifest-path src-tauri\/Cargo\.toml --release --lib/,
  "the Windows runner must execute Rust tests before building NSIS",
);

// Windows can only create a Wintun adapter from an elevated process. Lumen
// unlocks TUN the way the working Windows clients do — by restarting itself as
// administrator — instead of leaving TUN permanently unavailable.
assert.match(
  tunWindows,
  /pub fn is_elevated\(\) -> bool/,
  "the Windows runtime must know whether it holds an administrator token",
);
assert.match(
  tunWindows,
  /pub fn relaunch_self_elevated\(\) -> Result<\(\), String>/,
  "Windows must be able to unlock TUN by restarting Lumen elevated",
);
assert.match(
  tunWindows,
  /if !is_elevated\(\) \{\s*return Err\(ELEVATION_REQUIRED_MESSAGE/,
  "the privileged Windows TUN start path must fail closed without elevation",
);
assert.doesNotMatch(
  tunWindows,
  /LUMEN_WINDOWS_TUN_CANARY/,
  "Windows TUN must no longer be gated behind an operator-only canary flag",
);

// The 2.5.8 canary reported success on process liveness alone and left the
// machine without internet. Readiness is now proven by real traffic, and a
// failed start restores normal routing instead of stranding the user.
assert.match(
  tunWindows,
  /async fn wait_until_tun_carries_traffic/,
  "Windows TUN must verify that traffic actually passes before reporting success",
);
assert.match(
  tunWindows,
  /Err\(reason\) => \{\s*let _ = child\.kill\(\);/,
  "a TUN that never carries traffic must be torn down automatically",
);
assert.match(
  tunWindows,
  /pub fn tun_log_path\(\)/,
  "the Windows TUN session must write a diagnosable sing-box log",
);

// sing-box persists selector pins in cache.db, and `dns-proxy` detours through
// that same selector — so under TUN a pinned-but-dead exit kills every lookup.
// Observed 2026-08-10: a `relay-eu-443` pin made every TUN connect fail.
assert.match(
  tunWindows,
  /crate::clash_api::reset_selector_to_auto\(\)/,
  "a silent TUN must drop a dead manual exit pin before giving up",
);
assert.match(
  clashApi,
  /pub async fn reset_selector_to_auto\(\)/,
  "pin recovery must be one shared helper for both transports",
);
assert.match(
  lib,
  /async fn ensure_local_proxy_route_health/,
  "System Proxy must recover from the same dead pin as TUN",
);
assert.match(
  app,
  /listen<string>\("lumen:\/\/exit-pin-reset"/,
  "the shell must forget a dropped pin, or it re-pins the dead exit on the next poll",
);
assert.match(
  config,
  /"windows" => TunPolicy \{[\s\S]*?stack: "gvisor"/,
  "Windows must use the netstack the working Windows clients ship",
);

assert.match(
  settings,
  /localStorage\.getItem\("lumen-vpn-mode"\) as VpnMode\) \|\| "proxy"/,
  "Windows-safe connection settings must default to System Proxy",
);
assert.match(
  settings,
  /Enable TUN Mode/,
  "Settings must offer the Windows elevation unlock instead of a macOS-only helper install",
);

console.log("Windows TUN source contract tests passed");
