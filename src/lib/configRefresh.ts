/// Settings → Refresh Config: user-facing wording for the refresh result and
/// the "Config updated" timestamp. Pure so the wording is testable.

export interface ConfigRefreshResult {
  kind: "downloaded" | "static";
  updated_at_ms: number | null;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function hhmm(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export function refreshStatusMessage(
  result: ConfigRefreshResult,
  opts: { reconnected: boolean },
): string {
  if (result.kind === "static") {
    return "This profile is a single server link — nothing to download";
  }
  const at = result.updated_at_ms != null ? ` · ${hhmm(result.updated_at_ms)}` : "";
  return `Config downloaded and installed${at}${opts.reconnected ? " · VPN reconnected" : ""}`;
}

export function refreshErrorMessage(error: unknown): string {
  const raw = String(error ?? "").replace(/^Config download failed:\s*/, "").trim() || "unknown error";
  const prefix = "Couldn't download config: ";
  const room = 160 - prefix.length;
  return prefix + (raw.length > room ? `${raw.slice(0, room - 1)}…` : raw);
}

/// Unknown is "never", not "just now" — the old label claimed freshness it
/// never measured.
export function formatConfigUpdated(updatedAtMs: number | null, now: number = Date.now()): string {
  if (updatedAtMs == null) return "never";
  const age = now - updatedAtMs;
  if (age < 60_000) return "just now";
  if (age < 60 * 60_000) return `${Math.floor(age / 60_000)} min ago`;
  const d = new Date(updatedAtMs);
  const n = new Date(now);
  if (d.toDateString() === n.toDateString()) return `today ${hhmm(updatedAtMs)}`;
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${hhmm(updatedAtMs)}`;
}
