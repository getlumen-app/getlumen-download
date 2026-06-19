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

export async function getEffectiveStatus(): Promise<string> {
  if (!IS_TAURI) return "disconnected";
  return invoke<string>("get_effective_status");
}

export interface RepairNetworkResult {
  proxy_was_running: boolean;
  tun_was_running: boolean;
  proxy_stopped: boolean;
  tun_stopped: boolean;
  errors: string[];
}

export interface NetworkDiagnostics {
  effective_status: string;
  helper_installed: boolean;
  helper_running: boolean;
  tun_running: boolean;
  external_ip: string | null;
  region: string | null;
  country: string | null;
  asn_org: string | null;
  error: string | null;
}

export interface BootstrapImportResult {
  id: string;
  name: string;
  key_type: "vless";
  value: string;
  preferred_mode: "proxy" | "tun";
  full_config_url?: string | null;
}

export async function importBootstrapPayload(payload: string): Promise<BootstrapImportResult> {
  if (!IS_TAURI) {
    return {
      id: "bootstrap-imported",
      name: "Bootstrap profile",
      key_type: "vless",
      value: payload,
      preferred_mode: "proxy",
    };
  }
  return invoke<BootstrapImportResult>("import_bootstrap_payload", { payload });
}

export async function repairNetwork(): Promise<RepairNetworkResult> {
  if (!IS_TAURI) {
    return {
      proxy_was_running: false,
      tun_was_running: false,
      proxy_stopped: false,
      tun_stopped: false,
      errors: [],
    };
  }
  return invoke<RepairNetworkResult>("repair_network");
}

export async function networkDiagnostics(): Promise<NetworkDiagnostics> {
  if (!IS_TAURI) {
    return {
      effective_status: "disconnected",
      helper_installed: false,
      helper_running: false,
      tun_running: false,
      external_ip: null,
      region: null,
      country: null,
      asn_org: null,
      error: null,
    };
  }
  return invoke<NetworkDiagnostics>("network_diagnostics");
}

export async function internetHealthProbe(): Promise<boolean> {
  if (!IS_TAURI) return true;
  try {
    return await invoke<boolean>("internet_health_probe");
  } catch {
    return false;
  }
}

export async function healthMonitorDecision(
  transport: "tun" | "proxy" | "wbstream",
  previousFailures: number,
  probeOk: boolean
): Promise<{ consecutive_failures: number; action: "stay" | "switch_to_wbstream" }> {
  if (!IS_TAURI) {
    const consecutive_failures = probeOk ? 0 : previousFailures + 1;
    return {
      consecutive_failures,
      action: transport === "tun" && consecutive_failures >= 2 ? "switch_to_wbstream" : "stay",
    };
  }
  return invoke("health_monitor_decision", {
    transport,
    previousFailures,
    probeOk,
  });
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

export async function tunConnectWbstreamFallback(): Promise<number> {
  if (!IS_TAURI) return 0;
  return invoke<number>("tun_connect_wbstream_fallback");
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
