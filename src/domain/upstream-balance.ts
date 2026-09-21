import type { Account } from "../api/dashboard.ts";
import type { Connection } from "../api/connections.ts";
import type { Identity } from "../api/identities.ts";

const STEP_FUN_API_HOST = "api.stepfun.com";

function isStepFunPlanPath(pathname: string): boolean {
  return pathname === "/step_plan" || pathname.startsWith("/step_plan/");
}

/**
 * Official hosts that publish a Key-authenticated current-balance endpoint.
 * Exact hostname match only; never suffix matching.
 */
export const OFFICIAL_BALANCE_HOSTS = new Set([
  "api.deepseek.com",
  "api.moonshot.cn",
  "api.moonshot.ai",
]);

export function officialBalanceSupported(endpointUrl: string | null | undefined): boolean {
  const trimmed = endpointUrl?.trim();
  if (!trimmed) return false;
  try {
    const parsed = new URL(trimmed);
    const host = parsed.hostname.toLowerCase();
    if (host === STEP_FUN_API_HOST) {
      if (parsed.protocol !== "https:") return false;
      if (parsed.username !== "" || parsed.password !== "") return false;
      if (parsed.search !== "" || parsed.hash !== "") return false;
      if (parsed.port !== "" && parsed.port !== "443") return false;
      return !isStepFunPlanPath(parsed.pathname);
    }
    return OFFICIAL_BALANCE_HOSTS.has(host);
  } catch {
    return false;
  }
}

/**
 * Custom accounts own their Endpoint. User-defined Provider accounts inherit
 * the connection Endpoint when the identity overlay is present.
 */
export function accountInferenceEndpointUrl(
  account: Pick<Account, "custom_config">,
  identity: Identity | null | undefined,
  connections: readonly Connection[] | null | undefined,
): string | null {
  const custom = account.custom_config?.endpoint_url?.trim();
  if (custom) return custom;
  if (!identity || !connections?.length) return null;
  const credential = identity.credentials.find((row) => row.legacy.id === identity.legacy.id)
    ?? identity.credentials[0];
  const connectionId = credential?.bindings[0]?.connection_id;
  if (!connectionId) return null;
  const connection = connections.find((row) => row.id === connectionId);
  const url = connection?.endpoints.find((endpoint) => endpoint.url?.trim())?.url?.trim();
  return url || null;
}
