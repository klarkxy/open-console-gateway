import type { Account } from "../api/dashboard.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { Connection, ConnectionEndpoint } from "../api/connections.ts";
import type {
  BindingPatchInput,
  Identity,
  IdentityBinding,
  IdentityCredential,
  IdentityCredentialCreateInput,
  QuotaSharing,
} from "../api/identities.ts";
import { DashboardAuthError, DashboardRequestError } from "../api/dashboard-v3.ts";
import { accountCapabilities } from "./account-capabilities.ts";
import { accountIsReady } from "./account-display.ts";
import { credentialForAccount, inferenceCredentials } from "./account-identity.ts";
import { protocolDisplayName } from "./provider-contracts.ts";
import type { AccountMenuOption } from "./account-display.ts";
import { t, type MessageKey } from "../i18n/index.ts";

const MAX_MODEL_NAME_CHARS = 200;
const CONTROL_CHARS = /[\u0000-\u001F\u007F-\u009F]/u;

export type CredentialEditorMode = "rotate" | "binding";

export type CredentialRotateDraft = {
  secret: string;
};

export type CredentialBindingDraft = {
  enabled: boolean;
  scopeKind: "all" | "only";
  models: string[];
  selectedEndpointIds: string[];
  destinationsTouched: boolean;
};

export type CredentialCreateDraft = {
  secret: string;
  accountLabel: string;
  connectionId: string;
  sharingKind: "independent" | "shared";
  shareCredentialId: string;
};

export type CredentialCreateFailureKind = "none" | "uncertain" | "definitive";

export type CredentialEditorIssue =
  | "missing_secret"
  | "missing_models"
  | "duplicate_model"
  | "model_too_long"
  | "model_has_control_character"
  | "missing_connection"
  | "missing_share_target"
  | "uncertain_payload_locked";

export const CREDENTIAL_EDITOR_ISSUE_KEYS = {
  missing_secret: "填写新 Key",
  missing_models: "至少填写一个准确的模型名称",
  duplicate_model: "模型名称不能重复",
  model_too_long: "模型名称最多 200 个字符",
  model_has_control_character: "模型名称不能包含控制字符",
  missing_connection: "选择连接",
  missing_share_target: "选择同一身份下要共享额度的 Key",
  uncertain_payload_locked: "提交结果未知。用原内容重试或取消，勿修改后提交。",
} as const satisfies Record<CredentialEditorIssue, MessageKey>;

export class CredentialEditorError extends Error {
  readonly issue: CredentialEditorIssue;

  constructor(issue: CredentialEditorIssue) {
    super(issue);
    this.name = "CredentialEditorError";
    this.issue = issue;
  }
}

export function credentialEditorIssueKey(error: unknown): MessageKey {
  return error instanceof CredentialEditorError
    ? CREDENTIAL_EDITOR_ISSUE_KEYS[error.issue]
    : "保存失败，请重试";
}

export type CredentialWriteSupport = {
  rotate: boolean;
  binding: boolean;
  create: boolean;
  credential: IdentityCredential | null;
  bindingRecord: IdentityBinding | null;
  identityId: string | null;
  unsupportedReason: MessageKey | null;
};

function selectedInferenceCredential(
  account: Pick<Account, "id">,
  identity: Identity | null,
): IdentityCredential | null {
  const row = credentialForAccount(identity, account.id);
  if (!row || row.credential.purpose !== "inference") return null;
  return row;
}

/**
 * V4 rotate/binding are hidden for Zen, CPA, no-auth, and observer credentials.
 * Add Key also hides Custom API (dedicated account path) and missing overlay
 * rows rather than inventing ids. Matches backend `resolve_connection_target`.
 */
export function credentialWriteSupport(
  account: Pick<Account, "id" | "provider_id" | "account_type" | "credential_kind" | "setup_step">,
  identity: Identity | null,
  catalog: readonly ProviderCatalogEntry[] | null | undefined = null,
): CredentialWriteSupport {
  const hidden: CredentialWriteSupport = {
    rotate: false,
    binding: false,
    create: false,
    credential: null,
    bindingRecord: null,
    identityId: identity?.identity.id ?? null,
    unsupportedReason: null,
  };
  const caps = accountCapabilities(account, catalog);
  if (caps.keylessSingleton) {
    return { ...hidden, unsupportedReason: "Zen Free 使用供应商设置" };
  }
  if (caps.externalIntegration) {
    return { ...hidden, unsupportedReason: "CPA 订阅池使用 CPA 页面" };
  }
  if (account.credential_kind === "none") {
    return { ...hidden, unsupportedReason: "无鉴权账号不支持此操作" };
  }
  if (!accountIsReady(account)) {
    return { ...hidden, unsupportedReason: "完成注册后可轮换 Key 或编辑绑定" };
  }

  const credential = selectedInferenceCredential(account, identity);
  if (!credential) {
    return { ...hidden, unsupportedReason: "无法确定当前卡片的凭据" };
  }
  if (credential.subject === "anonymous") {
    return {
      ...hidden,
      credential,
      unsupportedReason: "无鉴权账号不支持此操作",
    };
  }
  if (credential.credential.material_kind !== "api_key") {
    return {
      ...hidden,
      credential,
      bindingRecord: credential.bindings[0] ?? null,
      unsupportedReason: "该凭据不支持轮换 Key 或编辑绑定",
    };
  }

  const bindingRecord = credential.bindings[0] ?? null;
  const identityId = identity?.identity.id ?? null;
  const rotate = true;
  const binding = bindingRecord !== null;
  const create = !caps.endpointOnAccount
    && !!identityId
    && !!bindingRecord?.connection_id;
  return {
    rotate,
    binding,
    create,
    credential,
    bindingRecord,
    identityId,
    unsupportedReason: caps.endpointOnAccount
      ? "Custom API 需到账号编辑中添加 Key"
      : null,
  };
}

export function accountCredentialMenuOptions(
  account: Pick<Account, "id" | "name" | "provider_id" | "account_type" | "credential_kind" | "setup_step">,
  identity: Identity | null,
  catalog: readonly ProviderCatalogEntry[] | null | undefined = null,
): AccountMenuOption[] {
  const support = credentialWriteSupport(account, identity, catalog);
  const options: AccountMenuOption[] = [];
  if (support.rotate) {
    options.push({
      key: "rotate-key",
      label: t("轮换 Key"),
      accountId: account.id,
      accountName: account.name,
    });
  }
  if (support.create) {
    options.push({
      key: "add-key",
      label: t("添加 Key"),
      accountId: account.id,
      accountName: account.name,
    });
  }
  if (support.binding) {
    options.push({
      key: "edit-binding",
      label: t("编辑绑定"),
      accountId: account.id,
      accountName: account.name,
    });
  }
  return options;
}

/**
 * Same-identity inference Keys that may be chosen as an explicit quota-share
 * target. Identity match alone never selects sharing.
 */
export function shareableInferenceCredentials(
  identity: Identity | null,
): IdentityCredential[] {
  return inferenceCredentials(identity).filter((row) => (
    row.credential.material_kind === "api_key"
    && row.subject !== "anonymous"
  ));
}

/**
 * Backend `resolve_connection_target`: Custom API dedicated rows, CPA, Zen,
 * no-auth, and singleton/unavailable builtins cannot receive a Key here.
 */
export function connectionAllowsIdentityCredentialCreate(
  connection: Pick<Connection, "legacy" | "origin">,
): boolean {
  if (connection.legacy.kind === "custom_account" || connection.origin === "custom_account") {
    return false;
  }
  const providerId = connection.legacy.id;
  if (
    providerId === "cpa"
    || providerId === "opencode-zen-free"
    || providerId === "custom"
  ) {
    return false;
  }
  if (connection.legacy.kind === "dynamic_provider" && connection.origin === "builtin") {
    return false;
  }
  return true;
}

export function emptyRotateDraft(): CredentialRotateDraft {
  return { secret: "" };
}

export function emptyCreateDraft(connectionId = ""): CredentialCreateDraft {
  return {
    secret: "",
    accountLabel: "",
    connectionId,
    sharingKind: "independent",
    shareCredentialId: "",
  };
}

function savedOriginSet(binding: IdentityBinding): Set<string> {
  const origins = new Set<string>();
  for (const raw of binding.allowed_origins) {
    const origin = normalizeOrigin(raw);
    if (origin) origins.add(origin);
  }
  return origins;
}

/** Saved ID plus current Origin (sealed url:null needs only the saved ID). */
export function endpointMatchesSavedGrant(
  endpoint: ConnectionEndpoint,
  binding: IdentityBinding,
): boolean {
  if (!binding.allowed_endpoint_ids.includes(endpoint.id)) return false;
  if (endpoint.url === null) return true;
  const current = normalizeOrigin(endpoint.url);
  return !!current && savedOriginSet(binding).has(current);
}

export function bindingDraftFrom(
  binding: IdentityBinding | null,
  endpoints: readonly ConnectionEndpoint[] = [],
): CredentialBindingDraft {
  const selectedEndpointIds = binding
    ? endpoints
      .filter((endpoint) => endpointMatchesSavedGrant(endpoint, binding))
      .map((endpoint) => endpoint.id)
    : [];
  const scope = binding?.model_scope;
  if (scope?.kind === "only") {
    return {
      enabled: binding?.enabled ?? true,
      scopeKind: "only",
      models: scope.models.length > 0 ? [...scope.models] : [""],
      selectedEndpointIds,
      destinationsTouched: false,
    };
  }
  return {
    enabled: binding?.enabled ?? true,
    scopeKind: "all",
    models: [""],
    selectedEndpointIds,
    destinationsTouched: false,
  };
}

export function buildRotatePayload(draft: CredentialRotateDraft): { secretInput: string } {
  const secretInput = draft.secret.trim();
  if (!secretInput) throw new CredentialEditorError("missing_secret");
  return { secretInput };
}

function parseExactModelNames(models: readonly string[]): string[] {
  const parsed: string[] = [];
  const seen = new Set<string>();
  for (const raw of models) {
    const name = raw.trim();
    if (!name) continue;
    if (Array.from(name).length > MAX_MODEL_NAME_CHARS) {
      throw new CredentialEditorError("model_too_long");
    }
    if (CONTROL_CHARS.test(name)) {
      throw new CredentialEditorError("model_has_control_character");
    }
    if (seen.has(name)) throw new CredentialEditorError("duplicate_model");
    seen.add(name);
    parsed.push(name);
  }
  if (parsed.length === 0) throw new CredentialEditorError("missing_models");
  return parsed;
}

function originFromEndpointUrl(url: string): string | null {
  const trimmed = url.trim();
  const split = trimmed.split("://");
  if (split.length < 2) return null;
  const scheme = split[0] ?? "";
  const rest = split.slice(1).join("://");
  if (!scheme || !rest) return null;
  const hostport = rest.split(/[/?#]/u, 1)[0] ?? "";
  if (!hostport) return null;
  return `${scheme}://${hostport}`;
}

/** HTTP(S) scheme + host [+ port]; scheme/host lowercased. Matches backend. */
export function normalizeOrigin(value: string): string | null {
  const origin = originFromEndpointUrl(value);
  if (!origin) return null;
  const split = origin.split("://");
  if (split.length < 2) return null;
  const scheme = (split[0] ?? "").toLowerCase();
  const hostport = split.slice(1).join("://");
  if (scheme !== "http" && scheme !== "https") return null;
  if (!hostport || hostport.includes("/")) return null;
  let normalizedHostport: string;
  if (hostport.startsWith("[")) {
    normalizedHostport = hostport;
  } else {
    const colon = hostport.lastIndexOf(":");
    if (colon > 0) {
      const host = hostport.slice(0, colon);
      const port = hostport.slice(colon + 1);
      if (host && /^[0-9]+$/u.test(port)) {
        normalizedHostport = `${host.toLowerCase()}:${port}`;
      } else {
        normalizedHostport = hostport.toLowerCase();
      }
    } else {
      normalizedHostport = hostport.toLowerCase();
    }
  }
  if (!normalizedHostport) return null;
  return `${scheme}://${normalizedHostport}`;
}

export function unionOriginsForEndpoints(
  endpoints: readonly ConnectionEndpoint[],
  selectedIds: readonly string[],
): string[] {
  const selected = new Set(selectedIds);
  const origins: string[] = [];
  const seen = new Set<string>();
  for (const endpoint of endpoints) {
    if (!selected.has(endpoint.id) || !endpoint.url) continue;
    const origin = normalizeOrigin(endpoint.url);
    if (!origin || seen.has(origin)) continue;
    seen.add(origin);
    origins.push(origin);
  }
  return origins;
}

export type BindingDestinationOption = {
  id: string;
  protocol: string;
  url: string | null;
  locked: boolean;
};

export function bindingDestinationOptions(
  endpoints: readonly ConnectionEndpoint[],
): BindingDestinationOption[] {
  return endpoints.map((endpoint) => ({
    id: endpoint.id,
    protocol: protocolDisplayName(endpoint.wire_protocol),
    url: endpoint.url,
    locked: endpoint.locked || endpoint.url === null,
  }));
}

export function staleSavedEndpointIds(
  binding: IdentityBinding | null,
  endpoints: readonly ConnectionEndpoint[],
): string[] {
  if (!binding) return [];
  const configured = new Set(endpoints.map((endpoint) => endpoint.id));
  return binding.allowed_endpoint_ids.filter((id) => !configured.has(id));
}

export function staleSavedOrigins(
  binding: IdentityBinding | null,
  endpoints: readonly ConnectionEndpoint[],
): string[] {
  if (!binding) return [];
  const current = new Set(
    endpoints
      .map((endpoint) => (endpoint.url ? normalizeOrigin(endpoint.url) : null))
      .filter((origin): origin is string => !!origin),
  );
  const stale: string[] = [];
  const seen = new Set<string>();
  for (const raw of binding.allowed_origins) {
    const origin = normalizeOrigin(raw) ?? raw.trim();
    if (!origin || seen.has(origin) || current.has(origin)) continue;
    seen.add(origin);
    stale.push(origin);
  }
  return stale;
}

export function buildBindingPayload(
  draft: CredentialBindingDraft,
  endpoints: readonly ConnectionEndpoint[] = [],
): BindingPatchInput {
  const base: BindingPatchInput = draft.scopeKind === "all"
    ? { enabled: draft.enabled, modelScope: { kind: "all" } }
    : {
      enabled: draft.enabled,
      modelScope: { kind: "only", models: parseExactModelNames(draft.models) },
    };
  if (!draft.destinationsTouched) return base;
  const configured = new Map(endpoints.map((endpoint) => [endpoint.id, endpoint]));
  const allowedEndpointIds: string[] = [];
  const seen = new Set<string>();
  for (const id of draft.selectedEndpointIds) {
    const trimmed = id.trim();
    if (!trimmed || seen.has(trimmed) || !configured.has(trimmed)) continue;
    seen.add(trimmed);
    allowedEndpointIds.push(trimmed);
  }
  return {
    ...base,
    allowedEndpointIds,
    allowedOrigins: unionOriginsForEndpoints(endpoints, allowedEndpointIds),
  };
}

export function buildCreatePayload(
  draft: CredentialCreateDraft,
  shareableCredentialIds: readonly string[] = [],
): IdentityCredentialCreateInput {
  const secretInput = draft.secret.trim();
  if (!secretInput) throw new CredentialEditorError("missing_secret");
  const connectionId = draft.connectionId.trim();
  if (!connectionId) throw new CredentialEditorError("missing_connection");
  const accountLabel = draft.accountLabel.trim();
  let quotaSharing: QuotaSharing = { kind: "independent" };
  if (draft.sharingKind === "shared") {
    const credentialId = draft.shareCredentialId.trim();
    if (!credentialId || !shareableCredentialIds.includes(credentialId)) {
      throw new CredentialEditorError("missing_share_target");
    }
    quotaSharing = { kind: "shared", credentialId };
  }
  const payload: IdentityCredentialCreateInput = {
    connectionId,
    secretInput,
    quotaSharing,
  };
  if (accountLabel) payload.accountLabel = accountLabel;
  return payload;
}

export function createPayloadSignature(payload: IdentityCredentialCreateInput): string {
  return JSON.stringify({
    connectionId: payload.connectionId,
    secretInput: payload.secretInput,
    accountLabel: payload.accountLabel ?? null,
    quotaSharing: payload.quotaSharing ?? { kind: "independent" },
  });
}

export function nextCreateOperationId(args: {
  previousId: string | null;
  previousSignature: string | null;
  nextSignature: string;
  lastFailure: CredentialCreateFailureKind;
}): string {
  if (args.lastFailure === "uncertain") {
    if (
      args.previousId
      && args.previousSignature !== null
      && args.previousSignature === args.nextSignature
    ) {
      return args.previousId;
    }
    throw new CredentialEditorError("uncertain_payload_locked");
  }
  return crypto.randomUUID();
}

const DEFINITE_CREATE_REJECT_STATUSES = new Set([400, 401, 403, 404, 409, 422]);

export function isUncertainCreateFailure(error: unknown): boolean {
  if (error instanceof DashboardAuthError) return false;
  if (error instanceof DashboardRequestError) {
    return !DEFINITE_CREATE_REJECT_STATUSES.has(error.status);
  }
  return true;
}
