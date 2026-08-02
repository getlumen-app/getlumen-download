import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { transformSync } from "esbuild";
import vm from "node:vm";

const source = readFileSync(new URL("../src/lib/connectionState.ts", import.meta.url), "utf8");
const { code } = transformSync(source, {
  format: "cjs",
  loader: "ts",
  target: "es2022",
});
const context = {
  exports: {},
  module: { exports: {} },
};
vm.runInNewContext(code, context);
const lib = context.module.exports;

assert.equal(lib.transportFromEffectiveStatus("connected-tun"), "tun");
assert.equal(lib.transportFromEffectiveStatus("connected-proxy"), "proxy");
assert.equal(lib.transportFromEffectiveStatus("disconnected"), null);

assert.equal(
  lib.shouldStopTunOnDisconnect(null, false, {
    helper_installed: true,
    helper_running: true,
    singbox_running: true,
    singbox_pid: 929,
    uptime_secs: 9000,
  }),
  true,
  "disconnect must stop orphan root TUN even when React lost activeTransport"
);

assert.equal(
  lib.shouldStopTunOnDisconnect("proxy", false, {
    helper_installed: true,
    helper_running: true,
    singbox_running: false,
    singbox_pid: null,
    uptime_secs: null,
  }),
  true,
  "teardown always asks helper to stop — no-op when TUN already down"
);

{
  const plan = lib.planSessionTeardown("proxy", {
    helper_installed: true,
    helper_running: true,
    singbox_running: false,
    singbox_pid: null,
    uptime_secs: null,
  });
  assert.equal(plan.stopProxy, true, "proxy session must stop System Proxy");
  assert.equal(
    plan.stopTun,
    true,
    "teardown always stops both planes — never leave an orphan transport"
  );
}

{
  const plan = lib.planSessionTeardown("tun", {
    helper_installed: true,
    helper_running: true,
    singbox_running: true,
    singbox_pid: 1,
    uptime_secs: 1,
  });
  assert.equal(plan.stopProxy, true, "TUN teardown must also clear orphan proxy");
  assert.equal(plan.stopTun, true, "TUN teardown must stop helper TUN");
}

assert.equal(
  lib.shouldStopTunOnDisconnect("proxy", true, {
    helper_installed: true,
    helper_running: true,
    singbox_running: false,
    singbox_pid: null,
    uptime_secs: null,
  }),
  true,
  "preferredTun=true must still stop proxy AND ask TUN stop (never divert to TUN-only)"
);

assert.equal(
  lib.shouldApplyEffectiveStatusSync({
    connectionState: "disconnected",
    connectInFlight: true,
    storedIntent: "disconnected",
  }),
  "skip",
  "in-flight disconnect must not let status poll snap UI back to Connected"
);

assert.equal(
  lib.shouldApplyEffectiveStatusSync({
    connectionState: "disconnected",
    connectInFlight: false,
    storedIntent: "disconnected",
  }),
  "force_disconnected",
  "intent=disconnected must never apply a live TUN/proxy as Connected"
);

assert.equal(
  lib.shouldApplyEffectiveStatusSync({
    connectionState: "connected",
    connectInFlight: false,
    storedIntent: "connected",
  }),
  "apply",
  "intentional session may mirror effective status"
);

assert.equal(
  lib.shouldApplyEffectiveStatusSync({
    connectionState: "connecting",
    connectInFlight: false,
    storedIntent: "connected",
  }),
  "skip",
  "connecting owns the UI"
);

assert.equal(
  lib.shouldDisconnectOnPowerTap("connected"),
  true,
  "power tap while connected always disconnects"
);

assert.equal(
  lib.shouldDisconnectOnPowerTap("disconnected"),
  false,
  "power tap while disconnected is connect, not disconnect"
);

assert.equal(
  lib.shouldSwitchModeOnPowerTap("connected", "proxy", true),
  true,
  "connected on proxy + prefer TUN → Settings mode change switches transport"
);

assert.equal(
  lib.shouldSwitchModeOnPowerTap("connected", "tun", true),
  false,
  "already on preferred TUN → power tap disconnects"
);

assert.equal(
  lib.shouldSwitchModeOnPowerTap("connected", "tun", false),
  true,
  "connected on TUN + prefer proxy → power tap switches mode"
);

assert.equal(
  lib.shouldSwitchModeOnPowerTap("disconnected", "proxy", true),
  false,
  "disconnected → power tap is a normal connect, not a mode switch"
);

assert.equal(
  lib.shouldSelfHealOnLaunch("disconnected", "tun"),
  true,
  "launch self-heal should stop an orphan TUN left after an intended disconnect"
);

assert.equal(
  lib.shouldSelfHealOnLaunch("disconnected", "proxy"),
  true,
  "launch self-heal should stop an orphan proxy left after an intended disconnect"
);

assert.equal(
  lib.shouldSelfHealOnLaunch("connected", "tun"),
  false,
  "launch self-heal must not stop a VPN session the user intended to keep connected"
);

assert.equal(
  lib.readStoredConnectionIntent(null),
  "unknown",
  "missing intent after upgrade should sync UI without auto-stopping an intentional VPN"
);
assert.equal(lib.readStoredConnectionIntent("connected"), "connected");
assert.equal(lib.readStoredConnectionIntent("anything-else"), "disconnected");

assert.equal(
  lib.repairNetworkMessage({
    proxy_was_running: false,
    tun_was_running: false,
    proxy_stopped: false,
    tun_stopped: false,
    errors: [],
  }),
  "Network already clean"
);

assert.equal(
  lib.repairNetworkMessage({
    proxy_was_running: false,
    tun_was_running: true,
    proxy_stopped: false,
    tun_stopped: true,
    errors: [],
  }),
  "Network repaired"
);

assert.equal(
  lib.repairNetworkMessage({
    proxy_was_running: true,
    tun_was_running: true,
    proxy_stopped: true,
    tun_stopped: false,
    errors: ["helper timeout"],
  }),
  "Repair needs attention"
);

assert.equal(lib.diagnosticRouteLabel("connected-tun"), "TUN");
assert.equal(lib.diagnosticRouteLabel("connected-proxy"), "System Proxy");
assert.equal(lib.diagnosticRouteLabel("connected-wbstream"), "WB Stream");
assert.equal(lib.diagnosticRouteLabel("disconnected"), "Direct");

assert.equal(
  lib.diagnosticLocationLabel({
    effective_status: "disconnected",
    external_ip: "91.75.100.14",
    region: "Dubai",
    country: "United Arab Emirates",
    asn_org: "AS15802",
    error: null,
  }),
  "Dubai, United Arab Emirates"
);

assert.equal(
  lib.diagnosticLocationLabel({
    effective_status: "disconnected",
    external_ip: null,
    region: null,
    country: null,
    asn_org: null,
    error: "timeout",
  }),
  "Unavailable"
);

assert.equal(
  lib.formatDiagnosticsSnapshot(
    {
      effective_status: "disconnected",
      helper_installed: true,
      helper_running: true,
      tun_running: false,
      external_ip: "91.75.100.14",
      region: "Dubai",
      country: "United Arab Emirates",
      asn_org: "Emirates Integrated Telecommunications Company PJSC",
      error: null,
    },
    new Date("2026-05-24T18:22:00.000Z")
  ),
  [
    "Lumen diagnostics",
    "Time: 2026-05-24T18:22:00.000Z",
    "Route: Direct",
    "External IP: 91.75.100.14",
    "Location: Dubai, United Arab Emirates",
    "Provider: Emirates Integrated Telecommunications Company PJSC",
    "Probe: Unknown via Unknown",
    "Helper: Running",
    "TUN: Stopped",
  ].join("\n")
);

assert.equal(
  lib.formatDiagnosticsSnapshot(
    {
      effective_status: "connected-tun",
      helper_installed: true,
      helper_running: true,
      tun_running: true,
      external_ip: null,
      region: null,
      country: null,
      asn_org: null,
      error: "external ip: timeout",
    },
    new Date("2026-05-24T18:22:00.000Z")
  ).includes("Diagnostics error: external ip: timeout"),
  true
);

assert.equal(
  typeof lib.shouldAttemptWbstreamOnConnectError,
  "function",
  "connect-error classifier must exist so control-plane failures do not jump to WB Stream"
);

assert.equal(
  lib.shouldAttemptWbstreamOnConnectError(
    "Config fetch failed: error sending request (no usable cached config: missing)"
  ),
  false,
  "control-plane config fetch failure must NOT auto-start WB Stream"
);

assert.equal(
  lib.shouldAttemptWbstreamOnConnectError("Config fetch failed: TLS reset"),
  false,
  "config fetch failure without cache still must NOT auto-start WB Stream"
);

assert.equal(
  lib.shouldAttemptWbstreamOnConnectError("VLESS parse failed: bad link"),
  false,
  "bad key/link must NOT auto-start WB Stream"
);

assert.equal(
  lib.shouldAttemptWbstreamOnConnectError("helper not running"),
  false,
  "local helper problems are not hard-whitelist symptoms"
);

console.log("connection-state tests OK");
