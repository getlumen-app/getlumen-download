import { useState } from "react";
import ConnectButton from "../components/ConnectButton";
import LumenLogo from "../components/LumenLogo";
import { latencyState, latencyColor } from "../lib/latency";
import {
  availableLocations,
  locationFlag,
  locationLabel,
  type LocationOption,
} from "../lib/locations";
import "./Home.css";

type ConnectionState = "disconnected" | "connecting" | "connected";

interface ProxyNode {
  name: string;
  type: string;
  alive: boolean;
  delay: number;
}

interface Props {
  connectionState: ConnectionState;
  currentServer: string;
  latency: number;
  uploadSpeed: number;
  downloadSpeed: number;
  connectionTime: number;
  onConnect: () => void;
  /** Nodes from Clash selector group `proxy` (includes proxy-auto + geo). */
  locationNodes?: ProxyNode[];
  onSelectLocation?: (tag: string) => void;
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
  latency,
  uploadSpeed,
  downloadSpeed,
  connectionTime,
  onConnect,
  locationNodes,
  onSelectLocation,
  errorMsg,
}: Props) {
  const [sheetOpen, setSheetOpen] = useState(false);
  const locations: LocationOption[] = availableLocations(locationNodes);
  const displayFlag = locationFlag(currentServer);
  const displayName = locationLabel(currentServer);
  const canPick = Boolean(onSelectLocation) && locations.length > 1;

  function handlePick(tag: string) {
    onSelectLocation?.(tag);
    setSheetOpen(false);
  }

  return (
    <div className="home">
      <LumenLogo size={32} className="home__logo" />

      <button
        type="button"
        className={`home__server${canPick ? " home__server--interactive" : ""}`}
        onClick={() => canPick && setSheetOpen(true)}
        disabled={!canPick}
        aria-haspopup="dialog"
        aria-expanded={sheetOpen}
      >
        <span className="home__flag">{displayFlag}</span>
        <span className="home__server-name">{displayName}</span>
        {connectionState === "connected" &&
          (() => {
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
        {canPick && <span className="home__server-chevron" aria-hidden>▾</span>}
      </button>

      <div className="home__button-area">
        <ConnectButton state={connectionState} onClick={onConnect} />
      </div>

      <div className={`home__status home__status--${connectionState}`}>
        {statusLabels[connectionState]}
      </div>

      {connectionState === "connected" && connectionTime > 0 && (
        <div className="home__time">{formatTime(connectionTime)}</div>
      )}

      {errorMsg && <div className="home__error">{errorMsg}</div>}

      <div
        className={`home__speed ${connectionState === "connected" ? "visible" : ""}`}
      >
        <div className="home__speed-item">
          <span className="home__speed-arrow">↑</span>
          <span className="home__speed-value">
            {formatSpeed(uploadSpeed).value}
          </span>
          <span className="home__speed-unit">
            {formatSpeed(uploadSpeed).unit}
          </span>
        </div>
        <div className="home__speed-divider" />
        <div className="home__speed-item">
          <span className="home__speed-arrow">↓</span>
          <span className="home__speed-value">
            {formatSpeed(downloadSpeed).value}
          </span>
          <span className="home__speed-unit">
            {formatSpeed(downloadSpeed).unit}
          </span>
        </div>
      </div>

      {sheetOpen && (
        <div
          className="home__sheet-backdrop"
          role="presentation"
          onClick={() => setSheetOpen(false)}
        >
          <div
            className="home__sheet"
            role="dialog"
            aria-label="Choose location"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="home__sheet-handle" />
            <h2 className="home__sheet-title">Location</h2>
            <ul className="home__sheet-list">
              {locations.map((loc) => {
                const active = currentServer === loc.tag;
                const node = locationNodes?.find((n) => n.name === loc.tag);
                const ms = node && node.delay > 0 ? node.delay : null;
                return (
                  <li key={loc.tag}>
                    <button
                      type="button"
                      className={`home__sheet-row${active ? " active" : ""}`}
                      onClick={() => handlePick(loc.tag)}
                    >
                      <span className="home__sheet-flag">{loc.flag}</span>
                      <span className="home__sheet-label">{loc.label}</span>
                      {ms != null && (
                        <span className="home__sheet-delay">{ms}ms</span>
                      )}
                      {active && (
                        <span className="home__sheet-check" aria-label="Selected">
                          ✓
                        </span>
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        </div>
      )}
    </div>
  );
}
