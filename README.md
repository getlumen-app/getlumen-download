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

## Features

- One-tap VPN connection via sing-box core
- Auto-select best server (urltest) or manual switch
- Proxy list with country flags, latency, collapsible groups
- Light theme (Notion-inspired) / Dark theme (Cursor-inspired)
- System theme detection, manual override in Settings
- Config auto-fetch from subscription server
- Offline mode with cached config
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

GPL-3.0 — see [LICENSE](LICENSE)
