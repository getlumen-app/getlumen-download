# Architecture Gate

Product/surface: Lumen 2.6.1 Windows TUN runtime + background/tray lifecycle
Date: 2026-08-10
Owner: getlumen-app

## Goal

Make TUN mode actually reachable on Windows, and let Lumen keep a tunnel up
after its window is closed.

## Gate

| Field | Decision |
|---|---|
| Task class | `runtime_infra` + `side_effecting_workflow` |
| LLM-owned | None |
| Code-owned | Elevation detection and unlock, child-process lifecycle, traffic-proven readiness, tray/background lifecycle, teardown on quit |
| Existing assets reused | Bundled sing-box 1.11.8, Tauri command surface, macOS helper protocol, exact-PID record, NSIS CI |
| Required schemas | Existing `Request`/`Response` runtime protocol; `TunStatus` extended with `platform` + `needs_elevation` |
| Required validators | Rust unit tests, frontend build, source-contract tests for Windows TUN and tray, Windows `cargo test --release --lib`, Windows NSIS build |
| Required golden cases | TUN reports unavailable without an administrator token; a TUN that carries no traffic is torn down automatically; closing the window never drops the tunnel; quitting from the tray restores normal routing |
| Runtime/state | Per-user data directory holds one PID record and one TUN log; no service, credentials, or machine-wide state |
| Proof gate | Green Windows tests plus an installer built from this tree |
| Human gate | UAC consent once per elevated session, and a real Windows connection check before promoting a public release |

## Baseline

2.5.8 registered the Windows TUN commands but gated the runtime behind
`LUMEN_WINDOWS_TUN_CANARY=1`, so `tun_status` reported TUN missing on every
ordinary install and the app silently stayed on System Proxy. The canary that
did run declared success on process liveness after 700 ms and left the machine
without internet. sing-box ran as an elevated `ShellExecuteEx` orphan: no
captured log, and a second UAC prompt to stop it.

Separately, closing the window terminated the app, so Lumen could not hold a
tunnel in the background.

## Diff

- Delete the canary flag; derive availability from the bundled sing-box plus an
  administrator token.
- Unlock TUN by relaunching Lumen elevated (v2rayN / NekoRay model) instead of
  elevating the core per connect.
- Run sing-box as a Lumen child with `stdout`/`stderr` in `singbox-tun.log`;
  stop it by terminating the recorded PID, keeping the elevated-taskkill
  fallback for 2.5.8-era orphans.
- Gate "connected" on a real request surviving the tunnel; tear the session
  down and report the sing-box log tail when it does not.
- Pin the `gvisor` netstack on Windows.
- Recover from a manual exit pin that carries no traffic: drop it back to
  `proxy-auto` mid-readiness (both transports) and tell the shell to forget the
  stored location, so it cannot be re-applied on the next poll.
- Scope System Proxy process matching to Lumen's own data directory, so an
  elevated Lumen cannot stop another client's sing-box.
- Add a tray icon (Show / Disconnect / Quit); closing the window hides it, and
  quitting runs one shared teardown that also serves the elevated restart.
- Show whichever sing-box log is current on the Logs screen.

## Uplift

Windows TUN is reachable by an ordinary user for the first time. A failed TUN
can no longer masquerade as a working one, and it can no longer strand the
machine: the failure path restores routing and names its cause. Disconnect costs
zero extra prompts. A closed window no longer means a dropped tunnel.

## Benchmarks

- Source-contract tests prove the canary is gone, the start path fails closed
  without elevation, readiness is traffic-proven, and the failure path kills the
  child.
- Tray contract tests prove close-hides, quit-tears-down, and that tray
  disconnect reuses the shell's teardown.
- Config tests prove the Windows and macOS TUN policies independently.
- A regression test proves another client's `sing-box … config.json` is never
  mistaken for Lumen's.
- Windows CI compiles/tests the Windows-only FFI path and creates the installer.

## Residual Risk

Elevation is held by the whole app while TUN is enabled, so the WebView2 UI runs
elevated in that session — the same trade the reference Windows clients make.
The window is narrowed by keeping System Proxy the default and only elevating on
explicit opt-in. Traffic-proven readiness covers the "up but dead" class of
failure but not a tunnel that degrades later; the existing health monitor still
owns that. WB Stream fallback on Windows remains out of scope.

## Rollback

Restart Lumen normally: without an administrator token the runtime reports TUN
unavailable and every session uses System Proxy. Any recorded TUN process is
stopped from Settings → Repair Network.
