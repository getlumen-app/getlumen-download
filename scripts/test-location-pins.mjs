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
  /\{\s*tag:\s*"relay-eu-httpupgrade",\s*label:\s*"Germany/,
  "the Home Germany pin must use the live Timeweb HTTPUpgrade relay"
);
assert.doesNotMatch(
  homeOptions,
  /tag:\s*"relay-eu-443"/,
  "relay-eu-443 must not be exposed as the Home Germany pin"
);
// Decommissioned inbounds (measured TCP-closed 2026-09-15): Timeweb :36743
// (relay-eu-grpc) + :36745 (proxy-moscow), FirstByte :36743
// (firstbyte-moscow-reality). Dead pins must never reach the sheet.
for (const dead of ["relay-eu-grpc", "proxy-moscow", "firstbyte-moscow-reality"]) {
  assert.doesNotMatch(
    homeOptions,
    new RegExp(`tag:\\s*"${dead}"`),
    `${dead} is a dead inbound and must not be pinnable`
  );
}

const geoSelector = config.slice(
  config.indexOf("const GEO_SELECTOR_TAGS"),
  config.indexOf("const AUTO_EXCLUDED_GEO_TAGS")
);
assert.match(
  geoSelector,
  /"relay-eu-httpupgrade"/,
  "the generated selector must expose relay-eu-httpupgrade for Germany"
);
assert.doesNotMatch(
  geoSelector,
  /"relay-eu-443"/,
  "the generated selector must not expose relay-eu-443 as a geo pin"
);
for (const dead of ["relay-eu-grpc", "proxy-moscow", "firstbyte-moscow-reality"]) {
  assert.doesNotMatch(
    geoSelector,
    new RegExp(`"${dead}"`),
    `${dead} is a dead inbound and must not be in the geo selector`
  );
}

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
