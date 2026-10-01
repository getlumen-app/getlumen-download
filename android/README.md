# Lumen for Android

Android build of Lumen: VpnService + sing-box + whitelist fallback through
Telemost joiner rooms, same architecture as the desktop client.

## Requirements

- JDK 17, Android SDK (platform android-36, build-tools), Android NDK is
  only required to rebuild `app/libs/mobile.aar` (gomobile tun2socks) —
  the committed AAR and `jniLibs` binaries are used as-is.
- Go 1.24+ to rebuild the bundled binaries.

## Build

```bash
cd android
./gradlew assembleRelease
# unsigned when LUMEN_KEYSTORE_* env vars are unset; otherwise signed
```

APK lands at `app/build/outputs/apk/release/app-release.apk` — arm64-v8a only (the
Telegram Bot API upload cap is 50MB), sideloadable,
Google Play not required.

## Bundled native binaries

- `app/src/main/jniLibs/*/libsingbox.so` — sing-box v1.14.0, built with
  `GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -tags "with_utls,with_quic,with_clash_api" -trimpath -ldflags="-s -w" ./cmd/sing-box`.
  Executed from
  `nativeLibraryDir`, which is the only app-writable path Android lets
  you `exec()` from; `android:extractNativeLibs="true"` is required.
  One source patch (`route/network.go`): when `auto_detect_interface` is off,
  a permission-denied `netlink.RouteSubscribe` downgrades to a warning —
  inside an app sandbox RTMGRP multicast always fails (no CAP_NET_ADMIN)
  while the one-shot interface dump keeps working. Remote rule-set TLS works
  because `SingBoxRunner` sets `SSL_CERT_DIR=/system/etc/security/cacerts`.
- `app/src/main/jniLibs/*/librelay.so` — headless joiner relay built from
  the same source the desktop Telemost joiner uses.
- `app/libs/mobile.aar` — gomobile bindings (tun2socks engine).

## Runtime architecture

1. The Proteus key is fetched as a full sing-box config
   (`config.getlumen.download/proteus-sub?sub=<key>&format=json-text`).
2. The signed Telemost room manifest is fetched from
   `config.getlumen.download/telemost-manifest.json` (or taken from the
   config's embedded `telemost_manifest`), verified with an embedded RS256
   public key, and cached in the app files dir.
3. `ConfigTransform` strips unusable `tun` inbounds, injects the
   `telemost-local` SOCKS outbound (127.0.0.1:11097) into the
   `whitelist-auto` urltest, and pins Telemost domains to `direct`.
4. A health monitor probes the data plane through the local mixed port;
   after two consecutive failures it spawns `librelay.so
   --mode telemost-headless-joiner`, waits for `TUNNEL_CONNECTED` and runs a
   real SOCKS CONNECT + response-byte probe before the joiner counts as a
   fallback. urltest handles the switch in both directions without breaking
   live connections.
5. The app package itself is disallowed from the VPN builder, so sing-box
   and joiner traffic cannot loop back through the tunnel.

## Tests

```bash
./gradlew testDebugUnitTest
```

Covers the config transform (injection, group synthesis, tun stripping) and
the manifest gate (shape, payload/signature binding, seeded ordering).
