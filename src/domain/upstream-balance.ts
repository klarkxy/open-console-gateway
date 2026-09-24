import type { Account } from "../api/dashboard.ts";
import type { Connection } from "../api/connections.ts";
import type { Identity } from "../api/identities.ts";
import { endpointMatchesSavedGrant } from "./account-credential.ts";
import { selectedInferenceBinding } from "./account-identity.ts";

/** Financial support comes from the server's classification of the full URL. */
export function officialBalanceSupported(
  endpointUrl: string | null | undefined,
  connections: readonly Connection[] | null | undefined,
): boolean {
  const url = endpointUrl?.trim();
  if (!url) return false;
  const endpoints = connections?.flatMap((connection) => connection.endpoints)
    .filter((endpoint) => endpoint.url?.trim() === url) ?? [];
  return endpoints.length > 0 && endpoints.every((endpoint) => endpoint.official_balance === true);
}

/** Exact account binding only. Ambiguous or revoked endpoint selection is unknown. */
export function accountInferenceEndpointUrl(
  account: Pick<Account, "id" | "custom_config">,
  identity: Identity | null | undefined,
  connections: readonly Connection[] | null | undefined,
): string | null {
  const custom = account.custom_config?.endpoint_url?.trim();
  if (custom) return custom;
  const binding = selectedInferenceBinding(identity ?? null, account.id);
  if (!binding || !connections) return null;
  const connection = connections.find((row) => row.id === binding.connection_id);
  if (!connection) return null;
  const urls = new Set<string>();
  for (const endpoint of connection.endpoints) {
    const url = endpoint.url?.trim();
    if (!url || !endpointMatchesSavedGrant(endpoint, binding)) continue;
    urls.add(url);
  }
  return urls.size === 1 ? [...urls][0]! : null;
}
