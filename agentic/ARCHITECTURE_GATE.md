# Architecture Gate

Product/surface: Lumen 2.5.8 Windows TUN runtime
Date: 2026-07-29
Owner: getlumen-app

## Goal

Make the Windows “TUN” connection mode start a real privileged sing-box TUN
process instead of exposing macOS-only commands that fail at runtime.

## Gate

| Field | Decision |
|---|---|
| Task class | `runtime_infra` + `side_effecting_workflow` |
| LLM-owned | None |
| Code-owned | Platform capability detection, Windows elevation, exact PID lifecycle, TUN config, cleanup, status, and release proof |
| Existing assets reused | Bundled sing-box 1.11.8, Tauri command surface, existing macOS helper protocol, NSIS CI |
| Required schemas | Existing `Request`/`Response` runtime protocol and `TunStatus` response |
| Required validators | Rust unit tests, frontend build, release-guard tests, Windows `cargo test --release --lib`, Windows NSIS build |
| Required golden cases | Windows commands are registered; Windows TUN config uses Windows-safe interface/MTU/strict route; only the recorded elevated PID can be stopped |
| Runtime/state | Per-user config directory stores one PID record; no service, credentials, or shared machine-wide state |
| Proof gate | Green Windows CI plus installer existence/hash/PE inspection before operator delivery |
| Human gate | UAC consent at TUN connect/disconnect and a real Windows connection check before promoting a public release |

## Baseline

The React UI exposes TUN on Windows, but every TUN command is compiled and
registered only on macOS. Windows therefore reports `Command
tun_install_helper not found` and falls back to System Proxy. The current TUN
JSON also hard-codes a macOS `utun777` interface and jumbo MTU.

## Diff

- Compile and register the TUN command surface on macOS and Windows.
- Add a Windows runtime that starts bundled `sing-box.exe` through UAC and
  persists only its exact PID for status/stop.
- Generate platform-safe TUN inbound settings.
- Make full server configs honor the requested inbound mode.
- Run Rust tests on the Windows release runner before producing NSIS.
- Bump all application version surfaces to 2.5.8.

## Uplift

The reported missing-command failure becomes impossible on a Windows build.
Privilege is limited to the TUN process rather than the whole UI, and cleanup
cannot use an unscoped process-name kill.

## Benchmarks

- Regression test proves Windows TUN commands are present in the Tauri handler.
- Config tests prove Windows and macOS policies independently.
- Windows CI compiles/tests the Windows-only FFI path and creates the installer.
- Operator verifies installer format, version-bearing filename, and SHA-256.

## Residual Risk

The macOS host cannot prove a real Windows route, DNS-leak behavior, or UAC
interaction. The 2.5.8 artifact is therefore a private Windows canary until a
human validates connect, external IP, DNS, reconnect, and disconnect on Windows.
WB Stream fallback remains outside this change.

## Rollback

The user can select System Proxy without uninstalling Lumen. If the canary
fails, stop the recorded elevated TUN PID, repair network state from Settings,
and reinstall the prior Windows build over the same per-user application.
