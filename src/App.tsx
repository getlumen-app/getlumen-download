import { useState, useEffect, useCallback, useRef } from "react";
import Home from "./pages/Home";
import Proxies from "./pages/Proxies";
import Settings from "./pages/Settings";
import KeyInput from "./pages/KeyInput";
import Logs from "./pages/Logs";
import BottomNav from "./components/BottomNav";
import * as tauri from "./hooks/useTauri";
import { useKeyStore } from "./hooks/useKeyStore";
import {
  CONNECTION_INTENT_KEY,
  planSessionTeardown,
  readStoredConnectionIntent,
  shouldApplyEffectiveStatusSync,
  shouldAttemptWbstreamOnConnectError,
  shouldDisconnectOnPowerTap,
  shouldSelfHealOnLaunch,
  shouldSwitchModeOnPowerTap,
  transportFromEffectiveStatus,
  type ActiveTransport,
  type ConnectionState,
} from "./lib/connectionState";
import {
  LOCATION_OPTIONS,
  readStoredLocation,
  writeStoredLocation,
} from "./lib/locations";
import "./App.css";

type Tab = "home" | "proxies" | "settings";

interface ProxyNode {
  name: string;
  type: string;
  alive: boolean;
  delay: number;
  history: { delay: number }[];
}

interface ProxyGroup {
  name: string;
  type: string;
  now: string;
  all: string[];
  nodes: ProxyNode[];
}

export default function App() {
  const [tab, setTab] = useState<Tab>("home");
  const keyStore = useKeyStore();
  const accessKey = keyStore.activeKey?.value ?? null;
  const [connectionState, setConnectionState] = useState<ConnectionState>("disconnected");
  const [errorMsg, setErrorMsg] = useState("");
  const [currentServer, setCurrentServer] = useState(() => readStoredLocation());
  const [latency, setLatency] = useState(0);
  const [uploadSpeed, setUploadSpeed] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [proxyGroups, setProxyGroups] = useState<ProxyGroup[]>([]);
  const [connectionTime, setConnectionTime] = useState(0);
  const [showLogs, setShowLogs] = useState(false);
  const [activeTransport, setActiveTransport] = useState<ActiveTransport>(null);
  const trafficInterval = useRef<ReturnType<typeof setInterval> | null>(null);
  const healthFailures = useRef(0);
  const fallbackSwitching = useRef(false);
  const launchSelfHealChecked = useRef(false);
  const locationAppliedRef = useRef(false);
  const connectInFlight = useRef(false);

  useEffect(() => {
    let cancelled = false;
    async function syncEffectiveStatus() {
      try {
        const storedIntent = readStoredConnectionIntent(
          localStorage.getItem(CONNECTION_INTENT_KEY)
        );
        const syncAction = shouldApplyEffectiveStatusSync({
          connectionState,
          connectInFlight: connectInFlight.current,
          storedIntent,
        });
        // Never fight an in-flight connect/disconnect/mode-switch, and never
        // snap Connected back while the user asked to disconnect (teardown
        // still draining → 2s "reconnect" flash, 2026-08-02).
        if (syncAction === "skip") return;

        const status = await tauri.getEffectiveStatus();
        if (cancelled) return;
        // Re-evaluate after await — disconnect may have started mid-poll.
        const intentNow = readStoredConnectionIntent(
          localStorage.getItem(CONNECTION_INTENT_KEY)
        );
        const actionNow = shouldApplyEffectiveStatusSync({
          connectionState,
          connectInFlight: connectInFlight.current,
          storedIntent: intentNow,
        });
        if (actionNow === "skip") return;
        const transport = transportFromEffectiveStatus(status);

        if (actionNow === "force_disconnected") {
          if (transport && !connectInFlight.current) {
            connectInFlight.current = true;
            try {
              try {
                await tauri.disconnect();
              } catch (e) {
                console.warn("disconnect-intent proxy stop:", e);
              }
              try {
                await tauri.tunDisconnect();
              } catch (e) {
                console.warn("disconnect-intent tun stop:", e);
              }
            } finally {
              connectInFlight.current = false;
            }
          }
          if (cancelled) return;
          setActiveTransport(null);
          setConnectionState("disconnected");
          setCurrentServer(readStoredLocation());
          locationAppliedRef.current = false;
          return;
        }

        if (!launchSelfHealChecked.current) {
          launchSelfHealChecked.current = true;
          if (shouldSelfHealOnLaunch(storedIntent, transport)) {
            if (transport === "proxy") {
              await tauri.disconnect();
            } else {
              await tauri.tunDisconnect();
            }
            if (cancelled) return;
            setActiveTransport(null);
            setConnectionState("disconnected");
            setCurrentServer(readStoredLocation());
            return;
          }
        }
        if (transport) {
          setActiveTransport(transport);
          setConnectionState("connected");
          // Keep geo tag on Home; do not overwrite with TUN/proxy mode labels.
        } else if (connectionState === "connected") {
          setActiveTransport(null);
          setConnectionState("disconnected");
          setCurrentServer(readStoredLocation());
          locationAppliedRef.current = false;
        }
      } catch (e) {
        console.warn("effective status sync failed:", e);
      }
    }
    syncEffectiveStatus();
    const interval = setInterval(syncEffectiveStatus, 5000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [connectionState]);

  // Connection timer
  useEffect(() => {
    if (connectionState !== "connected") {
      setConnectionTime(0);
      return;
    }
    const interval = setInterval(() => {
      setConnectionTime((t) => t + 1);
    }, 1000);
    return () => clearInterval(interval);
  }, [connectionState]);

  // Poll traffic when connected
  useEffect(() => {
    if (connectionState !== "connected") {
      if (trafficInterval.current) {
        clearInterval(trafficInterval.current);
        trafficInterval.current = null;
      }
      return;
    }
    trafficInterval.current = setInterval(async () => {
      try {
        const traffic = await tauri.getTraffic();
        // Clash API /traffic returns bytes/sec
        setUploadSpeed(traffic.up / 1024); // bytes → KB/s
        setDownloadSpeed(traffic.down / 1024);
      } catch {
        // Clash API not ready yet
      }
    }, 1000);
    return () => {
      if (trafficInterval.current) clearInterval(trafficInterval.current);
    };
  }, [connectionState]);

  // Fetch real proxy data from Clash API
  const fetchProxyData = useCallback(async () => {
    try {
      const data = await tauri.getProxies();
      if (!data || !("proxies" in data)) return;

      const proxies = data.proxies as Record<string, {
        type: string;
        now?: string;
        all?: string[];
        history?: { delay: number }[];
        alive?: boolean;
      }>;

      const groups: ProxyGroup[] = [];
      // Dynamically find ALL URLTest/Selector groups from Clash API
      const groupNames = Object.keys(proxies).filter((name) => {
        const p = proxies[name];
        return (p.type === "URLTest" || p.type === "Selector") && p.all && p.all.length > 0;
      });

      for (const gName of groupNames) {
        const g = proxies[gName];
        if (!g || !g.all) continue;

        const nodes: ProxyNode[] = g.all
          .map((nName): ProxyNode | null => {
            const n = proxies[nName];
            if (!n) return null;
            // Drop stale Clash-API HTTP-test delays. They are misleading
            // because they measure HTTPS via TUN, not direct TCP RTT.
            // Proxies.tsx runs its own TCP-RTT test and shows a "—" /
            // spinner placeholder until results arrive.
            return {
              name: nName,
              type: n.type || "Unknown",
              alive: n.alive ?? true,
              delay: 0,
              history: [] as { delay: number }[],
            };
          })
          .filter((n): n is ProxyNode => n !== null);

        groups.push({
          name: gName,
          type: g.type || "URLTest",
          now: g.now || "",
          all: g.all,
          nodes,
        });
      }

      if (groups.length > 0) {
        setProxyGroups(groups);
        // Prefer outer selector `proxy` (sticky pin). Fall back to proxy-auto
        // urltest `now` for older configs without a selector wrapper.
        const selector =
          groups.find((g) => g.name === "proxy" && g.type === "Selector") ??
          groups.find((g) => g.name === "proxy");
        const autoGroup =
          groups.find((g) => g.name === "proxy-auto") ??
          groups.find((g) => g.type === "URLTest");
        const now = selector?.now || autoGroup?.now;
        if (now) {
          setCurrentServer(now);
        }

        // Re-apply stored location once per connect so preference survives
        // Clash cache / reconnect without fighting ongoing Auto probes later.
        if (!locationAppliedRef.current && selector) {
          const preferred = readStoredLocation();
          const allowed = new Set(selector.all);
          if (preferred && preferred !== "proxy-auto" && allowed.has(preferred)) {
            locationAppliedRef.current = true;
            void tauri.selectProxy("proxy", preferred).then(() => {
              setCurrentServer(preferred);
              setProxyGroups((gs) =>
                gs.map((g) =>
                  g.name === "proxy" ? { ...g, now: preferred } : g
                )
              );
            }).catch(() => {
              locationAppliedRef.current = false;
            });
          } else {
            locationAppliedRef.current = true;
          }
        }
      }
    } catch {
      // Clash API not ready
    }
  }, []);

  // Poll proxy data when connected
  useEffect(() => {
    if (connectionState !== "connected") {
      locationAppliedRef.current = false;
      return;
    }
    fetchProxyData(); // immediate
    const interval = setInterval(fetchProxyData, 5000);
    return () => clearInterval(interval);
  }, [connectionState, fetchProxyData]);

  useEffect(() => {
    if (connectionState !== "connected" || activeTransport !== "tun") {
      healthFailures.current = 0;
      return;
    }

    let cancelled = false;
    async function checkHealth() {
      if (cancelled || fallbackSwitching.current || activeTransport !== "tun") return;
      const probeOk = await tauri.internetHealthProbe();
      const decision = await tauri.healthMonitorDecision(
        "tun",
        healthFailures.current,
        probeOk
      );
      healthFailures.current = decision.consecutive_failures;
      if (decision.action !== "switch_to_wbstream") return;

      fallbackSwitching.current = true;
      setConnectionState("connecting");
      setCurrentServer("WB Stream");
      setErrorMsg("");
      try {
        await tauri.tunDisconnect();
      } catch (e) {
        console.warn("TUN stop before WB Stream fallback:", e);
      }
      try {
        await tauri.tunConnectWbstreamFallback();
        if (!cancelled) {
          healthFailures.current = 0;
          setActiveTransport("wbstream");
          setConnectionState("connected");
        }
      } catch (e) {
        if (!cancelled) {
          setErrorMsg(`WB Stream fallback failed: ${e}`);
          setConnectionState("error");
          setActiveTransport(null);
        }
      } finally {
        fallbackSwitching.current = false;
      }
    }

    const warmup = setTimeout(checkHealth, 5000);
    const interval = setInterval(checkHealth, 10000);
    return () => {
      cancelled = true;
      clearTimeout(warmup);
      clearInterval(interval);
    };
  }, [connectionState, activeTransport]);

  async function tearDownSession() {
    const tunStatus = await tauri.tunStatus();
    const plan = planSessionTeardown(activeTransport, tunStatus);
    // Stop proxy first, then TUN. Effective status prefers TUN when helper is up.
    if (plan.stopProxy) {
      try {
        await tauri.disconnect();
      } catch (e) {
        console.error("Proxy disconnect error:", e);
      }
    }
    if (plan.stopTun) {
      try {
        await tauri.tunDisconnect();
      } catch (e) {
        console.error("TUN disconnect error:", e);
      }
    }
    setActiveTransport(null);
    setUploadSpeed(0);
    setDownloadSpeed(0);
    setProxyGroups([]);
    setCurrentServer(readStoredLocation());
    locationAppliedRef.current = false;
    healthFailures.current = 0;
  }

  async function connectWithPreferredMode(useTun: boolean) {
    if (!accessKey) return;
    setConnectionState("connecting");
    setErrorMsg("");
    locationAppliedRef.current = false;
    // Native connect starts the transport before the command resolves; mark
    // intent first so status sync never tears down that in-flight session.
    localStorage.setItem(CONNECTION_INTENT_KEY, "connected");

    try {
      if (useTun) {
        await tauri.tunConnect(accessKey);
        setActiveTransport("tun");
        healthFailures.current = 0;
      } else {
        await tauri.connect(accessKey);
        setActiveTransport("proxy");
      }
      setConnectionState("connected");
      setCurrentServer(readStoredLocation());
    } catch (e) {
      const msg = String(e);
      console.error("Connect error:", msg);
      // Control-plane / local failures must not jump to WB Stream (Doha class).
      // Hard-whitelist carrier is owned by the post-connect health monitor.
      if (useTun && shouldAttemptWbstreamOnConnectError(msg)) {
        try {
          setCurrentServer("WB Stream");
          await tauri.tunConnectWbstreamFallback();
          setActiveTransport("wbstream");
          localStorage.setItem(CONNECTION_INTENT_KEY, "connected");
          setConnectionState("connected");
          return;
        } catch (fallbackError) {
          console.error("WB Stream fallback error:", fallbackError);
          setErrorMsg(`${msg}; WB Stream fallback failed: ${fallbackError}`);
          localStorage.setItem(CONNECTION_INTENT_KEY, "disconnected");
          setConnectionState("error");
          return;
        }
      } else {
        setErrorMsg(msg);
      }
      localStorage.setItem(CONNECTION_INTENT_KEY, "disconnected");
      setConnectionState("error");
      setActiveTransport(null);
      // Reset to disconnected after showing error
      setTimeout(() => {
        setConnectionState((state) => (state === "error" ? "disconnected" : state));
      }, 5000);
    }
  }

  async function switchToPreferredMode(useTun: boolean) {
    if (connectInFlight.current) return;
    connectInFlight.current = true;
    try {
      setConnectionState("connecting");
      localStorage.setItem(CONNECTION_INTENT_KEY, "connected");
      await tearDownSession();
      await connectWithPreferredMode(useTun);
    } finally {
      connectInFlight.current = false;
    }
  }

  async function handleVpnModeChange(mode: "tun" | "proxy") {
    if (connectionState !== "connected") return;
    const tunReady = mode === "tun" ? await tauri.isTunAvailable() : false;
    const useTun = mode === "tun" && tunReady;
    if (mode === "tun" && !tunReady) return;
    if (!shouldSwitchModeOnPowerTap("connected", activeTransport, useTun)) return;
    await switchToPreferredMode(useTun);
  }

  async function handleConnect() {
    if (connectInFlight.current || connectionState === "connecting") return;

    // Mode resolution:
    //   1. If user explicitly chose 'proxy' in Settings → system proxy
    //   2. Else if helper available → TUN
    //   3. Else → system proxy fallback
    const userPref = localStorage.getItem("lumen-vpn-mode"); // "tun" | "proxy" | null
    const tunReady = await tauri.isTunAvailable();
    const useTun = userPref === "proxy" ? false : tunReady;

    // Power button while Connected always disconnects. Mode hot-swap lives in
    // Settings (handleVpnModeChange) — power-tap switch caused Stop→Start thrash
    // whenever activeTransport briefly disagreed with preference.
    if (shouldDisconnectOnPowerTap(connectionState)) {
      connectInFlight.current = true;
      try {
        localStorage.setItem(CONNECTION_INTENT_KEY, "disconnected");
        setConnectionState("disconnected");
        setActiveTransport(null);
        setUploadSpeed(0);
        setDownloadSpeed(0);
        await tearDownSession();
        setConnectionState("disconnected");
        setActiveTransport(null);
      } finally {
        connectInFlight.current = false;
      }
      return;
    }

    connectInFlight.current = true;
    try {
      await connectWithPreferredMode(useTun);
    } finally {
      connectInFlight.current = false;
    }
  }

  function handleSaveKey(key: string) {
    keyStore.addKey(key);
    // After first-time authorization the user should land on Home,
    // not wherever the tab state happens to be (e.g. Settings from
    // a previous "Add profile" flow).
    setTab("home");
  }

  async function handleBootstrapImport(payload: string) {
    const profile = await tauri.importBootstrapPayload(payload);
    keyStore.replaceWithKey(profile.value, profile.name);
    localStorage.setItem("lumen-vpn-mode", profile.preferred_mode);
    localStorage.setItem(CONNECTION_INTENT_KEY, "disconnected");
    setTab("home");
  }

  async function handleSelectProxy(groupName: string, nodeName: string) {
    try {
      await tauri.selectProxy(groupName, nodeName);
    } catch {
      // fallback: update UI optimistically
    }
    setProxyGroups((groups) =>
      groups.map((g) =>
        g.name === groupName ? { ...g, now: nodeName } : g
      )
    );
    // Outer selector `proxy` (or legacy proxy-auto) drives Home location.
    if (groupName === "proxy" || groupName === "proxy-auto") {
      setCurrentServer(nodeName);
      writeStoredLocation(nodeName);
      const node = proxyGroups
        .find((g) => g.name === groupName)
        ?.nodes.find((n) => n.name === nodeName);
      if (node) setLatency(node.delay);
    }
  }

  async function handleSelectLocation(tag: string) {
    writeStoredLocation(tag);
    setCurrentServer(tag);
    if (connectionState !== "connected") return;
    try {
      await tauri.selectProxy("proxy", tag);
      setProxyGroups((groups) =>
        groups.map((g) => (g.name === "proxy" ? { ...g, now: tag } : g))
      );
    } catch (e) {
      console.warn("select location failed:", e);
    }
  }

  const locationGroup =
    proxyGroups.find((g) => g.name === "proxy") ??
    proxyGroups.find((g) => g.name === "proxy-auto");
  // When disconnected, still offer the full geo sheet so preference can be set
  // before connect; Clash membership is applied after connect.
  const locationNodes =
    locationGroup?.nodes ??
    LOCATION_OPTIONS.map((o) => ({
      name: o.tag,
      type: o.tag === "proxy-auto" ? "URLTest" : "VLESS",
      alive: true,
      delay: 0,
      history: [] as { delay: number }[],
    }));

  if (!accessKey) {
    return <KeyInput onSubmit={handleSaveKey} onBootstrapImport={handleBootstrapImport} />;
  }

  return (
    <div className="app-shell">
      <div className="app-content">
        {tab === "home" && (
          <Home
            connectionState={connectionState === "error" ? "disconnected" : connectionState}
            currentServer={currentServer}
            latency={latency}
            uploadSpeed={uploadSpeed}
            downloadSpeed={downloadSpeed}
            connectionTime={connectionTime}
            onConnect={handleConnect}
            locationNodes={locationNodes}
            onSelectLocation={handleSelectLocation}
            errorMsg={errorMsg}
          />
        )}
        {tab === "proxies" && (
          <Proxies
            groups={proxyGroups}
            onSelectProxy={handleSelectProxy}
            onDelayMeasured={(name, ms) => {
              if (name === currentServer) setLatency(ms);
            }}
          />
        )}
        {tab === "settings" && (
          <Settings
            accessKey={accessKey || ""}
            keyStore={keyStore}
            onClearKey={() => {
              keyStore.clearAll();
            }}
            onViewLogs={() => setShowLogs(true)}
            onVpnModeChange={handleVpnModeChange}
          />
        )}
      </div>
      <BottomNav active={tab} onChange={setTab} />
      {showLogs && <Logs onClose={() => setShowLogs(false)} />}
    </div>
  );
}
