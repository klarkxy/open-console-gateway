import type {
  Account,
  AccountModelCapabilityInput,
  AccountProtocol,
} from "../api/dashboard.ts";
import type { Destination, DestinationCredential } from "../api/destinations.ts";

/**
 * Custom API accounts are administrator-trusted endpoints: the UI accepts any
 * backend-valid http:// or https:// API URL, including LAN,
 * localhost, and metadata addresses. Client-side validation only rejects
 * malformed input, non-http(s) schemes, and URL-embedded credentials.
 */
const CUSTOM_PROVIDER_ID = "custom";

export function isCustomApiAccount(
  account: Pick<Account, "provider_id">,
): boolean {
  return account.provider_id === CUSTOM_PROVIDER_ID;
}

export type CustomEndpointUrlIssue = "empty" | "malformed" | "not_http" | "with_credentials";

export const CUSTOM_ENDPOINT_URL_ISSUE_KEYS = {
  empty: "填写 API 地址",
  malformed: "Endpoint 格式无效",
  not_http: "Endpoint 必须是 http:// 或 https:// URL",
  with_credentials: "Endpoint 不能包含用户名或密码",
} as const satisfies Record<CustomEndpointUrlIssue, string>;

const MAX_CUSTOM_MODEL_ID_CHARS = 200;

export type CustomCapabilityIssue =
  | "missing"
  | "duplicate_public_model"
  | "public_model_too_long"
  | "public_model_has_control_character"
  | "upstream_model_too_long"
  | "upstream_model_has_control_character"
  | "protocol_mismatch";

export class CustomCapabilityError extends Error {
  readonly issue: CustomCapabilityIssue;

  constructor(issue: CustomCapabilityIssue) {
    super(issue);
    this.issue = issue;
  }
}

export function customEndpointUrlIssue(value: string): CustomEndpointUrlIssue | null {
  const trimmed = value.trim();
  if (!trimmed) return "empty";
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    return "malformed";
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return "not_http";
  if (!parsed.hostname) return "malformed";
  if (parsed.username || parsed.password) return "with_credentials";
  return null;
}

export const CUSTOM_PROTOCOLS: readonly AccountProtocol[] = [
  "chat_completions",
  "responses",
  "messages",
];

export function isCustomProtocol(value: unknown): value is AccountProtocol {
  return typeof value === "string" && CUSTOM_PROTOCOLS.includes(value as AccountProtocol);
}

export function customApiUrlPlaceholder(): string {
  return "https://api.example.com";
}

/** Root, `/v1`, and legacy standard endpoints have an unambiguous models URL. */
export function customApiUrlSupportsModelDiscovery(
  endpointUrl: string,
  protocol: AccountProtocol | null,
): boolean {
  if (!protocol || customEndpointUrlIssue(endpointUrl)) return false;
  try {
    const pathname = new URL(endpointUrl.trim()).pathname.replace(/\/+$/u, "");
    if (!pathname || pathname.endsWith("/v1")) return true;
    const standardPath = {
      chat_completions: "/chat/completions",
      responses: "/responses",
      messages: "/messages",
    } satisfies Record<AccountProtocol, string>;
    return pathname.endsWith(standardPath[protocol]);
  } catch {
    return false;
  }
}

/** Show the manual-model hint only for a valid API URL with no derivable models URL. */
export function customApiUrlNeedsManualModels(
  endpointUrl: string,
  protocol: AccountProtocol | null,
): boolean {
  return customEndpointUrlIssue(endpointUrl) === null
    && !customApiUrlSupportsModelDiscovery(endpointUrl, protocol);
}

export function normalizeCustomCapabilities(
  capabilities: readonly Pick<AccountModelCapabilityInput, "public_model" | "upstream_model" | "protocol">[],
  upstreamProtocol: AccountProtocol,
): AccountModelCapabilityInput[] {
  if (capabilities.length === 0) throw new CustomCapabilityError("missing");

  const seenPublicModels = new Set<string>();
  return capabilities.map((capability) => {
    const public_model = capability.public_model.trim();
    const upstream_model = capability.upstream_model.trim();
    if (Array.from(public_model).length > MAX_CUSTOM_MODEL_ID_CHARS) {
      throw new CustomCapabilityError("public_model_too_long");
    }
    if (Array.from(upstream_model).length > MAX_CUSTOM_MODEL_ID_CHARS) {
      throw new CustomCapabilityError("upstream_model_too_long");
    }
    if (/[\u0000-\u001F\u007F-\u009F]/u.test(public_model)) {
      throw new CustomCapabilityError("public_model_has_control_character");
    }
    if (/[\u0000-\u001F\u007F-\u009F]/u.test(upstream_model)) {
      throw new CustomCapabilityError("upstream_model_has_control_character");
    }
    if (capability.protocol !== upstreamProtocol) {
      throw new CustomCapabilityError("protocol_mismatch");
    }
    const publicIdentity = public_model.toLocaleLowerCase();
    if (!public_model || !upstream_model) {
      throw new CustomCapabilityError("missing");
    }
    if (seenPublicModels.has(publicIdentity)) {
      throw new CustomCapabilityError("duplicate_public_model");
    }
    seenPublicModels.add(publicIdentity);
    return { public_model, upstream_model, protocol: capability.protocol, source: "manual" };
  });
}

/**
 * Resolve the Providers destination that owns a legacy Custom account's
 * connection. The credential row points straight at it (multi-Key
 * destinations project one row per account); when that row is missing, fall
 * back to the account-owned Custom destination. Null means the projection
 * has no match and the link navigates to Providers unscoped.
 */
export function legacyCustomAccountDestinationId(
  accountId: string,
  credentialsByLegacyAccountId: ReadonlyMap<string, Pick<DestinationCredential, "destination_id">>,
  destinations: readonly Pick<Destination, "id" | "legacy">[],
): string | null {
  const credentialDestination = credentialsByLegacyAccountId.get(accountId)?.destination_id;
  if (credentialDestination) return credentialDestination;
  return destinations.find((destination) => (
    destination.legacy.kind === "custom_account" && destination.legacy.id === accountId
  ))?.id ?? null;
}
