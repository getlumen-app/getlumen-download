import { useState, useEffect, useRef, useMemo } from "react";
import { testDelay } from "../hooks/useTauri";
import {
  latencyState,
  latencyClassName,
  latencyDisplay,
  type LatencyState,
} from "../lib/latency";
import "./Proxies.css";

interface ProxyNode {
  name: string;
  type: string;
  alive: boolean;
  delay: number;
}

interface ProxyGroup {
  name: string;
  type: string;
  now: string;
  all: string[];
  nodes: ProxyNode[];
}

interface Props {
  groups: ProxyGroup[];
  onSelectProxy: (groupName: string, nodeName: string) => void;
  /** Fires when a TCP-RTT test result arrives for `name`. App propagates to Home. */
  onDelayMeasured?: (name: string, ms: number) => void;
}

const SERVER_FLAGS: Record<string, string> = {
  "relay-eu-443": "🇷🇺→🇩🇪",
  "relay-eu-httpupgrade": "🇷🇺→🇩🇪",
  "relay-eu-grpc": "🇷🇺→🇩🇪",
  "relay-moscow-httpupgrade": "🇷🇺→🇩🇪",
  "vless-cdn-ws": "🌐",
  "vless-cdn-grpc": "🌐",
  "netcup-tcp-reality": "🇩🇪",
  "netcup-grpc-reality": "🇩🇪",
  "proxy-moscow": "🇷🇺",
};

const SERVER_LABELS: Record<string, string> = {
  "relay-eu-443": "Port 443 Relay",
  "relay-eu-httpupgrade": "Moscow HTTPUpgrade",
  "relay-eu-grpc": "Moscow gRPC Relay",
  "relay-moscow-httpupgrade": "Moscow Relay",
  "vless-cdn-ws": "CDN WebSocket",
  "vless-cdn-grpc": "CDN gRPC",
  "netcup-tcp-reality": "Frankfurt Direct",
  "netcup-grpc-reality": "Frankfurt gRPC",
  "proxy-moscow": "Moscow Exit",
};

const GROUP_LABELS: Record<string, string> = {
  "proxy-auto": "Auto Select",
  "proxy-moscow": "Russian Exit",
  "messenger-auto": "Messengers",
  "ru-smart": "RU Smart",
};

/** Inline spinner — cyan, 12px, 1s rotation. SVG matches Lumen icon style. */
function DelaySpinner() {
  return (
    <svg
      className="proxy-node__spinner"
      viewBox="0 0 24 24"
      width="12"
      height="12"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      aria-label="Testing"
    >
      <path d="M21 12a9 9 0 11-6.219-8.56" />
    </svg>
  );
}

export default function Proxies({ groups, onSelectProxy, onDelayMeasured }: Props) {
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  // null = never tested (show "—"); 0 = timeout; >0 = ms
  const [liveDelays, setLiveDelays] = useState<Record<string, number | null>>({});
  // Per-server test-in-flight flag; drives spinner
  const [inFlight, setInFlight] = useState<Set<string>>(new Set());
  const [progress, setProgress] = useState({ done: 0, total: 0 });
  const autoTestedRef = useRef<string | null>(null);

  function toggleGroup(name: string) {
    setCollapsed((prev) => ({ ...prev, [name]: !prev[name] }));
  }

  // Collect unique individual-server tags (skip nested URLTest/Selector groups)
  const individualNames = useMemo(() => {
    const set = new Set<string>();
    for (const group of groups) {
      for (const node of group.nodes) {
        if (node.type !== "URLTest" && node.type !== "Selector") {
          set.add(node.name);
        }
      }
    }
    return Array.from(set);
  }, [groups]);

  async function handleTestAll() {
    if (inFlight.size > 0) return;
    if (individualNames.length === 0) return;

    // Mark every node as testing simultaneously (UI shows N spinners).
    setInFlight(new Set(individualNames));
    setProgress({ done: 0, total: individualNames.length });

    // Sequential to avoid TCP loopback contention; results arrive one-by-one.
    let done = 0;
    for (const name of individualNames) {
      let delay = 0;
      try {
        delay = await testDelay(name);
      } catch {
        delay = 0;
      }
      setLiveDelays((prev) => ({ ...prev, [name]: delay }));
      onDelayMeasured?.(name, delay);
      // Remove from inFlight set so this row stops spinning + shows result.
      setInFlight((prev) => {
        const next = new Set(prev);
        next.delete(name);
        return next;
      });
      done += 1;
      setProgress((p) => ({ ...p, done }));
    }
  }

  // Fire one auto-test per session/group-shape — avoids re-testing on collapse toggle.
  // Uses join of names as the key so adding/removing a server triggers a fresh sweep.
  useEffect(() => {
    if (individualNames.length === 0) return;
    const fingerprint = individualNames.slice().sort().join("|");
    if (autoTestedRef.current === fingerprint) return;
    autoTestedRef.current = fingerprint;
    handleTestAll();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [individualNames]);

  const testing = inFlight.size > 0;
  const testBtnLabel = testing
    ? `Testing ${progress.done}/${progress.total}…`
    : "Test All";

  return (
    <div className="proxies">
      <div className="proxies__header">
        <h2 className="proxies__title">Proxies</h2>
        <button
          className="proxies__test-btn"
          onClick={handleTestAll}
          disabled={testing || individualNames.length === 0}
        >
          {testBtnLabel}
        </button>
      </div>

      <div className="proxies__list">
        {groups.map((group) => (
          <div key={group.name} className="proxy-group">
            <button
              className="proxy-group__header"
              onClick={() => toggleGroup(group.name)}
            >
              <span className="proxy-group__chevron">
                {collapsed[group.name] ? "▸" : "▾"}
              </span>
              <span className="proxy-group__name">
                {GROUP_LABELS[group.name] || group.name}
              </span>
              <span className="proxy-group__count">
                ({group.nodes.length})
              </span>
              <span className="proxy-group__type">{group.type}</span>
            </button>

            {!collapsed[group.name] && (
              <div className="proxy-group__nodes">
                {group.nodes.map((node) => {
                  // Hide stale Clash-cached HTTP-test delays (`node.delay`):
                  // we only show our own TCP-RTT results from `liveDelays`.
                  // Until `testDelay` returns, `liveDelays[node.name]` is
                  // undefined → state="pending" → renders "—".
                  const ms =
                    node.name in liveDelays ? liveDelays[node.name] : null;
                  const state: LatencyState = latencyState({
                    ms,
                    testing: inFlight.has(node.name),
                  });
                  return (
                    <button
                      key={node.name}
                      className={`proxy-node ${group.now === node.name ? "active" : ""}`}
                      onClick={() => onSelectProxy(group.name, node.name)}
                    >
                      <span className="proxy-node__active-dot" />
                      <span className="proxy-node__flag">
                        {SERVER_FLAGS[node.name] || "🌍"}
                      </span>
                      <span className="proxy-node__name">
                        {SERVER_LABELS[node.name] || node.name}
                      </span>
                      <span
                        className={`proxy-node__delay ${latencyClassName(state)}`}
                        aria-live="polite"
                      >
                        {state === "testing" ? (
                          <DelaySpinner />
                        ) : (
                          latencyDisplay(state, ms)
                        )}
                      </span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
