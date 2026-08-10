import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const lib = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
const cargo = await readFile(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
const app = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");

assert.match(
  cargo,
  /tauri = \{[^}]*features = \[[^\]]*"tray-icon"/,
  "the tray icon needs the Tauri tray feature compiled in",
);
assert.match(
  lib,
  /TrayIconBuilder::with_id\("lumen"\)/,
  "Lumen must install a tray icon at startup",
);
for (const item of ["show", "disconnect", "quit"]) {
  assert.match(
    lib,
    new RegExp(`"${item}" =>`),
    `the tray menu must handle the ${item} action`,
  );
}

// A VPN client has to outlive its window: closing parks Lumen in the tray with
// the tunnel intact, and only the explicit tray Quit tears the session down.
assert.match(
  lib,
  /WindowEvent::CloseRequested \{ api, \.\. \}[\s\S]{0,200}api\.prevent_close\(\);\s*let _ = window\.hide\(\);/,
  "closing the window must hide Lumen instead of killing the tunnel",
);
assert.match(
  lib,
  /"quit" => \{\s*shutdown_network_runtime\(app\);\s*app\.exit\(0\);/,
  "quitting from the tray must restore normal routing before exiting",
);
assert.match(
  lib,
  /pub\(crate\) fn shutdown_network_runtime/,
  "there must be one shared teardown path for quit and elevated restart",
);
assert.match(
  lib,
  /const TRAY_DISCONNECT_EVENT: &str = "lumen:\/\/tray-disconnect";/,
  "the tray disconnect action must be delivered to the shell as an event",
);
assert.match(
  app,
  /listen\("lumen:\/\/tray-disconnect"/,
  "the React shell must run the same teardown for the tray as for the power button",
);

console.log("tray and background source contract tests passed");
