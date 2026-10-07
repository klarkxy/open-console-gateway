import type { Account } from "../api/dashboard.ts";
import type { AccountOperationDetail } from "../api/pages.ts";
import type { ConnectionEndpoint } from "../api/connections.ts";
import type {
  BindingPatchInput,
  Identity,
  IdentityBinding,
  IdentityCredential,
  IdentityCredentialCreateInput,
  QuotaSharing,
} from "../api/identities.ts";
import { DashboardAuthError, DashboardRequestError } from "../api/dashboard-v3.ts";
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

export type CredentialWriteReason = "provider_settings" | "external_integration" | "no_authentication" | "setup_required" | "credential_missing" | "unsupported_material" | "dedicated_account_flow";
export const CREDENTIAL_WRITE_REASON_KEYS: Record<CredentialWriteReason, MessageKey> = {
  provider_settings: "Zen Free 使用供应商设置",
  external_integration: "CPA 订阅池使用 CPA 页面",
  no_authentication: "无鉴权账号不支持此操作",
  setup_required: "完成注册后可轮换 Key 或编辑绑定",
  credential_missing: "无法确定当前卡片的凭据",
  unsupported_material: "该凭据不支持轮换 Key 或编辑绑定",
  dedicated_account_flow: "Custom API 需到账号编辑中添加 Key",
};

/** Join the server's selected ids to editor records; eligibility is server-owned. */
export function credentialWriteSupport(
  operations: AccountOperationDetail | null | undefined,
  identity: Identity | null,
): CredentialWriteSupport {
  const credential = identity?.credentials.find(row => row.credential.id === operations?.credentialId) ?? null;
  const bindingRecord = credential?.bindings.find(row => row.id === operations?.bindingId) ?? null;
  return {
    rotate: operations?.rotate ?? false,
    binding: operations?.binding ?? false,
    create: operations?.create ?? false,
    credential,
    bindingRecord,
    identityId: operations?.identityId ?? null,
    unsupportedReason: operations
      ? operations.unsupportedReason ? CREDENTIAL_WRITE_REASON_KEYS[operations.unsupportedReason as CredentialWriteReason] ?? "无法确定当前卡片的凭据" : null
      : "无法确定当前卡片的凭据",
  };
}

export function accountCredentialMenuOptions(
  account: Pick<Account, "id" | "name">,
  operations: AccountOperationDetail | null | undefined,
  identity: Identity | null,
): AccountMenuOption[] {
  const support = credentialWriteSupport(operations, identity);
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
  grantedEndpointIds: readonly string[] = [],
): CredentialBindingDraft {
  const selectedEndpointIds = [...grantedEndpointIds];
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

/**
 * HTTP(S) origin after URL parsing. Default ports are omitted and IPv6 is
 * compressed, matching `canonical_origin` / `normalize_origin` in
 * `ocg-domain`. HTTP vs HTTPS, distinct ports, and distinct hosts stay
 * different. Whether a URL may be a target is a separate check.
 */
export function normalizeOrigin(value: string): string | null {
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  const hostname = url.hostname;
  if (!hostname) return null;
  const scheme = url.protocol.slice(0, -1);
  return url.port ? `${scheme}://${hostname}:${url.port}` : `${scheme}://${hostname}`;
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
