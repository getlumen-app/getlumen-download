# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Lumen, please report it responsibly:

1. **Do not** open a public GitHub issue
2. Email: security@getlumen.download
3. Include: description, steps to reproduce, potential impact

We will acknowledge receipt within 48 hours and provide a fix timeline within 7 days.

## Architecture

Lumen is a client-side VPN application. It does not store or transmit user credentials beyond the subscription key needed to fetch VPN configuration.

- **No server infrastructure in source code** — all server details are fetched at runtime from a config endpoint
- **Subscription keys** are stored locally in the app's data directory
- **sing-box** runs as a local process with system proxy settings
- **No telemetry** — Lumen does not phone home or collect usage data

## Scope

The following are in scope for security reports:

- Vulnerabilities in the Tauri/Rust backend
- Config fetch authentication bypass
- Local privilege escalation
- Data leakage from the application

The following are out of scope:

- Issues requiring physical access to the device
- Social engineering
- Denial of service against the config endpoint
- Issues in sing-box itself (report to [SagerNet/sing-box](https://github.com/SagerNet/sing-box))
