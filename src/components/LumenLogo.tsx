interface Props {
  size?: number;
  className?: string;
}

/**
 * Lumen logo — sun with rays (matches app icon).
 * Uses currentColor so parent CSS controls tone.
 */
export default function LumenLogo({ size = 48, className }: Props) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 48 48"
      fill="none"
      className={className}
      xmlns="http://www.w3.org/2000/svg"
    >
      {/* Outer rays (faint) */}
      <g stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" opacity="0.4">
        <line x1="24" y1="4" x2="24" y2="8" />
        <line x1="24" y1="40" x2="24" y2="44" />
        <line x1="4" y1="24" x2="8" y2="24" />
        <line x1="40" y1="24" x2="44" y2="24" />
        <line x1="10" y1="10" x2="13" y2="13" />
        <line x1="35" y1="35" x2="38" y2="38" />
        <line x1="38" y1="10" x2="35" y2="13" />
        <line x1="13" y1="35" x2="10" y2="38" />
      </g>

      {/* Outer ring */}
      <circle cx="24" cy="24" r="10" fill="none" stroke="currentColor" strokeWidth="1.2" opacity="0.4" />

      {/* Main sun body */}
      <circle cx="24" cy="24" r="8" fill="currentColor" opacity="0.14" />
      <circle cx="24" cy="24" r="8" fill="none" stroke="currentColor" strokeWidth="1.8" />

      {/* Inner glow */}
      <circle cx="24" cy="24" r="4" fill="currentColor" opacity="0.24" />
      <circle cx="24" cy="24" r="2" fill="currentColor" opacity="0.4" />

      {/* Center dot */}
      <circle cx="24" cy="24" r="0.9" fill="currentColor" />
    </svg>
  );
}
