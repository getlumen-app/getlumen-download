import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./design-tokens.css";
import App from "./App";

/**
 * Theme logic:
 *   Storage value: "light" | "dark" | "system"
 *   Default (no storage): "dark"
 *   "system" follows prefers-color-scheme (dynamic)
 */
function resolveTheme(): "light" | "dark" {
  const saved = localStorage.getItem("lumen-theme");
  if (saved === "light") return "light";
  if (saved === "dark") return "dark";
  if (saved === "system") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  // Default — dark
  return "dark";
}

document.documentElement.setAttribute("data-theme", resolveTheme());

// Only follow system changes when user explicitly picked "system"
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (e) => {
  if (localStorage.getItem("lumen-theme") === "system") {
    document.documentElement.setAttribute("data-theme", e.matches ? "dark" : "light");
  }
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
