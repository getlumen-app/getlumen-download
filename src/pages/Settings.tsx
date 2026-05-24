import * as React from "react";
import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useKeyStore, detectType, defaultNameFor } from "../hooks/useKeyStore";
import * as tauri from "../hooks/useTauri";
import {
  CONNECTION_INTENT_KEY,
  diagnosticLocationLabel,
  diagnosticRouteLabel,
  formatDiagnosticsSnapshot,
  repairNetworkMessage,
} from "../lib/connectionState";
import "./Settings.css";

interface TunStatus {
  helper_installed: boolean;
  helper_running: boolean;
  singbox_running: boolean;
  singbox_pid: number | null;
  uptime_secs: number | null;
}

interface Props {
  accessKey: string;
  keyStore: ReturnType<typeof useKeyStore>;
  onClearKey: () => void;
  onViewLogs?: () => void;
}

type ThemeOption = "system" | "light" | "dark";

export default function Settings({ accessKey, keyStore, onClearKey, onViewLogs }: Props) {
  const [theme, setTheme] = useState<ThemeOption>(() => {
    return (localStorage.getItem("lumen-theme") as ThemeOption) || "dark";
  });
  const [autoConnect, setAutoConnect] = useState(true);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [killSwitch, setKillSwitch] = useState(false);
  const [copied, setCopied] = useState(false);

  // VPN mode state — TUN by default, user can override
  const [tunStatus, setTunStatus] = useState<TunStatus | null>(null);
  const [tunBusy, setTunBusy] = useState(false);
  const [repairBusy, setRepairBusy] = useState(false);
  const [repairStatus, setRepairStatus] = useState<string | null>(null);
  const [diagnostics, setDiagnostics] = useState<tauri.NetworkDiagnostics | null>(null);
  const [diagnosticsBusy, setDiagnosticsBusy] = useState(false);
  const [diagnosticsCopyStatus, setDiagnosticsCopyStatus] = useState<string | null>(null);
  type VpnMode = "tun" | "proxy";
  const [vpnMode, setVpnMode] = useState<VpnMode>(
    () => (localStorage.getItem("lumen-vpn-mode") as VpnMode) || "tun"
  );

  function setVpnModeAndPersist(mode: VpnMode) {
    setVpnMode(mode);
    localStorage.setItem("lumen-vpn-mode", mode);
  }

  useEffect(() => {
    refreshTunStatus();
    refreshDiagnostics();
    const i = setInterval(refreshTunStatus, 5000);
    return () => clearInterval(i);
  }, []);

  async function refreshTunStatus() {
    try {
      const s = await invoke<TunStatus>("tun_status");
      setTunStatus(s);
    } catch (e) {
      console.warn("tun_status:", e);
    }
  }

  async function handleInstallHelper() {
    setTunBusy(true);
    try {
      await invoke("tun_install_helper");
      await refreshTunStatus();
    } catch (e) {
      alert("Helper install failed: " + e);
    } finally {
      setTunBusy(false);
    }
  }

  async function handleUninstallHelper() {
    if (!confirm("Uninstall the privileged VPN helper? Lumen will fall back to slower system proxy mode.")) return;
    setTunBusy(true);
    try {
      await invoke("tun_uninstall_helper");
      await refreshTunStatus();
    } catch (e) {
      alert("Helper uninstall failed: " + e);
    } finally {
      setTunBusy(false);
    }
  }

  async function handleRepairNetwork() {
    setRepairBusy(true);
    setRepairStatus(null);
    try {
      const result = await tauri.repairNetwork();
      localStorage.setItem(CONNECTION_INTENT_KEY, "disconnected");
      setRepairStatus(repairNetworkMessage(result));
      await refreshTunStatus();
      await refreshDiagnostics();
    } catch (e) {
      setRepairStatus("Repair needs attention");
      console.error("Repair network failed:", e);
    } finally {
      setRepairBusy(false);
    }
  }

  async function refreshDiagnostics() {
    setDiagnosticsBusy(true);
    try {
      const next = await tauri.networkDiagnostics();
      setDiagnostics(next);
      return next;
    } catch (e) {
      console.warn("network diagnostics:", e);
      const fallback = {
        effective_status: "disconnected",
        helper_installed: false,
        helper_running: false,
        tun_running: false,
        external_ip: null,
        region: null,
        country: null,
        asn_org: null,
        error: String(e),
      };
      setDiagnostics(fallback);
      return fallback;
    } finally {
      setDiagnosticsBusy(false);
    }
  }

  async function handleCopyDiagnostics() {
    setDiagnosticsCopyStatus(null);
    try {
      const snapshot = diagnostics ?? (await refreshDiagnostics());
      await navigator.clipboard.writeText(formatDiagnosticsSnapshot(snapshot));
      setDiagnosticsCopyStatus("Copied");
      setTimeout(() => setDiagnosticsCopyStatus(null), 2000);
    } catch (e) {
      console.error("copy diagnostics:", e);
      setDiagnosticsCopyStatus("Copy failed");
    }
  }

  useEffect(() => {
    localStorage.setItem("lumen-theme", theme);
    if (theme === "system") {
      const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
      document.documentElement.setAttribute("data-theme", prefersDark ? "dark" : "light");
    } else {
      document.documentElement.setAttribute("data-theme", theme);
    }
  }, [theme]);

  void accessKey; void copied; void setCopied;

  return (
    <div className="settings">
      <div className="settings__header">
        <h2 className="settings__title">Settings</h2>
      </div>

      <div className="settings__sections">
        {/* Profiles (Keys) */}
        <ProfilesSection keyStore={keyStore} accessKey={accessKey} />

        {/* Appearance */}
        <section className="settings__section">
          <h3 className="settings__section-title">Appearance</h3>
          <div className="settings__segmented">
            {(["system", "light", "dark"] as ThemeOption[]).map((opt) => (
              <button
                key={opt}
                className={`settings__seg-btn ${theme === opt ? "active" : ""}`}
                onClick={() => setTheme(opt)}
              >
                {opt.charAt(0).toUpperCase() + opt.slice(1)}
              </button>
            ))}
          </div>
        </section>

        {/* Connection */}
        <section className="settings__section">
          <h3 className="settings__section-title">Connection</h3>
          <label className="settings__toggle">
            <input
              type="checkbox"
              checked={launchAtLogin}
              onChange={(e) => setLaunchAtLogin(e.target.checked)}
            />
            <span className="settings__toggle-label">Launch at login</span>
          </label>
          <label className="settings__toggle">
            <input
              type="checkbox"
              checked={autoConnect}
              onChange={(e) => setAutoConnect(e.target.checked)}
            />
            <span className="settings__toggle-label">Auto-connect on launch</span>
          </label>
          <label className="settings__toggle">
            <input
              type="checkbox"
              checked={killSwitch}
              onChange={(e) => setKillSwitch(e.target.checked)}
            />
            <span className="settings__toggle-label">Kill switch</span>
          </label>
        </section>

        {/* VPN Mode */}
        <section className="settings__section">
          <h3 className="settings__section-title">VPN Mode</h3>

          {/* Mode selector — visible when helper is available */}
          {tunStatus?.helper_installed && tunStatus?.helper_running && (
            <div className="settings__segmented" style={{ marginBottom: 12 }}>
              {(["tun", "proxy"] as VpnMode[]).map((opt) => (
                <button
                  key={opt}
                  className={`settings__seg-btn ${vpnMode === opt ? "active" : ""}`}
                  onClick={() => setVpnModeAndPersist(opt)}
                >
                  {opt === "tun" ? "TUN (fast)" : "System Proxy"}
                </button>
              ))}
            </div>
          )}

          {/* Status + actions */}
          {tunStatus?.helper_installed && tunStatus?.helper_running ? (
            vpnMode === "tun" ? (
              <>
                <p className="settings__info">
                  <span className="settings__mode-icon" aria-hidden="true">
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><polyline points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>
                  </span>
                  <strong>TUN mode</strong> — kernel routing, low latency, all apps & UDP covered.
                </p>
                <div className="settings__actions">
                  <button className="settings__action-btn" onClick={handleUninstallHelper} disabled={tunBusy}>
                    {tunBusy ? "Working..." : "Uninstall Helper"}
                  </button>
                </div>
              </>
            ) : (
              <>
                <p className="settings__info">
                  <span className="settings__mode-icon" aria-hidden="true">
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
                  </span>
                  <strong>System Proxy mode</strong> — works without root, but slower (~2× higher latency).
                </p>
                <div className="settings__actions">
                  <button className="settings__action-btn" onClick={handleUninstallHelper} disabled={tunBusy}>
                    {tunBusy ? "Working..." : "Uninstall Helper"}
                  </button>
                </div>
              </>
            )
          ) : tunStatus?.helper_installed ? (
            <p className="settings__info">
              <span className="settings__mode-icon" aria-hidden="true">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M10.29 3.86L1.82 18a2 2 0 001.71 3h16.94a2 2 0 001.71-3L13.71 3.86a2 2 0 00-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>
              </span>
              Helper installed but not running. Try restarting Lumen.
            </p>
          ) : (
            <>
              <p className="settings__info">
                <span className="settings__mode-icon" aria-hidden="true">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
                </span>
                <strong>System Proxy mode</strong> (~2× slower). Install VPN helper for TUN mode and lower latency.
              </p>
              <div className="settings__actions">
                <button className="settings__action-btn" onClick={handleInstallHelper} disabled={tunBusy}>
                  {tunBusy ? "Installing..." : "Install VPN Helper"}
                </button>
              </div>
            </>
          )}
        </section>

        {/* Advanced */}
        <section className="settings__section">
          <h3 className="settings__section-title">Advanced</h3>
          <p className="settings__info">Config updated: just now</p>
          <div className="settings__actions">
            <button className="settings__action-btn">Refresh Config</button>
            <button className="settings__action-btn" onClick={onViewLogs}>View Logs</button>
          </div>
        </section>

        <section className="settings__section">
          <h3 className="settings__section-title">Network Repair</h3>
          <p className="settings__info">
            Stops Lumen proxy and TUN processes, clears Lumen proxy environment, and returns traffic to the current Wi-Fi route.
          </p>
          <div className="settings__actions settings__actions--stack">
            <button className="settings__action-btn" onClick={handleRepairNetwork} disabled={repairBusy}>
              {repairBusy ? "Repairing..." : "Repair Network"}
            </button>
            {repairStatus && <span className="settings__repair-status">{repairStatus}</span>}
          </div>
        </section>

        <section className="settings__section">
          <div className="settings__section-head">
            <h3 className="settings__section-title">Diagnostics</h3>
            <div className="settings__mini-actions">
              <button className="settings__update-btn" onClick={handleCopyDiagnostics} disabled={diagnosticsBusy}>
                Copy Snapshot
              </button>
              <button className="settings__update-btn" onClick={refreshDiagnostics} disabled={diagnosticsBusy}>
                {diagnosticsBusy ? "Checking..." : "Refresh"}
              </button>
            </div>
          </div>
          <div className="settings__diagnostics-grid">
            <span>Route</span>
            <strong>{diagnosticRouteLabel(diagnostics?.effective_status ?? "disconnected")}</strong>
            <span>External IP</span>
            <strong>{diagnostics?.external_ip ?? "Unknown"}</strong>
            <span>Location</span>
            <strong>{diagnostics ? diagnosticLocationLabel(diagnostics) : "Unknown"}</strong>
            <span>Provider</span>
            <strong>{diagnostics?.asn_org ?? "Unknown"}</strong>
            <span>Helper</span>
            <strong>
              {diagnostics?.helper_running ? "Running" : diagnostics?.helper_installed ? "Installed" : "Not installed"}
            </strong>
            <span>TUN</span>
            <strong>{diagnostics?.tun_running ? "Running" : "Stopped"}</strong>
          </div>
          {diagnosticsCopyStatus && <p className="settings__copy-status">{diagnosticsCopyStatus}</p>}
        </section>

        {/* About */}
        <section className="settings__section settings__section--footer">
          <p className="settings__version">Lumen v2.4.0</p>
          <button className="settings__action-btn">Check for Updates</button>
          <button className="settings__logout-btn" onClick={onClearKey}>
            Sign Out
          </button>
        </section>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────
// Profiles (Keys) management section
// ─────────────────────────────────────────────────
interface ProfilesSectionProps {
  keyStore: ReturnType<typeof useKeyStore>;
  accessKey: string;
}

function ProfilesSection({ keyStore }: ProfilesSectionProps) {
  const [adding, setAdding] = useState(false);
  const [newKey, setNewKey] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  function badgeFor(type: string): string {
    if (type === "vless") return "VLESS";
    if (type === "subscription_url") return "URL";
    return "Proteus";
  }

  function flagFor(type: string): React.ReactNode {
    if (type === "vless") {
      return (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 014 10 15.3 15.3 0 01-4 10 15.3 15.3 0 01-4-10 15.3 15.3 0 014-10z"/></svg>
      );
    }
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><polygon points="12 2 22 12 12 22 2 12 12 2"/></svg>
    );
  }

  function handleAdd() {
    if (!newKey.trim()) return;
    const k = keyStore.addKey(newKey);
    if (k) {
      setNewKey("");
      setAdding(false);
    }
  }

  function startRename(id: string, currentName: string) {
    setRenamingId(id);
    setRenameValue(currentName);
  }

  function saveRename() {
    if (renamingId) {
      keyStore.renameKey(renamingId, renameValue);
      setRenamingId(null);
    }
  }

  function handleDelete(id: string) {
    if (confirmDeleteId === id) {
      keyStore.removeKey(id);
      setConfirmDeleteId(null);
    } else {
      setConfirmDeleteId(id);
      setTimeout(() => setConfirmDeleteId((prev) => (prev === id ? null : prev)), 3000);
    }
  }

  const detected = newKey.trim() ? detectType(newKey) : null;

  return (
    <section className="settings__section">
      <div className="settings__section-head">
        <h3 className="settings__section-title">Profiles ({keyStore.keys.length})</h3>
        <button
          className="settings__icon-btn"
          onClick={() => setAdding((a) => !a)}
          aria-label={adding ? "Cancel" : "Add profile"}
        >
          {adding ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>
          )}
        </button>
      </div>

      {adding && (
        <div className="profile__add">
          <textarea
            className="profile__add-input"
            value={newKey}
            onChange={(e) => setNewKey(e.target.value)}
            placeholder="vless://… or Proteus key"
            rows={2}
            autoFocus
          />
          {detected && (
            <p className="profile__add-hint">
              {detected === "vless" ? "VLESS link" : detected === "subscription_url" ? "Subscription URL" : "Proteus key"}
              {newKey.trim() && ` · ${defaultNameFor(newKey, detected)}`}
            </p>
          )}
          <div className="settings__actions">
            <button className="settings__action-btn" onClick={handleAdd} disabled={!newKey.trim()}>
              Add
            </button>
            <button className="settings__action-btn" onClick={() => { setAdding(false); setNewKey(""); }}>
              Cancel
            </button>
          </div>
        </div>
      )}

      <div className="profile__list">
        {keyStore.keys.map((k) => {
          const isActive = keyStore.activeId === k.id;
          const isRenaming = renamingId === k.id;
          const isConfirmDelete = confirmDeleteId === k.id;
          return (
            <div key={k.id} className={`profile__row ${isActive ? "active" : ""}`}>
              <button
                className="profile__select-dot"
                onClick={() => keyStore.setActive(k.id)}
                aria-label={isActive ? "Active" : "Set active"}
              >
                <span className={`profile__dot ${isActive ? "filled" : ""}`} />
              </button>
              <span className="profile__icon" aria-hidden="true">{flagFor(k.type)}</span>
              {isRenaming ? (
                <input
                  className="profile__name-input"
                  value={renameValue}
                  onChange={(e) => setRenameValue(e.target.value)}
                  onBlur={saveRename}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") saveRename();
                    if (e.key === "Escape") setRenamingId(null);
                  }}
                  autoFocus
                />
              ) : (
                <span
                  className="profile__name"
                  onDoubleClick={() => startRename(k.id, k.name)}
                  title="Double-click to rename"
                >
                  {k.name}
                </span>
              )}
              <span className="profile__badge">{badgeFor(k.type)}</span>
              <button
                className="profile__action-btn"
                onClick={() => startRename(k.id, k.name)}
                title="Rename"
              >
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M11 4H4a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 013 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>
              </button>
              <button
                className={`profile__action-btn ${isConfirmDelete ? "danger" : ""}`}
                onClick={() => handleDelete(k.id)}
                title={isConfirmDelete ? "Tap again to confirm" : "Delete"}
              >
                {isConfirmDelete ? (
                  <span style={{ fontSize: 11, fontWeight: 600 }}>Confirm?</span>
                ) : (
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-2 14a2 2 0 01-2 2H9a2 2 0 01-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/></svg>
                )}
              </button>
            </div>
          );
        })}
        {keyStore.keys.length === 0 && (
          <p className="settings__info" style={{ textAlign: "center", padding: "16px 0" }}>
            No profiles yet. Click + to add.
          </p>
        )}
      </div>
    </section>
  );
}
