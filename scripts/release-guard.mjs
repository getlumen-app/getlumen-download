#!/usr/bin/env node
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = "getlumen-app/getlumen-download";
const DEFAULT_TIMEOUT_MS = 10000;

const URLS = {
  latestApi: `https://api.github.com/repos/${REPO}/releases/latest`,
  releaseInstall: `https://github.com/${REPO}/releases/latest/download/install.sh`,
  landingInstall: "https://getlumen.download/install",
  configHealth: "https://config.getlumen.download/health",
};

function timeoutSignal(ms) {
  if (typeof AbortSignal !== "undefined" && typeof AbortSignal.timeout === "function") {
    return AbortSignal.timeout(ms);
  }
  return undefined;
}

function pass(id, label, message, extra = {}) {
  return { id, label, status: "pass", message, ...extra };
}

function fail(id, label, message, extra = {}) {
  return { id, label, status: "fail", message, ...extra };
}

function summarize(checks) {
  const passed = checks.filter((check) => check.status === "pass").length;
  return { total: checks.length, passed, failed: checks.length - passed };
}

function parseTagVersion(tag) {
  const match = /^v?(\d+\.\d+\.\d+)$/.exec(String(tag || "").trim());
  return match ? match[1] : null;
}

function expectedDmgName(version) {
  return `Lumen_${version}_aarch64.dmg`;
}

async function fetchText(fetchImpl, url, timeoutMs) {
  const response = await fetchImpl(url, {
    headers: { "User-Agent": "lumen-release-guard" },
    signal: timeoutSignal(timeoutMs),
  });
  return { response, body: await response.text() };
}

async function fetchJson(fetchImpl, url, timeoutMs) {
  const response = await fetchImpl(url, {
    headers: {
      Accept: "application/json",
      "User-Agent": "lumen-release-guard",
    },
    signal: timeoutSignal(timeoutMs),
  });
  return { response, body: await response.json() };
}

function installerBodyOk(body) {
  return (
    body.includes(`REPO="${REPO}"`) &&
    body.includes("--dry-run") &&
    body.includes("Dry run complete. No files were installed.")
  );
}

async function checkLatestRelease({ expectedVersion, fetchImpl, timeoutMs }) {
  try {
    const { response, body } = await fetchJson(fetchImpl, URLS.latestApi, timeoutMs);
    if (!response.ok) {
      return {
        check: fail("latest_release", "GitHub latest release", `HTTP ${response.status}`, { http: response.status }),
        release: { tag: null, version: null, assets: [], dmg_url: null },
      };
    }

    const tag = body?.tag_name || null;
    const version = parseTagVersion(tag);
    const assets = Array.isArray(body?.assets) ? body.assets.map((asset) => asset?.name).filter(Boolean) : [];
    const dmgName = expectedDmgName(expectedVersion);
    const dmgUrl = `https://github.com/${REPO}/releases/download/v${expectedVersion}/${dmgName}`;
    const release = { tag, version, assets, dmg_url: dmgUrl };

    if (version !== expectedVersion) {
      return {
        check: fail(
          "latest_release",
          "GitHub latest release",
          `latest tag ${tag || "<missing>"} does not match expected v${expectedVersion}`,
          { http: response.status },
        ),
        release,
      };
    }
    if (!assets.includes("install.sh")) {
      return {
        check: fail("latest_release", "GitHub latest release", "install.sh asset is missing", { http: response.status }),
        release,
      };
    }
    if (!assets.includes(dmgName)) {
      return {
        check: fail("latest_release", "GitHub latest release", `${dmgName} asset is missing`, { http: response.status }),
        release,
      };
    }

    return {
      check: pass("latest_release", "GitHub latest release", `v${expectedVersion} has install.sh and ${dmgName}`, {
        http: response.status,
      }),
      release,
    };
  } catch (error) {
    return {
      check: fail("latest_release", "GitHub latest release", error.message, { http: null }),
      release: { tag: null, version: null, assets: [], dmg_url: null },
    };
  }
}

async function checkInstaller({ id, label, url, fetchImpl, timeoutMs }) {
  try {
    const { response, body } = await fetchText(fetchImpl, url, timeoutMs);
    if (!response.ok) return fail(id, label, `HTTP ${response.status}`, { http: response.status, url });
    if (!installerBodyOk(body)) return fail(id, label, "installer body is missing required dry-run markers", { http: response.status, url });
    return pass(id, label, "installer body has safe dry-run markers", { http: response.status, url });
  } catch (error) {
    return fail(id, label, error.message, { http: null, url });
  }
}

async function checkDmgHead({ dmgUrl, fetchImpl, timeoutMs }) {
  if (!dmgUrl) return fail("release_dmg", "GitHub release DMG", "DMG URL unavailable", { http: null, url: null });
  try {
    const response = await fetchImpl(dmgUrl, {
      method: "HEAD",
      headers: { "User-Agent": "lumen-release-guard" },
      signal: timeoutSignal(timeoutMs),
    });
    if (!response.ok) return fail("release_dmg", "GitHub release DMG", `HTTP ${response.status}`, { http: response.status, url: dmgUrl });
    return pass("release_dmg", "GitHub release DMG", "DMG asset is reachable", { http: response.status, url: dmgUrl });
  } catch (error) {
    return fail("release_dmg", "GitHub release DMG", error.message, { http: null, url: dmgUrl });
  }
}

async function checkConfigGateway({ fetchImpl, timeoutMs }) {
  try {
    const { response, body } = await fetchJson(fetchImpl, URLS.configHealth, timeoutMs);
    if (!response.ok) return fail("config_gateway", "Config gateway", `HTTP ${response.status}`, { http: response.status, url: URLS.configHealth });
    if (body?.status !== "ok") return fail("config_gateway", "Config gateway", "health status is not ok", { http: response.status, url: URLS.configHealth });
    return pass("config_gateway", "Config gateway", "health status is ok", { http: response.status, url: URLS.configHealth });
  } catch (error) {
    return fail("config_gateway", "Config gateway", error.message, { http: null, url: URLS.configHealth });
  }
}

export async function checkReleasePath({
  expectedVersion,
  fetchImpl = fetch,
  timeoutMs = DEFAULT_TIMEOUT_MS,
} = {}) {
  if (!expectedVersion) throw new Error("expectedVersion is required");

  const checks = [];
  const { check: latestCheck, release } = await checkLatestRelease({ expectedVersion, fetchImpl, timeoutMs });
  checks.push(latestCheck);
  checks.push(await checkInstaller({ id: "release_install", label: "GitHub release installer", url: URLS.releaseInstall, fetchImpl, timeoutMs }));
  checks.push(await checkInstaller({ id: "landing_install", label: "Landing installer mirror", url: URLS.landingInstall, fetchImpl, timeoutMs }));
  checks.push(await checkDmgHead({ dmgUrl: release.dmg_url, fetchImpl, timeoutMs }));
  checks.push(await checkConfigGateway({ fetchImpl, timeoutMs }));

  const summary = summarize(checks);
  return {
    ok: summary.failed === 0,
    checked_at: new Date().toISOString(),
    expected_version: expectedVersion,
    release,
    summary,
    checks,
  };
}

async function readPackageVersion() {
  const here = dirname(fileURLToPath(import.meta.url));
  const packagePath = join(here, "..", "package.json");
  const json = JSON.parse(await readFile(packagePath, "utf8"));
  return json.version;
}

function hasFlag(name) {
  return process.argv.slice(2).includes(name);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const expectedVersion = process.argv.find((arg) => arg.startsWith("--version="))?.split("=").slice(1).join("=") || await readPackageVersion();
  const result = await checkReleasePath({ expectedVersion });
  if (hasFlag("--json")) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(`Lumen release guard: ${result.ok ? "OK" : "FAIL"} (${result.summary.passed}/${result.summary.total})`);
    for (const check of result.checks) {
      const marker = check.status === "pass" ? "ok" : "fail";
      console.log(`- ${marker}: ${check.label} — ${check.message}`);
    }
  }
  process.exit(result.ok ? 0 : 1);
}
