import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
const scripts = pkg.scripts || {};
const installer = await readFile(new URL("../install.sh", import.meta.url), "utf8");

assert.ok(scripts["release:verify"], "package.json must expose release:verify");
assert.match(
  scripts["release:verify"],
  /npm run test:release-guard/,
  "release:verify must run the mocked release guard regression tests",
);
assert.match(
  scripts["release:verify"],
  /npm run release:guard/,
  "release:verify must run the live public release-path guard",
);
assert.match(
  scripts["release:verify"],
  /npm run build/,
  "release:verify must include a frontend build gate",
);

assert.doesNotMatch(
  installer,
  /awk '\\\/\\\\\\\\\/Volumes\\\\\\\\\/\\\\\/'/,
  "installer must not use over-escaped /Volumes/ awk regex that fails on macOS awk",
);
assert.match(
  installer,
  /awk -F '\\t' 'index\(\$0, "\/Volumes\/"\) \{print \$NF; exit\}'/,
  "installer must parse the tab-separated hdiutil mount point with a macOS awk-safe /Volumes/ check",
);
assert.match(
  installer,
  /failed to attach DMG/,
  "installer must expose hdiutil attach failures instead of hiding the macOS diagnostic output",
);

console.log("release script tests passed");
