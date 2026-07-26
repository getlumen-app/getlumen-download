export type ConnectionState = "disconnected" | "connecting" | "connected" | "error";
export type ActiveTransport = "tun" | "proxy" | "wbstream" | null;
export type StoredConnectionIntent = "connected" | "disconnected" | "unknown";

export const CONNECTION_INTENT_KEY = "lumen-connection-intent";

export interface TunStatusLike {
  helper_installed: boolean;
  helper_running: boolean;
  singbox_running: boolean;
  singbox_pid: number | null;
  uptime_secs: number | null;
}

export interface RepairNetworkResultLike {
  proxy_was_running: boolean;
  tun_was_running: boolean;
  proxy_stopped: boolean;
  tun_stopped: boolean;
  errors: string[];
}

export interface NetworkDiagnosticsLike {
  effective_status: string;
  helper_installed?: boolean;
  helper_running?: boolean;
  tun_running?: boolean;
  external_ip: string | null;
  region: string | null;
  country: string | null;
  asn_org: string | null;
  error: string | null;
}

export function transportFromEffectiveStatus(status: string): ActiveTransport {
  if (status === "connected-tun") return "tun";
  if (status === "connected-proxy") return "proxy";
  return null;
}

export function shouldStopTunOnDisconnect(
  activeTransport: ActiveTransport,
  preferredTun: boolean,
  tunStatus: TunStatusLike | null
): boolean {
  return (
    activeTransport === "tun" ||
    activeTransport === "wbstream" ||
    preferredTun ||
    !!tunStatus?.singbox_running
  );
}

export function readStoredConnectionIntent(value: string | null): StoredConnectionIntent {
  if (value === null) return "unknown";
  return value === "connected" ? "connected" : "disconnected";
}

export function shouldSelfHealOnLaunch(
  storedIntent: StoredConnectionIntent,
  activeTransport: ActiveTransport
): boolean {
  return storedIntent === "disconnected" && activeTransport !== null;
}

export function repairNetworkMessage(result: RepairNetworkResultLike): string {
  if (result.errors.length > 0) return "Repair needs attention";
  if (result.proxy_stopped || result.tun_stopped) return "Network repaired";
  if (result.proxy_was_running || result.tun_was_running) return "Network checked";
  return "Network already clean";
}

export function diagnosticRouteLabel(status: string): string {
  if (status === "connected-tun") return "TUN";
  if (status === "connected-proxy") return "System Proxy";
  if (status === "connected-wbstream") return "WB Stream";
  return "Direct";
}

/**
 * Connect-time WB Stream auto-fallback gate.
 *
 * Hard-whitelist carrier must NOT run on control-plane / local failures
 * (blocked config host, missing cache, bad key, helper down). Those are
 * handled by cache-first Proteus connect. WB Stream is reserved for the
 * post-connect health-monitor path when TUN is up but exits look dead.
 */
export function shouldAttemptWbstreamOnConnectError(errorMsg: string): boolean {
  const msg = String(errorMsg || "").toLowerCase();
  if (!msg) return false;
  if (msg.includes("config fetch failed")) return false;
  if (msg.includes("no usable cached config")) return false;
  if (msg.includes("vless parse failed")) return false;
  if (msg.includes("config build failed")) return false;
  if (msg.includes("helper")) return false;
  if (msg.includes("permission")) return false;
  // Connect-time default: do not jump. Health monitor owns WB switch.
  return false;
}

export function diagnosticLocationLabel(diagnostics: NetworkDiagnosticsLike): string {
  if (diagnostics.error) return "Unavailable";
  const parts = [diagnostics.region, diagnostics.country].filter(Boolean);
  return parts.length > 0 ? parts.join(", ") : "Unknown";
}

export function formatDiagnosticsSnapshot(
  diagnostics: NetworkDiagnosticsLike,
  generatedAt: Date = new Date()
): string {
  const lines = [
    "Lumen diagnostics",
    `Time: ${generatedAt.toISOString()}`,
    `Route: ${diagnosticRouteLabel(diagnostics.effective_status)}`,
    `External IP: ${diagnostics.external_ip ?? "Unknown"}`,
    `Location: ${diagnosticLocationLabel(diagnostics)}`,
    `Provider: ${diagnostics.asn_org ?? "Unknown"}`,
    `Helper: ${diagnostics.helper_running ? "Running" : diagnostics.helper_installed ? "Installed" : "Not installed"}`,
    `TUN: ${diagnostics.tun_running ? "Running" : "Stopped"}`,
  ];
  if (diagnostics.error) lines.push(`Diagnostics error: ${diagnostics.error}`);
  return lines.join("\n");
}
