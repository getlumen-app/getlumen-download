import { useState, useEffect, useCallback, useRef } from "react";
import Home from "./pages/Home";
import Proxies from "./pages/Proxies";
import Settings from "./pages/Settings";
import KeyInput from "./pages/KeyInput";
import Logs from "./pages/Logs";
import BottomNav from "./components/BottomNav";
import * as tauri from "./hooks/useTauri";
import { useKeyStore } from "./hooks/useKeyStore";
import "./App.css";

type Tab = "home" | "proxies" | "settings";
type ConnectionState = "disconnected" | "connecting" | "connected" | "error";

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
  const [currentServer, setCurrentServer] = useState("Auto Select");
  const [currentFlag] = useState("⚡");
  const [latency, setLatency] = useState(0);
  const [uploadSpeed, setUploadSpeed] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState(0);
  const [proxyGroups, setProxyGroups] = useState<ProxyGroup[]>([]);
  const [connectionTime, setConnectionTime] = useState(0);
  const [showLogs, setShowLogs] = useState(false);
  const trafficInterval = useRef<ReturnType<typeof setInterval> | null>(null);

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
        // Auto-group lookup: Proteus config uses "proxy-auto", single-VLESS config uses "proxy".
        // Fall back to any URLTest as the active auto-group so Home can display the server name.
        const autoGroup =
          groups.find((g) => g.name === "proxy-auto") ??
          groups.find((g) => g.name === "proxy") ??
          groups.find((g) => g.type === "URLTest");
        if (autoGroup?.now) {
          setCurrentServer(autoGroup.now);
          // Don't set latency from stale Clash cache here. Proxies' TCP-RTT
          // test owns the value via setLatency below in handleSelectProxy
          // and (if added) in a future Home-side test. Keep latency=0
          // so Home shows the "—" pending placeholder.
        }
      }
    } catch {
      // Clash API not ready
    }
  }, []);

  // Poll proxy data when connected
  useEffect(() => {
    if (connectionState !== "connected") return;
    fetchProxyData(); // immediate
    const interval = setInterval(fetchProxyData, 5000);
    return () => clearInterval(interval);
  }, [connectionState, fetchProxyData]);

  async function handleConnect() {
    // Mode resolution:
    //   1. If user explicitly chose 'proxy' in Settings → system proxy
    //   2. Else if helper available → TUN
    //   3. Else → system proxy fallback
    const userPref = localStorage.getItem("lumen-vpn-mode"); // "tun" | "proxy" | null
    const tunReady = await tauri.isTunAvailable();
    const useTun = userPref === "proxy" ? false : tunReady;

    if (connectionState === "connected") {
      // Disconnect
      setConnectionState("disconnected");
      setUploadSpeed(0);
      setDownloadSpeed(0);
      setProxyGroups([]);
      try {
        if (useTun) {
          await tauri.tunDisconnect();
        } else {
          await tauri.disconnect();
        }
      } catch (e) {
        console.error("Disconnect error:", e);
      }
      return;
    }

    if (!accessKey) return;
    setConnectionState("connecting");
    setErrorMsg("");

    try {
      if (useTun) {
        await tauri.tunConnect(accessKey);
      } else {
        await tauri.connect(accessKey);
      }
      setConnectionState("connected");
    } catch (e) {
      const msg = String(e);
      console.error("Connect error:", msg);
      setErrorMsg(msg);
      setConnectionState("error");
      // Reset to disconnected after showing error
      setTimeout(() => {
        if (connectionState === "error") setConnectionState("disconnected");
      }, 5000);
    }
  }

  function handleSaveKey(key: string) {
    keyStore.addKey(key);
    // After first-time authorization the user should land on Home,
    // not wherever the tab state happens to be (e.g. Settings from
    // a previous "Add profile" flow).
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
    // Update Home's displayed server when the auto-group (Proteus "proxy-auto" or
    // single-VLESS "proxy") selection changes.
    if (groupName === "proxy-auto" || groupName === "proxy") {
      setCurrentServer(nodeName);
      const node = proxyGroups
        .find((g) => g.name === groupName)
        ?.nodes.find((n) => n.name === nodeName);
      if (node) setLatency(node.delay);
    }
  }

  if (!accessKey) {
    return <KeyInput onSubmit={handleSaveKey} />;
  }

  return (
    <div className="app-shell">
      <div className="app-content">
        {tab === "home" && (
          <Home
            connectionState={connectionState === "error" ? "disconnected" : connectionState}
            currentServer={currentServer}
            currentFlag={currentFlag}
            latency={latency}
            uploadSpeed={uploadSpeed}
            downloadSpeed={downloadSpeed}
            connectionTime={connectionTime}
            onConnect={handleConnect}
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
          />
        )}
      </div>
      <BottomNav active={tab} onChange={setTab} />
      {showLogs && <Logs onClose={() => setShowLogs(false)} />}
    </div>
  );
}
