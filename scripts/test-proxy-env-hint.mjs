import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (p) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

const tauriHook = read("src/hooks/useTauri.ts");
const app = read("src/App.tsx");
const home = read("src/pages/Home.tsx");

{
  assert.match(
    tauriHook,
    /export interface DisconnectOutcome\b/,
    "useTauri must type the disconnect outcome"
  );
  assert.match(
    tauriHook,
    /proxy_env_cleared/,
    "disconnect outcome must carry whether a proxy env was inherited"
  );
  const start = tauriHook.indexOf("export async function disconnect(");
  assert.notEqual(start, -1, "useTauri must export disconnect");
  const body = tauriHook.slice(start, tauriHook.indexOf("\nexport ", start + 1));
  assert.match(
    body,
    /Promise<DisconnectOutcome>/,
    "disconnect must return the outcome, not void"
  );
  assert.match(
    body,
    /proxy_env_cleared:\s*false/,
    "the non-Tauri browser path must still return a well-formed outcome"
  );
}

{
  assert.match(app, /restartHint/, "App must track whether a restart hint is owed");
  const start = app.indexOf("async function tearDownSession()");
  assert.notEqual(start, -1, "App must define tearDownSession");
  const body = app.slice(start, app.indexOf("\n  async function", start + 1));
  assert.match(
    body,
    /proxy_env_cleared/,
    "teardown must read the disconnect outcome to decide the hint"
  );
}

{
  assert.match(home, /restartHint\?:\s*boolean/, "Home must accept the hint prop");
  assert.match(
    home,
    /restartHint && connectionState === "disconnected"/,
    "the hint belongs to the disconnected state only"
  );
  assert.match(
    home,
    /приложения, запущенные при включённом VPN, могут требовать перезапуска/,
    "the hint text must use the approved soft restart warning"
  );
  assert.doesNotMatch(
    home,
    /ps eww|process list|scan your apps/i,
    "the hint must not claim Lumen inspects other processes"
  );
}

console.log("proxy-env restart hint: ok");
