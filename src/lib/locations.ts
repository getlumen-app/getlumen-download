/** Friendly geo labels for Home picker + Proxies tab (private Lumen). */

export interface LocationOption {
  /** Clash/sing-box outbound or group tag */
  tag: string;
  label: string;
  flag: string;
}

/** Ordered Home location sheet. Auto first, then geo pins.
 *  Tags must match real Proteus outbounds — stale entries can't be pinned. */
export const LOCATION_OPTIONS: LocationOption[] = [
  { tag: "proxy-auto", label: "Auto", flag: "⚡" },
  { tag: "hostodo-via-firstbyte", label: "USA · Hostodo", flag: "🇺🇸" },
  { tag: "hostodo-via-timeweb", label: "USA · Hostodo (Timeweb)", flag: "🇺🇸" },
  { tag: "dubai-residential", label: "Dubai", flag: "🇦🇪" },
  { tag: "dubai-via-firstbyte", label: "Dubai · FirstByte", flag: "🇦🇪" },
  { tag: "relay-eu-grpc", label: "Germany · Netcup gRPC", flag: "🇷🇺→🇩🇪" },
  { tag: "firstbyte-relay-httpupgrade", label: "Germany · FirstByte", flag: "🇷🇺→🇩🇪" },
  { tag: "firstbyte-995-httpupgrade", label: "Germany · FirstByte :995", flag: "🇷🇺→🇩🇪" },
  { tag: "izhevsk-via-netcup", label: "Izhevsk · Netcup", flag: "🇷🇺" },
  { tag: "izhevsk-via-firstbyte", label: "Izhevsk · FirstByte", flag: "🇷🇺" },
];

const BY_TAG: Record<string, LocationOption> = Object.fromEntries(
  LOCATION_OPTIONS.map((o) => [o.tag, o])
);

/** Extra leaf labels that appear in Proxies but are not Home geo rows. */
const EXTRA_FLAGS: Record<string, string> = {
  "izhevsk-telemost": "🇷🇺",
  "firstbyte-tm-telemost": "🇷🇺",
  "netcup-tcp-reality": "🇩🇪",
  "netcup-grpc-reality": "🇩🇪",
};

const EXTRA_LABELS: Record<string, string> = {
  "izhevsk-telemost": "Izhevsk · Telemost",
  "firstbyte-tm-telemost": "FirstByte · Telemost",
  "netcup-tcp-reality": "Frankfurt Direct",
  "netcup-grpc-reality": "Frankfurt gRPC",
};

export const GROUP_LABELS: Record<string, string> = {
  proxy: "Location",
  "proxy-auto": "Auto Select",
  "proxy-tg": "Messengers",
  "proxy-yt": "YouTube",
  "messenger-auto": "Messengers",
  "telegram-cdn": "Telegram",
  "twitch-auto": "Twitch",
  "hostodo-relay-auto": "Hostodo Relay",
  "whitelist-auto": "Whitelist",
};

export const LOCATION_PREFERENCE_KEY = "lumen-location-tag";

export function locationLabel(tag: string): string {
  return BY_TAG[tag]?.label || EXTRA_LABELS[tag] || tag;
}

export function locationFlag(tag: string): string {
  return BY_TAG[tag]?.flag || EXTRA_FLAGS[tag] || "🌍";
}

export function readStoredLocation(): string {
  try {
    const v = localStorage.getItem(LOCATION_PREFERENCE_KEY);
    return v && v.trim() ? v.trim() : "proxy-auto";
  } catch {
    return "proxy-auto";
  }
}

export function writeStoredLocation(tag: string): void {
  try {
    localStorage.setItem(LOCATION_PREFERENCE_KEY, tag || "proxy-auto");
  } catch {
    // ignore quota / private mode
  }
}

/** Locations available from the current selector group's node list. */
export function availableLocations(
  selectorNodes: { name: string }[] | undefined
): LocationOption[] {
  if (!selectorNodes || selectorNodes.length === 0) {
    return [LOCATION_OPTIONS[0]];
  }
  const names = new Set(selectorNodes.map((n) => n.name));
  return LOCATION_OPTIONS.filter((o) => names.has(o.tag));
}
