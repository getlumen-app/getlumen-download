import ConnectButton from "../components/ConnectButton";
import LumenLogo from "../components/LumenLogo";
import { latencyState, latencyColor } from "../lib/latency";
import "./Home.css";

type ConnectionState = "disconnected" | "connecting" | "connected";

interface Props {
  connectionState: ConnectionState;
  currentServer: string;
  currentFlag: string;
  latency: number;
  uploadSpeed: number;
  downloadSpeed: number;
  connectionTime: number;
  onConnect: () => void;
  errorMsg?: string;
}

function formatTime(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}h ${String(m).padStart(2, "0")}m`;
  if (m > 0) return `${m}m ${String(s).padStart(2, "0")}s`;
  return `${s}s`;
}

function formatSpeed(kbps: number): { value: string; unit: string } {
  if (kbps < 1) return { value: "0", unit: "KB/s" };
  if (kbps < 1024) return { value: Math.round(kbps).toString(), unit: "KB/s" };
  const mbps = kbps / 1024;
  if (mbps < 10) return { value: mbps.toFixed(1), unit: "MB/s" };
  return { value: Math.round(mbps).toString(), unit: "MB/s" };
}

const statusLabels: Record<ConnectionState, string> = {
  disconnected: "Tap to connect",
  connecting: "Connecting...",
  connected: "Connected",
};

export default function Home({
  connectionState,
  currentServer,
  currentFlag,
  latency,
  uploadSpeed,
  downloadSpeed,
  connectionTime,
  onConnect,
  errorMsg,
}: Props) {
  return (
    <div className="home">
      {/* Logo */}
      <LumenLogo size={32} className="home__logo" />

      {/* Server info */}
      <div className="home__server">
        <span className="home__flag">{currentFlag}</span>
        <span className="home__server-name">{currentServer}</span>
        {connectionState === "connected" && (() => {
          // Pending state shows faint "—" instead of nothing — gives the
          // user a clear "we know about this row, just no measurement yet"
          // signal instead of layout that jumps when the test arrives.
          const state = latencyState({
            ms: latency > 0 ? latency : null,
          });
          return (
            <span
              className="home__latency"
              style={{ color: latencyColor(state) }}
              aria-live="polite"
            >
              {state === "pending" ? "—" : `${latency}ms`}
            </span>
          );
        })()}
      </div>

      {/* Connect button */}
      <div className="home__button-area">
        <ConnectButton state={connectionState} onClick={onConnect} />
      </div>

      {/* Status */}
      <div className={`home__status home__status--${connectionState}`}>
        {statusLabels[connectionState]}
      </div>

      {/* Connection time */}
      {connectionState === "connected" && connectionTime > 0 && (
        <div className="home__time">{formatTime(connectionTime)}</div>
      )}

      {/* Error message */}
      {errorMsg && (
        <div className="home__error">{errorMsg}</div>
      )}

      {/* Speed indicators */}
      <div className={`home__speed ${connectionState === "connected" ? "visible" : ""}`}>
        <div className="home__speed-item">
          <span className="home__speed-arrow">↑</span>
          <span className="home__speed-value">{formatSpeed(uploadSpeed).value}</span>
          <span className="home__speed-unit">{formatSpeed(uploadSpeed).unit}</span>
        </div>
        <div className="home__speed-divider" />
        <div className="home__speed-item">
          <span className="home__speed-arrow">↓</span>
          <span className="home__speed-value">{formatSpeed(downloadSpeed).value}</span>
          <span className="home__speed-unit">{formatSpeed(downloadSpeed).unit}</span>
        </div>
      </div>
    </div>
  );
}
