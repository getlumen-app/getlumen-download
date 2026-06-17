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

for (const required of [
  'killall "$APP_NAME"',
  "killall sing-box",
  "launchctl bootout system/io.getlumen.helper",
  "/Library/LaunchDaemons/io.getlumen.helper.plist",
  "/Library/PrivilegedHelperTools/io.getlumen.helper",
  "$HOME/Library/Caches/io.getlumen.app",
]) {
  assert.match(installer, new RegExp(required.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")), `install.sh must hard-clean ${required}`);
}

console.log("release script tests passed");
