# Windows TUN 2.6.1

Supersedes [windows-tun-2.5.8.md](windows-tun-2.5.8.md), whose canary never
passed its promotion gate and left Windows TUN permanently disabled.

## What was actually broken

1. `LUMEN_WINDOWS_TUN_CANARY=1` gated both `is_helper_installed()` and the
   privileged start path, so on every ordinary Windows install `tun_status`
   reported the runtime as absent, `isTunAvailable()` was false, and the app
   silently stayed on System Proxy. TUN was unreachable, not merely unproven.
2. sing-box was elevated per connect through `ShellExecuteEx`. The resulting
   process was an orphan: its stdout went nowhere, so a failure had no
   diagnosis, and stopping it needed a second UAC prompt for `taskkill`.
3. Success was declared on process liveness after 700 ms. That is what produced
   the 2.5.8 field report — an "active" TUN with no working internet.

`wintun.dll` was checked and ruled out: sing-box 1.11.8 carries Wintun as an
embedded resource. An unelevated run fails with `configure tun interface:
Access is denied`, never with a missing-library error.

## What 2.6.1 does instead

Follows the model the working Windows clients (v2rayN, NekoRay, Hiddify) use:
the *app* holds the privilege, and the core is an ordinary child process.

- **Unlock by elevation, not by a flag.** `tun_install_helper` on Windows
  validates the bundled sing-box and, when Lumen is not elevated, relaunches
  Lumen through the `runas` verb and exits the current instance. A cancelled
  UAC prompt leaves the session untouched.
- **sing-box is a child of Lumen.** `stdout`/`stderr` are redirected to
  `singbox-tun.log` in the Lumen data directory, and disconnect terminates the
  recorded PID directly — no second prompt. Elevated orphans from 2.5.8 are
  still handled by the taskkill fallback.
- **Readiness is proven by traffic.** After start, Lumen requests
  `cloudflare.com/cdn-cgi/trace` (then the config health endpoint) for up to
  25 s. Only a successful response reports "connected". Otherwise sing-box is
  killed, the PID record is cleared, normal routing returns, and the error
  carries the last sing-box log lines.
- **`gvisor` netstack on Windows** — what v2rayN and Clash Verge ship there.
  The system TCP stack behind the previous `mixed` setting is the fragile one
  on a Wintun adapter. macOS keeps `mixed`.

## A dead exit pin must not be able to black out TUN

Found on the first real 2.6.1 connect. sing-box persists Clash selector choices
in `cache.db`, so a manual exit pin outlives the process that made it — here a
`relay-eu-443` pin from an earlier session. `dns-proxy` detours through that
same selector, and TUN's `hijack-dns` captures every system lookup, so one
unreachable exit took all name resolution with it:

```
outbound/vless[relay-eu-443]: outbound multiplex connection to 1.1.1.1:443
dns: exchange failed for www.cloudflare.com. IN A: context deadline exceeded
```

System Proxy hides this class of failure — DNS never enters sing-box there, so
apps keep resolving through the OS and only proxied traffic suffers. TUN is
where a dead pin becomes total.

Recovery is now part of the readiness gate. Nine seconds of silence drops the
pin back to `proxy-auto` (`clash_api::reset_selector_to_auto`) and the probe
continues on the remaining budget. `connect` does the same for System Proxy, so
both transports recover identically. A successful recovery raises
`lumen://exit-pin-reset`; the shell must clear its stored location on that
event, otherwise the next proxy poll re-applies the pin and takes the recovered
session straight back down.

Latency numbers cannot catch this: `test_delay` times a bare TCP connect, and
per the field note in `config.rs` the DPI that kills these exits passes small
probes while throttling sustained traffic. The pinned exit read 11 ms while
moving zero bytes.

## Windows TUN policy

| Field | Value |
|---|---|
| Interface | `Lumen` |
| Address | `172.19.0.1/30`, `fdfe:dcba:9876::1/126` |
| MTU | `1500` |
| Auto route | `true` |
| Strict route | `true` |
| Stack | `gvisor` |

## Invariants covered by tests

`scripts/test-windows-tun.mjs` (source contract) and
`cargo test --release --lib`:

- every TUN command is registered on Windows;
- the canary flag is gone;
- the start path fails closed without an administrator token;
- readiness is verified with real traffic and a failed start tears itself down;
- the TUN session writes a log that the Logs screen can show;
- Windows policy pins `gvisor`.

## Promotion gate

Unchanged from 2.5.8, and still requires a real Windows check:

1. Install over the previous per-user build without deleting user data.
2. Enable TUN Mode → one UAC prompt → Lumen restarts elevated.
3. Connect: external IP changes and DNS resolves through the tunnel.
4. Disconnect with no second prompt; routing returns to normal.
5. Reconnect after app restart.
6. A deliberately broken exit must surface an error and restore internet
   instead of leaving a dead tunnel installed.
