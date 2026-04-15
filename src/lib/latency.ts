/**
 * Latency display SSOT — shared by Home, Proxies, and any future surface.
 *
 * Thresholds (TCP RTT measured via physical iface, bypassing TUN):
 *   0–149 ms  good   — excellent
 *   150–299  ok     — acceptable
 *   ≥ 300    bad    — slow / overloaded
 *
 * Lifecycle states (drive UI rendering):
 *   pending  — never tested; show faint "—"
 *   testing  — test currently in flight; show spinner
 *   timeout  — test returned 0 / errored; show ✕ in red
 *   good/ok/bad — colored number
 */

export type LatencyState = "pending" | "testing" | "good" | "ok" | "bad" | "timeout";

export interface LatencyInput {
  /** Measured RTT in ms. `null`/`undefined` means never tested. `0` means timeout. */
  ms?: number | null;
  /** True while a test is currently running for this server. */
  testing?: boolean;
}

export function latencyState({ ms, testing }: LatencyInput): LatencyState {
  if (testing) return "testing";
  if (ms == null) return "pending";
  if (ms <= 0) return "timeout";
  if (ms < 150) return "good";
  if (ms < 300) return "ok";
  return "bad";
}

export function latencyColor(state: LatencyState): string {
  switch (state) {
    case "good":
      return "var(--status-healthy)";
    case "ok":
      return "var(--status-degraded)";
    case "bad":
    case "timeout":
      return "var(--status-down)";
    case "testing":
    case "pending":
    default:
      return "var(--fg-faint)";
  }
}

/** Stable className suffix for CSS targeting (matches existing `delay--good` etc.). */
export function latencyClassName(state: LatencyState): string {
  return `delay--${state}`;
}

/** Human-readable string for the value cell. Spinner/dash handled by caller. */
export function latencyDisplay(state: LatencyState, ms?: number | null): string {
  if (state === "pending") return "—";
  if (state === "testing") return ""; // caller renders spinner
  if (state === "timeout") return "✕";
  return `${ms}ms`;
}
