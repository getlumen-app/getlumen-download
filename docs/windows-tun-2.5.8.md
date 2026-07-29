# Windows TUN 2.5.8

## Runtime contract

- TUN commands compile and register on Windows.
- Lumen itself stays unelevated.
- The bundled `sing-box.exe` starts through the Windows `runas` verb.
- Only the exact recorded PID is eligible for elevated stop.
- PID reuse is rejected by comparing the live process image path with the
  recorded bundled executable path.
- System Proxy is disabled and its user-mode sing-box process is stopped before
  TUN starts.
- Full subscription configs have their inbound replaced with the requested TUN
  inbound instead of silently preserving a System Proxy inbound.

## Windows TUN policy

| Field | Value |
|---|---|
| Interface | `Lumen` |
| Address | `172.19.0.1/30`, `fdfe:dcba:9876::1/126` |
| MTU | `1500` |
| Auto route | `true` |
| Strict route | `true` |
| Stack | `mixed` |

## Artifact gate

The private canary may be sent only after:

1. `cargo check --target x86_64-pc-windows-gnu --lib` is green.
2. Local Rust, frontend, connection-state, and release-guard tests are green.
3. The official `windows-latest` workflow runs Rust tests and builds NSIS.
4. The downloaded artifact is an `.exe`, has the 2.5.8 version-bearing
   filename, and has a recorded SHA-256.

Public release promotion additionally requires a real Windows check of:

1. Install over the previous per-user Lumen build without deleting user data.
2. TUN connect with UAC consent.
3. External IP changes and DNS resolves through the tunnel.
4. Reconnect after app restart.
5. Disconnect restores normal routing and leaves no recorded TUN process.

WB Stream fallback on Windows is not part of this change.
