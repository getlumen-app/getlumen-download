import assert from "node:assert/strict";

import { checkReleasePath } from "./release-guard.mjs";

function response(body, init = {}) {
  return {
    ok: init.ok ?? true,
    status: init.status ?? 200,
    async text() {
      return typeof body === "string" ? body : JSON.stringify(body);
    },
    async json() {
      return typeof body === "string" ? JSON.parse(body) : body;
    },
  };
}

function assertAnonymousRequest(init = {}) {
  const headers = init.headers || {};
  const authHeader = Object.entries(headers).find(([name]) => name.toLowerCase() === "authorization");
  assert.equal(authHeader, undefined, "release guard requests must stay anonymous");
}

const goodFetch = async (url, init = {}) => {
  assertAnonymousRequest(init);
  const u = String(url);
  const goodInstaller = [
    'REPO="getlumen-app/getlumen-download"',
    "--dry-run",
    "Dry run complete. No files were installed.",
    'killall "$APP_NAME"',
    "Removing old helper, caches, and stale runtime files",
    "/Library/PrivilegedHelperTools/io.getlumen.helper",
    "$HOME/Library/Caches/io.getlumen.app",
  ].join("\n");

  if (u === "https://api.github.com/repos/getlumen-app/getlumen-download/releases/latest") {
    return response({
      tag_name: "v2.4.0",
      assets: [
        {
          name: "install.sh",
          browser_download_url: "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/install.sh",
        },
        {
          name: "Lumen_2.4.0_aarch64.dmg",
          browser_download_url:
            "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/Lumen_2.4.0_aarch64.dmg",
        },
        {
          name: "Lumen_2.4.0_x64-setup.exe",
          browser_download_url:
            "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/Lumen_2.4.0_x64-setup.exe",
        },
      ],
    });
  }
  if (u === "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/install.sh") {
    return response(goodInstaller);
  }
  if (u === "https://getlumen.download/install") {
    return response(goodInstaller);
  }
  if (u === "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/Lumen_2.4.0_aarch64.dmg") {
    assert.equal(init.method, "HEAD");
    return response("", { status: 200 });
  }
  if (u === "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/Lumen_2.4.0_x64-setup.exe") {
    assert.equal(init.method, "HEAD");
    return response("", { status: 200 });
  }
  if (u === "https://config.getlumen.download/health") {
    return response({ status: "ok", version: "1.5.0" });
  }
  throw new Error(`unexpected URL: ${u}`);
};

const good = await checkReleasePath({ expectedVersion: "2.4.0", fetchImpl: goodFetch });
assert.equal(good.ok, true);
assert.equal(good.summary.passed, good.summary.total);
assert.equal(good.release.tag, "v2.4.0");
assert.equal(
  good.release.install_url,
  "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/install.sh",
);
assert.equal(
  good.release.windows_installer_url,
  "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/Lumen_2.4.0_x64-setup.exe",
);

const missingAsset = await checkReleasePath({
  expectedVersion: "2.4.0",
  fetchImpl: async (url) => {
    const u = String(url);
    if (u.includes("api.github.com")) {
      return response({ tag_name: "v2.4.0", assets: [{ name: "install.sh" }] });
    }
    return goodFetch(url);
  },
});

assert.equal(missingAsset.ok, false);
assert.equal(
  missingAsset.checks.find((check) => check.id === "latest_release").status,
  "fail",
  "latest release check must fail when the matching desktop assets are missing",
);

const missingWindowsAsset = await checkReleasePath({
  expectedVersion: "2.4.0",
  fetchImpl: async (url) => {
    const u = String(url);
    if (u.includes("api.github.com")) {
      return response({
        tag_name: "v2.4.0",
        assets: [{ name: "install.sh" }, { name: "Lumen_2.4.0_aarch64.dmg" }],
      });
    }
    return goodFetch(url);
  },
});

assert.equal(missingWindowsAsset.ok, false);
assert.equal(
  missingWindowsAsset.checks.find((check) => check.id === "latest_release").message,
  "Lumen_2.4.0_x64-setup.exe asset is missing",
);

const versionMismatch = await checkReleasePath({
  expectedVersion: "2.4.1",
  fetchImpl: goodFetch,
});

assert.equal(versionMismatch.ok, false);
assert.equal(
  versionMismatch.checks.find((check) => check.id === "latest_release").message,
  "latest tag v2.4.0 does not match expected v2.4.1",
);

console.log("release guard tests passed");
