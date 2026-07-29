# Lumen

Open-source VPN client for macOS and Windows. Built with Tauri 2, React, and sing-box.

## Install (macOS)

### One-liner

```bash
curl -sL https://github.com/getlumen-app/getlumen-download/releases/latest/download/install.sh | bash
```

### Manual

1. Download the latest `.dmg` from [Releases](https://github.com/getlumen-app/getlumen-download/releases)
2. Drag `Lumen.app` to `/Applications`
3. Open Terminal and run: `xattr -cr /Applications/Lumen.app`
4. Launch Lumen

> **Note:** The app is not code-signed. The `xattr` command removes the macOS quarantine flag so it can run.

## Usage

1. Enter your subscription key
2. Press Connect
3. Lumen auto-selects the best server

### Offline bootstrap profile

Lumen `v2.5.6+` supports a per-user bootstrap payload for hostile networks where
GitHub, cloud drive mirrors, or the Lumen config endpoint are blocked during a
clean install. The app does not ship with a shared VPN profile. Instead, an
operator or ProteusKeyBot generates a payload for one user account:

```json
{
  "schema_version": "lumen.bootstrap.v1",
  "name": "User bootstrap",
  "vless": "vless://USER_UUID@HOST:443?...#User",
  "preferred_mode": "proxy"
}
```

For messenger copy/paste, URL-encode the JSON and prefix it with
`lumen-bootstrap-v1:`. The first Lumen screen and Settings both accept this
payload. Import validates the VLESS link, prebuilds local proxy/TUN configs, and
sets the imported VLESS profile as active without fetching the control-plane
config endpoint. If the payload leaks, only that user's VLESS/UUID must be
revoked; there is no global embedded profile in the binary.

## Features

- One-tap VPN connection via sing-box core
- Auto-select best server (urltest) or manual switch
- Proxy list with country flags, latency, collapsible groups
- Light theme / Dark theme
- System theme detection, manual override in Settings
- Config auto-fetch from subscription server
- Offline mode with cached config
- Per-user offline bootstrap import for clean installs on blocked networks
- Smart split-tunneling (.ru domains direct, everything else via proxy)

## Architecture

```
Lumen.app (Tauri 2.x)
├── React + TypeScript UI
│   ├── Home — connect button, speed, timer
│   ├── Proxies — server list with groups & flags
│   └── Settings — theme, key, connection options
├── Rust backend
│   ├── Config Manager — fetch, cache, refresh
│   ├── sing-box Process Manager — start/stop/health
│   └── Clash API Client — proxies, traffic, delay test
└── sing-box binary (bundled)
    ├── Multi-server auto-select (urltest)
    ├── Smart split-tunneling
    └── Clash API on localhost:9090
```

## Development

### Prerequisites

- [Node.js](https://nodejs.org/) 20+
- [Rust](https://rustup.rs/) (stable)
- [sing-box](https://github.com/SagerNet/sing-box) v1.11.8 binary in `bin/`

### Build sing-box from source

```bash
cd /tmp
git clone --depth 1 --branch v1.11.8 https://github.com/SagerNet/sing-box.git
cd sing-box
go build -tags "with_quic,with_utls,with_clash_api,with_gvisor" -o sing-box-bin ./cmd/sing-box
mkdir -p <project>/bin && cp sing-box-bin <project>/bin/sing-box
```

### Dev mode

```bash
npm install
npm run tauri dev
```

### Production build

```bash
npm run tauri build
# Output: src-tauri/target/release/bundle/dmg/Lumen_*.dmg
```

### Windows release build

The GitHub Actions `build-windows` job builds the NSIS installer on
`windows-latest`. It downloads `sing-box.exe`, builds
`wbstream_multipath_client.exe`, copies both into `bin/`, runs the Rust tests
against the Windows target, then runs:

```bash
npm run tauri -- build --bundles nsis
```

Lumen `v2.5.8+` supports real Windows TUN mode. The UI remains a normal
per-user process; Windows shows a UAC prompt only when the bundled
`sing-box.exe` TUN runtime starts or stops. Lumen records the exact elevated
PID and executable path so cleanup never uses an unscoped process-name kill.
The Windows TUN policy uses MTU 1500, automatic routing, and strict routing to
reduce DNS leaks.

WB Stream hard-whitelist fallback on Windows still needs a Windows build of
`headless-wbstream-joiner.exe`; until that sidecar exists, Windows builds are
for normal System Proxy and TUN validation, not WB Stream fallback.

### Release guard

Before announcing a release, verify the public install path:

```bash
npm run release:verify
```

This runs the release guard tests, connection-state tests, frontend build, and
the live public release-path guard. The guard checks the latest GitHub release
tag, `install.sh`, the matching macOS DMG asset, the landing installer mirror,
and the config gateway health. It exits non-zero if any public install path is
stale or missing.

### macOS install hygiene

After local install/release testing, keep Finder/Spotlight unambiguous:

```bash
npm run macos:app-hygiene -- --fix
```

This archives old `/Applications/Lumen.app.backup-*` bundles, removes the
generated Tauri `src-tauri/target/.../Lumen.app` bundle, refreshes
LaunchServices, and verifies that macOS exposes only `/Applications/Lumen.app`.

### WB Stream account pool groundwork

The client includes a deterministic WB Stream account-pool model in
`src-tauri/src/wbstream_accounts.rs`. It is intentionally secret-free: tests use
a fake provider that simulates healthy accounts, refreshable sessions, forced
reauth, rate limits, disabled accounts, cooldowns, and room allocation.

Before adding real volunteered accounts, keep the real provider behind the same
contract:

- store cookies/session material only in a server-side encrypted vault;
- never commit or log raw cookies, phone numbers, WB ids, or owner names;
- refresh only normal valid-session cookies/tokens automatically;
- mark accounts as `needs_reauth` when WB requires SMS/OTP/CAPTCHA/manual login;
- rotate rooms across healthy accounts and exclude rate-limited/revoked accounts.

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `LUMEN_CONFIG_URL` | `https://config.getlumen.download` | Config server endpoint (compile-time) |

## Design

| Token | Light | Dark |
|---|---|---|
| Background | `#FFFFFF` | `#181818` |
| Surface | `#F7F6F3` | `#1D1D1D` |
| Text | `#37352F` | `#D6D6DD` |
| Accent | `#0075DE` | `#228DF2` |
| Connected | `#448361` | `#15AC91` |

Font: Inter (UI) + JetBrains Mono (data).

## Tech Stack

| Component | Technology |
|---|---|
| UI | React 19 + TypeScript |
| Desktop | Tauri 2.x (Rust) |
| Build | Vite 8 |
| VPN Core | sing-box v1.11.8 |
| Proxy API | Clash API (localhost) |

## License

GPL-3.0.
