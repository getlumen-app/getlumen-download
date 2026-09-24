import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { transformSync } from "esbuild";
import vm from "node:vm";

const libUrl = new URL("../src/lib/configRefresh.ts", import.meta.url);
assert.ok(existsSync(libUrl), "src/lib/configRefresh.ts must exist");
const { code } = transformSync(readFileSync(libUrl, "utf8"), { format: "cjs", loader: "ts", target: "es2022" });
const context = { exports: {}, module: { exports: {} } };
vm.runInNewContext(code, context);
const lib = context.module.exports;

const now = new Date(2026, 8, 24, 8, 31, 0).getTime();

// Success in words the user asked for: downloaded AND installed, with a time.
assert.equal(
  lib.refreshStatusMessage({ kind: "downloaded", updated_at_ms: now }, { reconnected: false }),
  "Config downloaded and installed · 08:31"
);
assert.equal(
  lib.refreshStatusMessage({ kind: "downloaded", updated_at_ms: now }, { reconnected: true }),
  "Config downloaded and installed · 08:31 · VPN reconnected"
);
// A single-link profile has nothing to download — say so, don't fake success.
assert.match(lib.refreshStatusMessage({ kind: "static", updated_at_ms: null }, { reconnected: false }), /nothing to download/i);

// Errors are shown, trimmed, never swallowed.
assert.equal(lib.refreshErrorMessage("Config download failed: timeout"), "Couldn't download config: timeout");
assert.ok(lib.refreshErrorMessage("x".repeat(500)).length <= 160);

// "Config updated" reflects the real file time; unknown is not "just now".
assert.equal(lib.formatConfigUpdated(null, now), "never");
assert.equal(lib.formatConfigUpdated(now - 20_000, now), "just now");
assert.equal(lib.formatConfigUpdated(now - 5 * 60_000, now), "5 min ago");
assert.equal(lib.formatConfigUpdated(new Date(2026, 8, 24, 6, 5).getTime(), now), "today 06:05");
assert.equal(lib.formatConfigUpdated(new Date(2026, 8, 20, 6, 5).getTime(), now), "20 Sep 06:05");

// Wiring: the button must have a handler, show a busy state, and the fake
// hard-coded "just now" label must stay gone.
const settings = readFileSync(new URL("../src/pages/Settings.tsx", import.meta.url), "utf8");
assert.ok(!settings.includes("Config updated: just now"), "Config updated label must not be hard-coded");
assert.match(settings, /onClick=\{handleRefreshConfig\}/, "Refresh Config button must call handleRefreshConfig");
assert.match(settings, /settings__spinner/, "Refresh Config must render a spinner while busy");
const tauriHooks = readFileSync(new URL("../src/hooks/useTauri.ts", import.meta.url), "utf8");
assert.match(tauriHooks, /invoke<[^>]+>\("refresh_config"/, "useTauri must call refresh_config");

console.log("config refresh tests passed");
