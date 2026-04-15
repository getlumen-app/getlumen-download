import { useState, useMemo, useRef, useEffect } from "react";
import LumenLogo from "../components/LumenLogo";
import { detectType, defaultNameFor } from "../hooks/useKeyStore";
import { openUrl } from "../hooks/useTauri";
import "./KeyInput.css";

interface Props {
  onSubmit: (key: string) => void;
}

const TELEGRAM_BOT_URL = "https://t.me/ProteusKeyBot";

export default function KeyInput({ onSubmit }: Props) {
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Auto-grow textarea height based on content (no manual resize drag).
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    const next = Math.min(Math.max(el.scrollHeight, 44), 220);
    el.style.height = `${next}px`;
  }, [key]);

  const detected = useMemo(() => {
    const v = key.trim();
    if (!v) return null;
    const type = detectType(v);
    if (type === "vless") {
      const valid = /vless:\/\/[^@]+@[^:/?#]+:\d+/.test(v);
      return {
        type,
        valid,
        label: valid ? `VLESS · ${defaultNameFor(v, type)}` : "Looks like VLESS but malformed",
      };
    }
    if (type === "subscription_url") {
      try {
        const host = new URL(v).hostname;
        return { type, valid: true, label: `Subscription URL · ${host}` };
      } catch {
        return { type, valid: false, label: "Malformed URL" };
      }
    }
    return {
      type,
      valid: v.length >= 4,
      label: v.length < 4 ? "Too short" : "Subscription key",
    };
  }, [key]);

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const trimmed = key.trim();
    if (!trimmed) {
      setError("Please enter your access key");
      return;
    }
    if (!detected?.valid) {
      setError(detected?.label || "Invalid key format");
      return;
    }
    setError("");
    onSubmit(trimmed);
  }

  function handleTelegramClick(e: React.MouseEvent<HTMLAnchorElement>) {
    e.preventDefault();
    openUrl(TELEGRAM_BOT_URL);
  }

  return (
    <div className="key-input">
      <div className="key-input__logo">
        <LumenLogo size={56} />
      </div>

      <h1 className="key-input__title">Welcome to Lumen</h1>
      <p className="key-input__subtitle">Fast, private internet access</p>

      <form className="key-input__form" onSubmit={handleSubmit}>
        <label className="key-input__label" htmlFor="lumen-key-field">
          Subscription key or VLESS link
        </label>
        <textarea
          ref={textareaRef}
          id="lumen-key-field"
          className={`key-input__textarea ${error ? "error" : ""}`}
          value={key}
          onChange={(e) => {
            setKey(e.target.value);
            setError("");
          }}
          placeholder="vless://… or your Proteus key"
          autoFocus
          spellCheck={false}
          autoComplete="off"
          rows={1}
        />
        {detected && !error && (
          <p className={`key-input__hint-detect ${detected.valid ? "" : "warn"}`}>
            <span className="key-input__hint-icon" aria-hidden="true">
              {detected.valid ? (
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><polyline points="20 6 9 17 4 12"/></svg>
              ) : (
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
              )}
            </span>
            {detected.label}
          </p>
        )}
        {error && <p className="key-input__error">{error}</p>}
        <button className="key-input__submit" type="submit">
          Connect
        </button>
      </form>

      <div className="key-input__divider">
        <span>or</span>
      </div>

      <a
        className="key-input__telegram"
        href={TELEGRAM_BOT_URL}
        target="_blank"
        rel="noopener noreferrer"
        onClick={handleTelegramClick}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
          <path d="M21.5 2.5L2.5 10.5l5.5 2 2 5.5 2.5-3 5 4 4-16.5z"/>
        </svg>
        Get a free key on Telegram
      </a>
      <p className="key-input__hint">@ProteusKeyBot · sends instructions in chat</p>
    </div>
  );
}
