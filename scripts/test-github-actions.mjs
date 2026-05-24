import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const workflow = await readFile(new URL("../.github/workflows/release-guard.yml", import.meta.url), "utf8");

assert.match(workflow, /npm ci/, "release guard workflow must install locked dependencies");
assert.match(
  workflow,
  /npm run test:release-guard/,
  "release guard workflow must run deterministic release guard tests",
);
assert.doesNotMatch(
  workflow,
  /LITELLM|ANTHROPIC|OPENAI|GITHUB_TOKEN/,
  "release guard workflow must not depend on private LLM or repo tokens",
);

console.log("GitHub Actions release guard workflow tests passed");
