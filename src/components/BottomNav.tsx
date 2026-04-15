import "./BottomNav.css";

type Tab = "home" | "proxies" | "settings";

interface Props {
  active: Tab;
  onChange: (tab: Tab) => void;
}

const tabs: { id: Tab; label: string; icon: string }[] = [
  { id: "home", label: "Home", icon: "⏻" },
  { id: "proxies", label: "Proxies", icon: "◉" },
  { id: "settings", label: "Settings", icon: "⚙" },
];

export default function BottomNav({ active, onChange }: Props) {
  return (
    <nav className="bottom-nav">
      {tabs.map((t) => (
        <button
          key={t.id}
          className={`bottom-nav__item ${active === t.id ? "active" : ""}`}
          onClick={() => onChange(t.id)}
        >
          <span className="bottom-nav__icon">{t.icon}</span>
          <span className="bottom-nav__label">{t.label}</span>
        </button>
      ))}
    </nav>
  );
}
