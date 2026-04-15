import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./Logs.css";

interface Props {
  onClose: () => void;
}

const IS_TAURI = "__TAURI_INTERNALS__" in window;

export default function Logs({ onClose }: Props) {
  const [logs, setLogs] = useState<string[]>([]);
  const [autoScroll, setAutoScroll] = useState(true);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    // Poll log file via Tauri command
    const fetchLogs = async () => {
      try {
        if (IS_TAURI) {
          const lines = await invoke<string[]>("get_logs");
          setLogs(lines);
        } else {
          setLogs(["Running in browser — logs available in Tauri app only."]);
        }
      } catch (e) {
        setLogs([`Error fetching logs: ${e}`]);
      }
    };

    fetchLogs();
    const interval = setInterval(fetchLogs, 2000);
    return () => clearInterval(interval);
  }, []);

  useEffect(() => {
    if (autoScroll && logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [logs, autoScroll]);

  function handleDownload() {
    const blob = new Blob([logs.join("\n")], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `lumen-logs-${new Date().toISOString().slice(0, 19)}.txt`;
    a.click();
    URL.revokeObjectURL(url);
  }

  function handleCopy() {
    navigator.clipboard.writeText(logs.join("\n"));
  }

  return (
    <div className="logs-overlay">
      <div className="logs-panel">
        <div className="logs-header">
          <h2 className="logs-title">Logs</h2>
          <div className="logs-actions">
            <button className="logs-btn" onClick={handleCopy}>Copy</button>
            <button className="logs-btn" onClick={handleDownload}>Download</button>
            <button className="logs-btn logs-btn--close" onClick={onClose}>Close</button>
          </div>
        </div>
        <div className="logs-content" ref={logRef}>
          {logs.length === 0 ? (
            <p className="logs-empty">No logs yet. Connect to start capturing.</p>
          ) : (
            logs.map((line, i) => {
              let cls = "logs-line";
              if (line.includes("ERROR") || line.includes("FATAL")) cls += " logs-line--error";
              else if (line.includes("WARN")) cls += " logs-line--warn";
              else if (line.includes("INFO")) cls += " logs-line--info";
              return <div key={i} className={cls}>{line}</div>;
            })
          )}
        </div>
        <div className="logs-footer">
          <label className="logs-auto">
            <input
              type="checkbox"
              checked={autoScroll}
              onChange={(e) => setAutoScroll(e.target.checked)}
            />
            Auto-scroll
          </label>
          <span className="logs-count">{logs.length} lines</span>
        </div>
      </div>
    </div>
  );
}
