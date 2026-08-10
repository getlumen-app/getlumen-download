import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const locations = await readFile(new URL("../src/lib/locations.ts", import.meta.url), "utf8");
const config = await readFile(new URL("../src-tauri/src/config.rs", import.meta.url), "utf8");

const homeOptions = locations.slice(
  locations.indexOf("export const LOCATION_OPTIONS"),
  locations.indexOf("const BY_TAG")
);
assert.match(
  homeOptions,
  /\{\s*tag:\s*"relay-eu-grpc",\s*label:\s*"Germany/,
  "the Home Germany pin must use the non-443 gRPC relay"
);
assert.doesNotMatch(
  homeOptions,
  /tag:\s*"relay-eu-443"/,
  "relay-eu-443 must not be exposed as the Home Germany pin"
);

const geoSelector = config.slice(
  config.indexOf("const GEO_SELECTOR_TAGS"),
  config.indexOf("const AUTO_EXCLUDED_GEO_TAGS")
);
assert.match(
  geoSelector,
  /"relay-eu-grpc"/,
  "the generated selector must expose relay-eu-grpc for Germany"
);
assert.doesNotMatch(
  geoSelector,
  /"relay-eu-443"/,
  "the generated selector must not expose relay-eu-443 as a geo pin"
);

const autoExcluded = config.slice(
  config.indexOf("const AUTO_EXCLUDED_GEO_TAGS"),
  config.indexOf("fn prioritize_hostodo_firstbyte")
);
assert.match(
  autoExcluded,
  /"relay-eu-443"/,
  "relay-eu-443 must stay out of Auto urltests"
);

console.log("location pin contract tests passed");
