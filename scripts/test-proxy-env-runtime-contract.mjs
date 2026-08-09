import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (p) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

const lib = read("src-tauri/src/lib.rs");

{
  const start = lib.indexOf("fn read_launchd_env(");
  assert.notEqual(start, -1, "read_launchd_env must exist");
  const end = lib.indexOf("\nfn has_lumen_proxy_env", start);
  assert.notEqual(end, -1, "has_lumen_proxy_env must follow read_launchd_env");
  const body = lib.slice(start, end);

  assert.match(
    body,
    /output\.stdout\.is_empty\(\)/,
    "launchctl getenv must distinguish an unset key by zero stdout bytes"
  );
  assert.doesNotMatch(
    body,
    /if\s+value\.is_empty\(\)\s*\{\s*None/s,
    "an empty launchd variable is stdout '\\n', not the same as unset"
  );
}

{
  const start = lib.indexOf("fn heal_stale_lumen_proxy_env(");
  assert.notEqual(start, -1, "heal_stale_lumen_proxy_env must exist");
  const end = lib.indexOf("\n#[tauri::command]", start);
  assert.notEqual(end, -1, "a tauri command must follow heal_stale_lumen_proxy_env");
  const body = lib.slice(start, end);

  assert.match(
    body,
    /SingboxManager::local_proxy_listener_ready\(\)/,
    "startup heal must reuse the sing-box listener readiness helper"
  );
  assert.doesNotMatch(
    body,
    /TcpStream::connect_timeout/,
    "startup heal must not duplicate listener probing"
  );
}

{
  const start = lib.indexOf("async fn repair_network(");
  assert.notEqual(start, -1, "repair_network command must exist");
  const end = lib.indexOf("\n#[tauri::command]\nasync fn get_proxies", start);
  assert.notEqual(end, -1, "get_proxies must follow repair_network");
  const body = lib.slice(start, end);

  assert.match(
    body,
    /for key in clear_lumen_proxy_env\(\)/,
    "repair_network must inspect variables that survived cleanup"
  );
  assert.match(
    body,
    /proxy env \{\} still set in the launchd domain/,
    "repair_network must surface surviving launchd proxy keys to the UI"
  );
}

console.log("proxy-env runtime contract: ok");
