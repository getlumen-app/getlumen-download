import { invoke } from "@tauri-apps/api/core";

const IS_TAURI = "__TAURI_INTERNALS__" in window;

export async function fetchConfig(key: string): Promise<string> {
  if (!IS_TAURI) {
    await new Promise((r) => setTimeout(r, 500));
    return JSON.stringify({ mock: true });
  }
  return invoke<string>("fetch_config", { key });
}

export async function connect(key: string): Promise<void> {
  if (!IS_TAURI) return;
  return invoke("connect", { key });
}

export async function disconnect(): Promise<void> {
  if (!IS_TAURI) return;
  return invoke("disconnect");
}

export async function getStatus(): Promise<string> {
  if (!IS_TAURI) return "disconnected";
  return invoke<string>("get_status");
}

export async function getProxies(): Promise<Record<string, unknown> | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<Record<string, unknown>>("get_proxies");
  } catch {
    return null;
  }
}

export async function selectProxy(group: string, name: string): Promise<void> {
  if (!IS_TAURI) return;
  return invoke("select_proxy", { group, name });
}

export async function getTraffic(): Promise<{ up: number; down: number }> {
  if (!IS_TAURI) return { up: 0, down: 0 };
  try {
    return await invoke("get_traffic");
  } catch {
    return { up: 0, down: 0 };
  }
}

export async function testDelay(name: string): Promise<number> {
  if (!IS_TAURI) return Math.floor(Math.random() * 100) + 10;
  return invoke<number>("test_delay", { name });
}

// TUN mode (via privileged helper, macOS only)
interface TunStatus {
  helper_installed: boolean;
  helper_running: boolean;
  singbox_running: boolean;
  singbox_pid: number | null;
  uptime_secs: number | null;
}

export async function tunStatus(): Promise<TunStatus | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<TunStatus>("tun_status");
  } catch {
    return null;
  }
}

export async function isTunAvailable(): Promise<boolean> {
  const s = await tunStatus();
  return !!(s && s.helper_installed && s.helper_running);
}

export async function tunConnect(key: string): Promise<number> {
  if (!IS_TAURI) return 0;
  return invoke<number>("tun_connect", { key });
}

export async function tunDisconnect(): Promise<void> {
  if (!IS_TAURI) return;
  return invoke("tun_disconnect");
}

export async function openUrl(url: string): Promise<void> {
  if (!IS_TAURI) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  try {
    await invoke("open_url", { url });
  } catch {
    window.open(url, "_blank", "noopener,noreferrer");
  }
}

export { IS_TAURI };
