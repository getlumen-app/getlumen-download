import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
const scripts = pkg.scripts || {};

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

console.log("release script tests passed");
