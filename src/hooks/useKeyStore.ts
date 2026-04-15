/// Multi-key store — backed by localStorage.
/// Migrates legacy single-key (`lumen-key`) on first read.
import { useState, useCallback, useEffect } from "react";

export type KeyType = "proteus" | "vless" | "subscription_url";

export interface SavedKey {
  id: string;
  name: string;
  type: KeyType;
  value: string;
  createdAt: number;
}

const KEYS_STORAGE = "lumen-keys";
const ACTIVE_STORAGE = "lumen-active-key-id";
const LEGACY_STORAGE = "lumen-key";

function genId(): string {
  return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
}

export function detectType(raw: string): KeyType {
  const s = raw.trim();
  if (s.startsWith("vless://")) return "vless";
  if (s.startsWith("https://") || s.startsWith("http://")) return "subscription_url";
  return "proteus";
}

/** Best-effort: extract display name (fragment) from vless URI; or short id for others. */
export function defaultNameFor(raw: string, type: KeyType): string {
  const s = raw.trim();
  if (type === "vless") {
    const hash = s.indexOf("#");
    if (hash > 0) {
      const frag = s.slice(hash + 1);
      try {
        return decodeURIComponent(frag) || "VLESS";
      } catch {
        return frag || "VLESS";
      }
    }
    // host:port from vless://uuid@host:port
    const match = s.match(/vless:\/\/[^@]+@([^:/?#]+)/);
    return match ? match[1] : "VLESS";
  }
  if (type === "subscription_url") {
    try {
      return new URL(s).hostname;
    } catch {
      return "Subscription";
    }
  }
  // Proteus: 4-char prefix + last 4
  if (s.length > 8) return `Proteus ${s.slice(0, 4)}…${s.slice(-4)}`;
  return "Proteus key";
}

function loadAll(): { keys: SavedKey[]; activeId: string | null } {
  // Try multi-key storage
  let keys: SavedKey[] = [];
  try {
    const raw = localStorage.getItem(KEYS_STORAGE);
    if (raw) keys = JSON.parse(raw);
  } catch {}

  // Migrate legacy single key
  if (keys.length === 0) {
    const legacy = localStorage.getItem(LEGACY_STORAGE);
    if (legacy) {
      const type = detectType(legacy);
      const k: SavedKey = {
        id: genId(),
        name: defaultNameFor(legacy, type),
        type,
        value: legacy,
        createdAt: Date.now(),
      };
      keys = [k];
      localStorage.setItem(KEYS_STORAGE, JSON.stringify(keys));
      localStorage.setItem(ACTIVE_STORAGE, k.id);
      // keep legacy in place for backwards safety; will be removed next session
    }
  }

  let activeId = localStorage.getItem(ACTIVE_STORAGE);
  if (activeId && !keys.find((k) => k.id === activeId)) {
    activeId = keys[0]?.id ?? null;
  }
  if (!activeId && keys.length > 0) {
    activeId = keys[0].id;
  }

  return { keys, activeId };
}

function persist(keys: SavedKey[], activeId: string | null) {
  localStorage.setItem(KEYS_STORAGE, JSON.stringify(keys));
  if (activeId) localStorage.setItem(ACTIVE_STORAGE, activeId);
  else localStorage.removeItem(ACTIVE_STORAGE);
}

export function useKeyStore() {
  const [{ keys, activeId }, setState] = useState(() => loadAll());

  // Sync to localStorage on every change
  useEffect(() => {
    persist(keys, activeId);
  }, [keys, activeId]);

  const activeKey = keys.find((k) => k.id === activeId) ?? null;

  const addKey = useCallback((value: string, name?: string): SavedKey | null => {
    const v = value.trim();
    if (!v) return null;
    const type = detectType(v);
    const k: SavedKey = {
      id: genId(),
      name: name?.trim() || defaultNameFor(v, type),
      type,
      value: v,
      createdAt: Date.now(),
    };
    setState((prev) => {
      const next = [...prev.keys, k];
      return { keys: next, activeId: prev.activeId ?? k.id };
    });
    return k;
  }, []);

  const removeKey = useCallback((id: string) => {
    setState((prev) => {
      const next = prev.keys.filter((k) => k.id !== id);
      const newActive = prev.activeId === id ? next[0]?.id ?? null : prev.activeId;
      return { keys: next, activeId: newActive };
    });
  }, []);

  const renameKey = useCallback((id: string, name: string) => {
    setState((prev) => ({
      ...prev,
      keys: prev.keys.map((k) => (k.id === id ? { ...k, name: name.trim() || k.name } : k)),
    }));
  }, []);

  const setActive = useCallback((id: string) => {
    setState((prev) => ({ ...prev, activeId: id }));
  }, []);

  const clearAll = useCallback(() => {
    setState({ keys: [], activeId: null });
    localStorage.removeItem(LEGACY_STORAGE);
  }, []);

  return {
    keys,
    activeKey,
    activeId,
    addKey,
    removeKey,
    renameKey,
    setActive,
    clearAll,
  };
}
