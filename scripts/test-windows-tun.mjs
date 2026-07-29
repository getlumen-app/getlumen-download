import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const lib = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
const cargo = await readFile(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
const tunWindows = await readFile(
  new URL("../src-tauri/src/tun_windows.rs", import.meta.url),
  "utf8",
);
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
  workflow,
  /cargo test --manifest-path src-tauri\/Cargo\.toml --release --lib/,
  "the Windows runner must execute Rust tests before building NSIS",
);
assert.match(
  tunWindows,
  /LUMEN_WINDOWS_TUN_CANARY/,
  "Windows TUN must require an explicit operator canary opt-in",
);
assert.match(
  tunWindows,
  /if !windows_tun_canary_enabled\(\)/,
  "the privileged Windows TUN start path must fail closed without canary opt-in",
);
assert.match(
  settings,
  /localStorage\.getItem\("lumen-vpn-mode"\) as VpnMode\) \|\| "proxy"/,
  "Windows-safe connection settings must default to System Proxy",
);

console.log("Windows TUN source contract tests passed");
