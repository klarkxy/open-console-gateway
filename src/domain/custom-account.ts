import type {
  Account,
  AccountCustomConfigUpdateInput,
  AccountModelCapability,
  AccountModelCapabilityInput,
  AccountProtocol,
  AccountUpdate,
} from "../api/dashboard.ts";

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
  empty: "请填写 API 地址",
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

/** Comparison identity only; the submitted API URL preserves administrator input. */
function canonicalCustomEndpointUrl(value: string): string {
  const parsed = new URL(value.trim());
  if (parsed.username || parsed.password) {
    throw new Error("Custom Endpoint must not contain credentials");
  }
  const pathname = parsed.pathname.replace(/\/+$/u, "");
  return `${parsed.protocol}//${parsed.host}${pathname}${parsed.search}${parsed.hash}`;
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

export type CustomAccountEditInput = {
  name: string;
  notes?: string;
  key?: string;
  endpoint_url?: string;
  upstream_protocol?: AccountProtocol;
  model_capabilities?: readonly Pick<AccountModelCapabilityInput, "public_model" | "upstream_model" | "protocol">[];
};

export type CustomAccountEditPlan = {
  account?: AccountUpdate;
  customConfig?: AccountCustomConfigUpdateInput;
};

export type CustomAccountEditWriters = {
  account: (update: AccountUpdate) => Promise<void>;
  customConfig: (config: AccountCustomConfigUpdateInput) => Promise<void>;
};

function sameCapabilities(
  saved: readonly AccountModelCapability[],
  next: readonly AccountModelCapabilityInput[],
): boolean {
  return saved.length === next.length && saved.every((capability, index) => (
    capability.public_model.trim() === next[index]?.public_model
      && capability.upstream_model.trim() === next[index]?.upstream_model
      && capability.protocol === next[index]?.protocol
  ));
}

/** Validate all Custom sections before any write, then combine config and models. */
export function planCustomAccountEdit(
  account: Account,
  input: CustomAccountEditInput,
): CustomAccountEditPlan {
  const config = account.custom_config;
  if (!config) throw new Error("Custom account configuration is missing");

  const endpoint_url = (input.endpoint_url ?? config.endpoint_url).trim();
  const endpointUrlIssue = customEndpointUrlIssue(endpoint_url);
  if (endpointUrlIssue) throw new Error(CUSTOM_ENDPOINT_URL_ISSUE_KEYS[endpointUrlIssue]);
  const canonicalEndpointUrl = canonicalCustomEndpointUrl(endpoint_url);
  const canonicalSavedEndpointUrl = canonicalCustomEndpointUrl(config.endpoint_url);
  const upstream_protocol = input.upstream_protocol ?? config.upstream_protocol;
  if (!isCustomProtocol(upstream_protocol)) throw new CustomCapabilityError("protocol_mismatch");

  const capabilities = normalizeCustomCapabilities(
    input.model_capabilities ?? account.model_capabilities,
    upstream_protocol,
  );
  const name = input.name.trim();
  const notes = input.notes ?? "";
  const keyReplacement = input.key !== undefined;
  const metadataChanged = name !== account.name || notes !== account.notes || keyReplacement;
  const capabilitiesChanged = !sameCapabilities(account.model_capabilities, capabilities);
  const configChanged = canonicalEndpointUrl !== canonicalSavedEndpointUrl
    || upstream_protocol !== config.upstream_protocol;

  return {
    ...(metadataChanged
      ? { account: { name, notes, ...(input.key === undefined ? {} : { key: input.key }) } }
      : {}),
    ...(configChanged || capabilitiesChanged || keyReplacement
      ? {
        customConfig: {
          endpoint_url,
          upstream_protocol,
          model_capabilities: capabilities,
        },
      }
      : {}),
  };
}

export async function applyCustomAccountEditPlan(
  plan: CustomAccountEditPlan,
  writers: CustomAccountEditWriters,
): Promise<void> {
  if (plan.account) await writers.account(plan.account);
  if (plan.customConfig) await writers.customConfig(plan.customConfig);
}

export async function executeCustomAccountEdit(
  account: Account,
  input: CustomAccountEditInput,
  writers: CustomAccountEditWriters,
): Promise<void> {
  await applyCustomAccountEditPlan(planCustomAccountEdit(account, input), writers);
}
