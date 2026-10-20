// install.sh must pick the newest release that actually carries the macOS DMG.
// Origin: from 01.10 to 20.10 the Android release was the repo's "latest", and the
// installer — which read releases/latest — exited 1 with NO message for every Mac.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const INSTALL = join(dirname(fileURLToPath(import.meta.url)), "..", "install.sh");

const asset = (name) => ({ name, browser_download_url: `https://github.com/getlumen-app/getlumen-download/releases/download/x/${name}` });

function run(releases, arch = "arm64") {
  const bin = mkdtempSync(join(tmpdir(), "lumen-install-test-"));
  const fixture = join(bin, "releases.json");
  writeFileSync(fixture, JSON.stringify(releases, null, 2));
  // Like the real API: /releases/latest is ONE object (the newest), /releases is the list.
  const latest = join(bin, "latest.json");
  writeFileSync(latest, JSON.stringify(releases[0] ?? {}, null, 2));
  // Fake curl: the releases API gets the fixture; anything else (the DMG) fails,
  // so the run stops right after the installer announces what it picked.
  writeFileSync(join(bin, "curl"), `#!/bin/bash
out=""; url=""
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift 2;; -*) shift;; *) url="$1"; shift;; esac; done
case "$url" in
  https://api.github.com/repos/*/releases/latest) cp "${latest}" "$out" ;;
  https://api.github.com/repos/*/releases*) cp "${fixture}" "$out" ;;
  *) exit 22 ;;
esac
`);
  writeFileSync(join(bin, "uname"), `#!/bin/bash\ncase "$1" in -s) echo Darwin ;; *) echo ${arch} ;; esac\n`);
  chmodSync(join(bin, "curl"), 0o755);
  chmodSync(join(bin, "uname"), 0o755);
  return spawnSync("bash", [INSTALL, "--dry-run"], {
    encoding: "utf8",
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}` },
  });
}

const android = { tag_name: "android-v1.0.1", assets: [asset("Lumen-1.0.1-android.apk")] };
const desktop = (v) => ({
  tag_name: `v${v}`,
  assets: [asset("install.sh"), asset(`Lumen_${v}_aarch64.dmg`), asset(`Lumen_${v}_x64-setup.exe`)],
});

// 1. Android release is newest: the installer must skip it, not die.
{
  const r = run([android, desktop("2.6.8"), desktop("2.6.7")]);
  assert.match(r.stdout, /Downloading: Lumen_2\.6\.8_aarch64\.dmg/, `picked wrong release:\n${r.stdout}\n${r.stderr}`);
}

// 2. Desktop release is newest: unchanged behaviour.
{
  const r = run([desktop("2.6.10"), android, desktop("2.6.8")]);
  assert.match(r.stdout, /Downloading: Lumen_2\.6\.10_aarch64\.dmg/);
}

// 3. No DMG anywhere: fail LOUDLY with a reason, never a silent exit 1.
{
  const r = run([android]);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /could not find aarch64 DMG/, `silent failure:\n${r.stderr}`);
}

console.log("installer release pick: 3/3 ok");
