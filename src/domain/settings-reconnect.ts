import type { MessageKey } from "../i18n/index.ts";

export const SETTINGS_RECONNECT_KIND_KEYS = {
  stay: "保持当前页面",
  "manual-recovery": "在新端口打开当前页面",
} as const satisfies Record<"stay" | "manual-recovery", MessageKey>;

export type SettingsReconnectKind = keyof typeof SETTINGS_RECONNECT_KIND_KEYS;

export interface SettingsReconnectPlan {
  kind: SettingsReconnectKind;
  href: string;
}

/**
 * Manual port recovery only. Equality of the gateway port never navigates;
 * the caller may show `href` as a user-activated link.
 */
export function planSettingsReconnect(input: {
  href: string;
  previousGatewayPort: number;
  nextGatewayPort: number;
  dev: boolean;
}): SettingsReconnectPlan {
  const { href, previousGatewayPort, nextGatewayPort, dev } = input;
  if (dev || previousGatewayPort === nextGatewayPort) return { kind: "stay", href };
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return { kind: "stay", href };
  }
  if (!isLoopbackHost(url.hostname) || explicitPort(url) !== previousGatewayPort) {
    return { kind: "stay", href };
  }
  url.port = String(nextGatewayPort);
  return { kind: "manual-recovery", href: url.href };
}

/** An empty URL port is the scheme default, not a gateway port. */
function explicitPort(url: URL): number | null {
  if (!url.port) return null;
  const port = Number(url.port);
  return Number.isInteger(port) ? port : null;
}

function isLoopbackHost(hostname: string): boolean {
  const host = hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (host === "localhost" || host === "::1") return true;
  const parts = host.split(".");
  if (parts.length !== 4) return false;
  const octets = parts.map((part) => (/^\d+$/.test(part) ? Number(part) : NaN));
  return octets[0] === 127 && octets.every((octet) => octet >= 0 && octet <= 255);
}
