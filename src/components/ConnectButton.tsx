import "./ConnectButton.css";

type State = "disconnected" | "connecting" | "connected";

interface Props {
  state: State;
  onClick: () => void;
}

export default function ConnectButton({ state, onClick }: Props) {
  return (
    <button
      className={`connect-btn connect-btn--${state}`}
      onClick={onClick}
      aria-label={state === "connected" ? "Disconnect" : "Connect"}
    >
      <svg
        className="connect-btn__icon"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
      >
        {/* Power icon */}
        <path d="M18.36 6.64a9 9 0 1 1-12.73 0" />
        <line x1="12" y1="2" x2="12" y2="12" />
      </svg>
      {state === "connecting" && <div className="connect-btn__pulse" />}
    </button>
  );
}
