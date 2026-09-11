import type { WithoutExpectation } from "../api/dashboard-v3.ts";
import type { OnboardingCommitRequest } from "../api/generated/dashboard-v4.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { customEndpointUrlIssue } from "./custom-account.ts";

const DYNAMIC_PROVIDER_MODEL_SOURCE = "dynamic_provider";

export type DynamicAuthKind = "bearer" | "x-api-key" | "none";
export type DynamicUpstreamProtocol = "chat_completions" | "responses" | "messages";

/**
 * Per-model upstream override. Both fields are explicit; when absent the
 * mapping inherits the supplier endpoint/protocol. Auth is always
 * supplier-owned and never overridden here.
 */
export interface DynamicMappingOverride {
  protocol: DynamicUpstreamProtocol;
  endpoint_url: string;
}

export interface ProviderDefinitionMapping {
  public_model: string;
  upstream_model: string;
  /** Null/undefined inherits the supplier default protocol and endpoint. */
  upstream_override?: DynamicMappingOverride | null;
}

export interface ProviderDefinitionDraft {
  name: string;
  endpoint_url: string;
  upstream_protocol: DynamicUpstreamProtocol | "";
  auth_kind: DynamicAuthKind | "";
  models: ProviderDefinitionMapping[];
  account_name: string;
  notes: string;
  key: string;
  /**
   * Source-template provenance. "" or undefined means manual/none; undefined
   * on edit means "do not touch the persisted value" (PATCH omits it).
   */
  preset_id?: string;
}

export type ProviderDefinitionDraftError =
  | "missing_name"
  | "missing_endpoint_url"
  | "invalid_endpoint_url"
  | "endpoint_url_not_http"
  | "endpoint_url_with_credentials"
  | "missing_protocol"
  | "missing_auth_kind"
  | "missing_mappings"
  | "duplicate_public_model"
  | "missing_public_model"
  | "missing_upstream_model"
  | "public_model_too_long"
  | "public_model_has_control_character"
  | "upstream_model_too_long"
  | "upstream_model_has_control_character"
  | "missing_override_endpoint"
  | "invalid_override_endpoint"
  | "override_endpoint_not_http"
  | "override_endpoint_with_credentials"
  | "missing_key"
  | "missing_replacement_key";

export const DYNAMIC_PROVIDER_DRAFT_ERROR_KEYS = {
  missing_name: "请填写供应商名称",
  missing_endpoint_url: "请填写 API 地址",
  invalid_endpoint_url: "Endpoint 格式无效",
  endpoint_url_not_http: "Endpoint 必须是 http:// 或 https:// URL",
  endpoint_url_with_credentials: "Endpoint 不能包含用户名或密码",
  missing_protocol: "请选择上游协议",
  missing_auth_kind: "请选择鉴权方式",
  missing_mappings: "请至少添加一个完整模型映射",
  duplicate_public_model: "对外模型名不能重复",
  missing_public_model: "请填写对外模型名",
  missing_upstream_model: "请填写上游模型 ID",
  public_model_too_long: "对外模型名最多 200 个字符",
  public_model_has_control_character: "对外模型名不能包含控制字符",
  upstream_model_too_long: "上游模型 ID 最多 200 个字符",
  upstream_model_has_control_character: "上游模型 ID 不能包含控制字符",
  missing_override_endpoint: "请填写该模型覆盖的上游地址，或改回跟随供应商默认",
  invalid_override_endpoint: "覆盖的上游地址格式无效",
  override_endpoint_not_http: "覆盖的上游地址必须是 http:// 或 https:// URL",
  override_endpoint_with_credentials: "覆盖的上游地址不能包含用户名或密码",
  missing_key: "请填写 API Key",
  missing_replacement_key: "从无鉴权改为需要 Key 时必须填写替换 Key",
} as const satisfies Record<ProviderDefinitionDraftError, string>;

export const DYNAMIC_AUTH_KINDS: readonly DynamicAuthKind[] = ["bearer", "x-api-key", "none"];
export const DYNAMIC_PROTOCOLS: readonly DynamicUpstreamProtocol[] = [
  "chat_completions",
  "responses",
  "messages",
];

export function isDynamicCatalogEntry(
  entry: Pick<ProviderCatalogEntry, "model_source">,
): boolean {
  return entry.model_source === DYNAMIC_PROVIDER_MODEL_SOURCE;
}

export function dynamicAuthRequiresKey(authKind: DynamicAuthKind | ""): boolean {
  return authKind === "bearer" || authKind === "x-api-key";
}

export function emptyProviderDefinitionDraft(): ProviderDefinitionDraft {
  return {
    name: "",
    endpoint_url: "",
    upstream_protocol: "chat_completions",
    auth_kind: "bearer",
    models: [{ public_model: "", upstream_model: "" }],
    account_name: "",
    notes: "",
    key: "",
    preset_id: "",
  };
}

export function sanitizeProviderDefinitionDraft(draft: ProviderDefinitionDraft): ProviderDefinitionDraft {
  return {
    ...draft,
    key: "",
  };
}

function hasControlCharacter(value: string): boolean {
  return [...value].some((char) => {
    const code = char.codePointAt(0) ?? 0;
    return code < 32 || code === 127;
  });
}

function normalizeDynamicOverride(
  override: DynamicMappingOverride | null | undefined,
): DynamicMappingOverride | null | ProviderDefinitionDraftError {
  if (!override) return null;
  if (override.protocol !== "chat_completions"
    && override.protocol !== "responses"
    && override.protocol !== "messages") {
    return "missing_protocol";
  }
  const endpointUrl = override.endpoint_url.trim();
  if (!endpointUrl) return "missing_override_endpoint";
  const issue = customEndpointUrlIssue(endpointUrl);
  if (issue === "malformed") return "invalid_override_endpoint";
  if (issue === "not_http") return "override_endpoint_not_http";
  if (issue === "with_credentials") return "override_endpoint_with_credentials";
  return { protocol: override.protocol, endpoint_url: endpointUrl };
}

export function normalizeDynamicMappings(
  mappings: readonly ProviderDefinitionMapping[],
): ProviderDefinitionMapping[] | ProviderDefinitionDraftError {
  const normalized: ProviderDefinitionMapping[] = [];
  const seen = new Set<string>();
  for (const mapping of mappings) {
    const publicModel = mapping.public_model.trim();
    const upstreamModel = mapping.upstream_model.trim();
    if (!publicModel && !upstreamModel) continue;
    if (!publicModel) return "missing_public_model";
    if (!upstreamModel) return "missing_upstream_model";
    if (publicModel.length > 200) return "public_model_too_long";
    if (hasControlCharacter(publicModel)) return "public_model_has_control_character";
    if (upstreamModel.length > 200) return "upstream_model_too_long";
    if (hasControlCharacter(upstreamModel)) return "upstream_model_has_control_character";
    const override = normalizeDynamicOverride(mapping.upstream_override);
    if (typeof override === "string") return override;
    const key = publicModel.toLocaleLowerCase();
    if (seen.has(key)) return "duplicate_public_model";
    seen.add(key);
    normalized.push({
      public_model: publicModel,
      upstream_model: upstreamModel,
      upstream_override: override,
    });
  }
  if (normalized.length === 0) return "missing_mappings";
  return normalized;
}

/** Only fully filled mapping rows can be tested; the picker never silently selects a partial row. */
export function completeDynamicTestTargets(
  models: readonly ProviderDefinitionMapping[],
): ProviderDefinitionMapping[] {
  return models
    .map((mapping) => ({
      public_model: mapping.public_model.trim(),
      upstream_model: mapping.upstream_model.trim(),
      upstream_override: mapping.upstream_override
        ? {
          protocol: mapping.upstream_override.protocol,
          endpoint_url: mapping.upstream_override.endpoint_url.trim(),
        }
        : null,
    }))
    .filter((mapping) => mapping.public_model && mapping.upstream_model);
}

/**
 * Effective route for one mapping: ANY present override wins verbatim — even
 * an unfinished one — so an explicit override never silently falls back to
 * the supplier default. Callers validate a present override before use (same
 * rules as Save). No sibling endpoints are ever guessed.
 */
export function resolveDynamicMappingRoute(
  supplier: { endpoint_url: string; upstream_protocol: DynamicUpstreamProtocol | "" },
  mapping: Pick<ProviderDefinitionMapping, "upstream_override">,
): { endpoint_url: string; upstream_protocol: DynamicUpstreamProtocol | "" } {
  const override = mapping.upstream_override;
  if (override) {
    return { endpoint_url: override.endpoint_url.trim(), upstream_protocol: override.protocol };
  }
  return {
    endpoint_url: supplier.endpoint_url.trim(),
    upstream_protocol: supplier.upstream_protocol,
  };
}

/**
 * Validates one mapping's override only — unrelated unfinished draft rows are
 * not checked. Null when the row inherits the supplier default or the
 * override satisfies the same endpoint rules as Save.
 */
export function dynamicMappingOverrideError(
  mapping: Pick<ProviderDefinitionMapping, "upstream_override">,
): ProviderDefinitionDraftError | null {
  const result = normalizeDynamicOverride(mapping.upstream_override);
  return typeof result === "string" ? result : null;
}

export function validateProviderDefinitionDraft(
  draft: ProviderDefinitionDraft,
  options: {
    mode: "create" | "edit";
    previousAuthKind?: DynamicAuthKind | "";
    requireKey?: boolean;
  } = { mode: "create" },
): ProviderDefinitionDraftError | null {
  if (!draft.name.trim()) return "missing_name";
  if (!draft.endpoint_url.trim()) return "missing_endpoint_url";
  const endpointIssue = customEndpointUrlIssue(draft.endpoint_url);
  if (endpointIssue === "malformed") return "invalid_endpoint_url";
  if (endpointIssue === "not_http") return "endpoint_url_not_http";
  if (endpointIssue === "with_credentials") return "endpoint_url_with_credentials";
  if (draft.upstream_protocol !== "chat_completions"
    && draft.upstream_protocol !== "responses"
    && draft.upstream_protocol !== "messages") {
    return "missing_protocol";
  }
  if (draft.auth_kind !== "bearer" && draft.auth_kind !== "x-api-key" && draft.auth_kind !== "none") {
    return "missing_auth_kind";
  }
  const mappings = normalizeDynamicMappings(draft.models);
  if (typeof mappings === "string") return mappings;
  if (options.requireKey && dynamicAuthRequiresKey(draft.auth_kind) && !draft.key.trim()) {
    return "missing_key";
  }
  if (
    options.mode === "edit"
    && options.previousAuthKind === "none"
    && dynamicAuthRequiresKey(draft.auth_kind)
    && !draft.key.trim()
  ) {
    return "missing_replacement_key";
  }
  return null;
}

export function buildProviderDefinitionCreateBody(draft: ProviderDefinitionDraft): {
  name: string;
  endpointUrl: string;
  upstreamProtocol: DynamicUpstreamProtocol;
  authKind: DynamicAuthKind;
  models: Array<{
    publicModel: string;
    upstreamModel: string;
    /** Explicit per-model upstream; null inherits the supplier default. */
    upstreamOverride: { protocol: DynamicUpstreamProtocol; endpointUrl: string } | null;
  }>;
  accountName?: string;
  notes?: string;
  key?: string;
  presetId?: string;
} {
  const error = validateProviderDefinitionDraft(draft, { mode: "create" });
  if (error) throw new Error(error);
  const models = normalizeDynamicMappings(draft.models);
  if (typeof models === "string") throw new Error(models);
  const body: ReturnType<typeof buildProviderDefinitionCreateBody> = {
    name: draft.name.trim(),
    endpointUrl: draft.endpoint_url.trim(),
    upstreamProtocol: draft.upstream_protocol as DynamicUpstreamProtocol,
    authKind: draft.auth_kind as DynamicAuthKind,
    models: models.map((mapping) => ({
      publicModel: mapping.public_model,
      upstreamModel: mapping.upstream_model,
      upstreamOverride: mapping.upstream_override
        ? { protocol: mapping.upstream_override.protocol, endpointUrl: mapping.upstream_override.endpoint_url }
        : null,
    })),
  };
  const accountName = draft.account_name.trim();
  if (accountName) body.accountName = accountName;
  const notes = draft.notes.trim();
  if (notes) body.notes = notes;
  const key = draft.key.trim();
  if (dynamicAuthRequiresKey(draft.auth_kind) && key) body.key = key;
  // Create omits provenance when manual; only a concrete preset ID is sent.
  if (draft.preset_id) body.presetId = draft.preset_id;
  return body;
}

export function buildOnboardingCommitRequest(
  draft: ProviderDefinitionDraft,
  operationId: string,
): WithoutExpectation<OnboardingCommitRequest> {
  const error = validateProviderDefinitionDraft(draft, { mode: "create" });
  if (error) throw new Error(error);
  const models = normalizeDynamicMappings(draft.models);
  if (typeof models === "string") throw new Error(models);
  const body: WithoutExpectation<OnboardingCommitRequest> = {
    operationId,
    connection: {
      kind: "new",
      templateId: draft.preset_id || "custom-http",
      name: draft.name.trim(),
      endpointUrl: draft.endpoint_url.trim(),
      upstreamProtocol: draft.upstream_protocol as DynamicUpstreamProtocol,
      authKind: draft.auth_kind as DynamicAuthKind,
    },
    targets: models.map((mapping) => ({
      publicModel: mapping.public_model,
      upstreamModel: mapping.upstream_model,
      upstreamOverride: mapping.upstream_override
        ? { protocol: mapping.upstream_override.protocol, endpointUrl: mapping.upstream_override.endpoint_url }
        : null,
    })),
  };
  if (draft.auth_kind === "none") {
    body.authorization = { kind: "none" };
  } else if (dynamicAuthRequiresKey(draft.auth_kind)) {
    const secretInput = draft.key.trim();
    if (secretInput) {
      const accountLabel = draft.account_name.trim();
      const notes = draft.notes.trim();
      body.authorization = {
        kind: "api_key",
        secretInput,
        ...(accountLabel ? { accountLabel } : {}),
        ...(notes ? { notes } : {}),
      };
    }
  }
  return body;
}

export function buildProviderDefinitionUpdateBody(
  draft: ProviderDefinitionDraft,
  previousAuthKind: DynamicAuthKind | "",
): {
  name: string;
  endpointUrl: string;
  upstreamProtocol: DynamicUpstreamProtocol;
  authKind: DynamicAuthKind;
  models: Array<{
    publicModel: string;
    upstreamModel: string;
    upstreamOverride: { protocol: DynamicUpstreamProtocol; endpointUrl: string } | null;
  }>;
  key?: string;
  presetId?: string;
} {
  const error = validateProviderDefinitionDraft(draft, { mode: "edit", previousAuthKind });
  if (error) throw new Error(error);
  const models = normalizeDynamicMappings(draft.models);
  if (typeof models === "string") throw new Error(models);
  const body: ReturnType<typeof buildProviderDefinitionUpdateBody> = {
    name: draft.name.trim(),
    endpointUrl: draft.endpoint_url.trim(),
    upstreamProtocol: draft.upstream_protocol as DynamicUpstreamProtocol,
    authKind: draft.auth_kind as DynamicAuthKind,
    models: models.map((mapping) => ({
      publicModel: mapping.public_model,
      upstreamModel: mapping.upstream_model,
      upstreamOverride: mapping.upstream_override
        ? { protocol: mapping.upstream_override.protocol, endpointUrl: mapping.upstream_override.endpoint_url }
        : null,
    })),
  };
  const sendReplacementKey =
    previousAuthKind === "none" && dynamicAuthRequiresKey(draft.auth_kind);
  if (sendReplacementKey && draft.key.trim()) {
    body.key = draft.key.trim();
  }
  // Omitted preserves the persisted source; "" explicitly clears it.
  if (draft.preset_id !== undefined) body.presetId = draft.preset_id;
  return body;
}

export const DYNAMIC_PAID_TEST_WARNING_KEY = "真实测试会消耗上游额度或产生费用。确定继续？" as const;

export type ProviderDefinitionUiAction = "save" | "discover" | "test" | "delete";

export function dynamicProviderActionNeedsConfirm(action: ProviderDefinitionUiAction): boolean {
  return action === "test" || action === "delete";
}
