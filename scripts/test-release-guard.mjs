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

const goodFetch = async (url, init = {}) => {
  const u = String(url);
  if (u === "https://api.github.com/repos/getlumen-app/getlumen-download/releases/latest") {
    return response({
      tag_name: "v2.4.0",
      assets: [{ name: "install.sh" }, { name: "Lumen_2.4.0_aarch64.dmg" }],
    });
  }
  if (u === "https://github.com/getlumen-app/getlumen-download/releases/latest/download/install.sh") {
    return response('REPO="getlumen-app/getlumen-download"\n--dry-run\nDry run complete. No files were installed.');
  }
  if (u === "https://getlumen.download/install") {
    return response('REPO="getlumen-app/getlumen-download"\n--dry-run\nDry run complete. No files were installed.');
  }
  if (u === "https://github.com/getlumen-app/getlumen-download/releases/download/v2.4.0/Lumen_2.4.0_aarch64.dmg") {
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
  "latest release check must fail when the matching DMG asset is missing",
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
