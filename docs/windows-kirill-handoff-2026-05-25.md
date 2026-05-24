# Windows Kirill Handoff - 2026-05-25

## Current state

- `main` contains the Windows release guard work from PR #1 and PR #2.
- GitHub Actions `Release guard` is green on `main`.
- GitHub Release `v2.4.0` contains:
  - `install.sh`
  - `Lumen_2.4.0_aarch64.dmg`
  - `Lumen_2.4.0_x64-setup.exe`
- `npm run release:guard -- --json` passes 6/6 against live release endpoints.
- Kirill's Windows machine had Lumen 2.4.0 installed into:
  - `C:\Users\kiril\AppData\Local\Lumen`
- Bundled Windows `sing-box.exe` was verified remotely:
  - path: `C:\Users\kiril\AppData\Local\Lumen\_up_\bin\sing-box.exe`
  - version output: `sing-box version 1.11.8`, `windows/amd64`

## Fixes landed

- Windows CI now builds an NSIS installer artifact on `windows-latest`.
- Release guard now requires the Windows installer asset for the expected version.
- Release guard uses resolved GitHub release `browser_download_url` values instead of relying on the fragile `releases/latest/download/*` redirect path.
- Windows `sing-box` resolution now checks the Tauri `_up_\bin` resource layout before app root fallback.
- `src-tauri/tauri.conf.json` resources are packaged from `../bin` so platform resources can be included without requiring every platform binary to exist locally.

## Known gap

Full WB Stream fallback on Windows is not complete yet.

The installed package contained:

- `sing-box.exe`
- `wbstream_multipath_client.exe`
- `headless-wbstream-joiner` without a Windows `.exe`

Next implementation layer should produce/package a real Windows `headless-wbstream-joiner.exe`, update lookup/tests, then run an end-to-end connect/fallback test on Kirill's machine.

## Remote access cleanup

The Windows reverse SSH tunnel used for this session was temporary and must not remain active overnight.

Cleanup checklist for the end of this session:

- remove the temporary Kirill tunnel public key from TimeWeb `authorized_keys`
- kill the TimeWeb reverse tunnel listener on `127.0.0.1:9938`
- delete local temporary tunnel keys and installer artifacts under `/tmp`
- verify `ss -ltnp | grep 9938` returns no listener on TimeWeb

Tomorrow, create a fresh temporary tunnel/key if remote access is needed again.

## Suggested next task

Build the Windows WB Stream joiner packaging path:

1. Find or create the Windows build target for `headless-wbstream-joiner.exe`.
2. Add a deterministic test that Windows lookup prefers `headless-wbstream-joiner.exe` in `_up_\bin`.
3. Add the joiner to the Windows Actions sidecar preparation step.
4. Build a new NSIS artifact.
5. Install it on Kirill's Windows machine.
6. Run a real WB Stream fallback/connect proof and capture logs.
