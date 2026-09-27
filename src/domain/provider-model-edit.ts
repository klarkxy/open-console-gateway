import type {
  Destination,
  DestinationCatalogModel,
  DestinationPatchInput,
  ProtocolDto,
} from "../api/destinations.ts";
import type { CatalogModelEditRequest } from "../api/generated/dashboard-v4.ts";
import type { MessageKey } from "../i18n/index.ts";

export interface ProviderModelDraft {
  /** Empty means use the upstream ID verbatim, not a display-only nickname. */
  public_model: string;
  upstream_model: string;
  protocols: ProtocolDto[];
  preferred: ProtocolDto | null;
  enabled: boolean;
}

export type ProviderModelEditIssue =
  | "invalid_model_id"
  | "immutable_destination"
  | "missing_model"
  | "missing_upstream_model"
  | "duplicate_upstream_model"
  | "duplicate_public_model"
  | "invalid_protocols"
  | "invalid_preferred"
  | "unsupported_legacy_routes";

export const PROVIDER_MODEL_EDIT_ISSUE_KEYS = {
  invalid_model_id: "模型 ID 不能包含控制字符，且不能超过 200 个字符",
  immutable_destination: "此连接由系统托管，不能在此编辑",
  missing_model: "状态已变化，请刷新后重试。",
  missing_upstream_model: "填写上游模型 ID",
  duplicate_upstream_model: "该上游模型已存在，请编辑已有模型",
  duplicate_public_model: "对外模型名不能重复",
  invalid_protocols: "选择上游协议",
  invalid_preferred: "选择上游协议",
  unsupported_legacy_routes: "选择上游协议",
} as const satisfies Record<ProviderModelEditIssue, MessageKey>;

export type ProviderModelEditPlan =
  | { kind: "invalid"; issue: ProviderModelEditIssue }
  | { kind: "save"; input: DestinationPatchInput };

const PROTOCOLS: readonly ProtocolDto[] = ["chat_completions", "responses", "messages"];

// Match the server's ASCII case-insensitive identity, not the user's locale.
function modelKey(value: string): string {
  return value.trim().replace(/[A-Z]/g, (letter) => letter.toLowerCase());
}

export function canEditProviderModels(destination: Destination | null): destination is Destination {
  return destination?.adapter === "http" && !destination.capabilities.observer;
}

function findModel(destination: Destination, modelId: string | null): DestinationCatalogModel | null {
  if (modelId === null) return null;
  return destination.catalog.find((model) => modelKey(model.public_model) === modelKey(modelId)) ?? null;
}

/** Only configured routes are choices. A per-model override stays authoritative. */
export function providerModelProtocols(destination: Destination, modelId: string | null): ProtocolDto[] {
  const override = findModel(destination, modelId)?.upstream_override;
  const configured = override
    ? [override.protocol]
    : destination.protocol_routes?.length
      ? destination.protocol_routes.map((route) => route.protocol)
      : destination.protocols;
  return [...new Set(configured.filter((protocol) => PROTOCOLS.includes(protocol)))];
}

export function providerModelDraft(destination: Destination, modelId: string | null): ProviderModelDraft | null {
  const existing = findModel(destination, modelId);
  if (modelId !== null && !existing) return null;
  const protocols = existing ? [...existing.protocols] : providerModelProtocols(destination, modelId).slice(0, 1);
  return {
    public_model: existing?.public_model ?? "",
    upstream_model: existing?.upstream_model ?? "",
    protocols,
    preferred: existing?.preferred && protocols.includes(existing.preferred)
      ? existing.preferred : protocols[0] ?? null,
    enabled: existing?.enabled ?? protocols.length > 0,
  };
}

function modelInput(model: DestinationCatalogModel): DestinationPatchInput["models"][number] {
  return {
    publicModel: model.public_model,
    upstreamModel: model.upstream_model,
    protocols: [...model.protocols],
    enabled: model.enabled,
    ...(model.preferred ? { preferred: model.preferred } : {}),
    upstreamOverride: model.upstream_override
      ? { protocol: model.upstream_override.protocol, endpointUrl: model.upstream_override.endpoint_url }
      : null,
  };
}

/**
 * One atomic PATCH, built from the captured destination. Replace one mapping
 * only; never authorize Keys, infer routes, or enable unrelated models.
 */
export function planProviderModelEdit(
  destination: Destination,
  draft: ProviderModelDraft,
  modelId: string | null,
): ProviderModelEditPlan {
  if (!canEditProviderModels(destination)) return { kind: "invalid", issue: "immutable_destination" };
  const original = findModel(destination, modelId);
  if (modelId !== null && !original) return { kind: "invalid", issue: "missing_model" };
  const upstream = draft.upstream_model.trim();
  if (!upstream) return { kind: "invalid", issue: "missing_upstream_model" };
  const publicModel = draft.public_model.trim() || upstream;
  if (destination.catalog.some((model) => model !== original && modelKey(model.public_model) === modelKey(publicModel))) {
    return { kind: "invalid", issue: "duplicate_public_model" };
  }
  const available = providerModelProtocols(destination, modelId);
  if (
    new Set(draft.protocols).size !== draft.protocols.length
    || draft.protocols.some((protocol) => !available.includes(protocol))
    || (draft.enabled && draft.protocols.length === 0)
  ) return { kind: "invalid", issue: "invalid_protocols" };
  if (draft.preferred !== null && !draft.protocols.includes(draft.preferred)) {
    return { kind: "invalid", issue: "invalid_preferred" };
  }
  const routes = destination.protocol_routes ?? [];
  const first = routes[0];
  const protocol = first?.protocol ?? destination.protocols[0];
  // The legacy PATCH would collapse multiple implicit protocols to its first.
  // Refuse rather than silently changing transport configuration.
  if (!protocol || !PROTOCOLS.includes(protocol) || (!first && destination.protocols.length !== 1)) {
    return { kind: "invalid", issue: "unsupported_legacy_routes" };
  }
  const replacement = modelInput({
    public_model: publicModel,
    upstream_model: upstream,
    protocols: [...draft.protocols],
    preferred: draft.preferred ?? draft.protocols[0] ?? null,
    enabled: draft.enabled,
    upstream_override: original?.upstream_override ?? null,
  });
  const models = destination.catalog.map((model) => model === original ? replacement : modelInput(model));
  if (!original) models.push(replacement);
  return {
    kind: "save",
    input: {
      name: destination.name,
      enabled: destination.enabled,
      endpointUrl: first?.endpoint_url ?? destination.base_url ?? "",
      upstreamProtocol: protocol,
      authScheme: first?.auth_scheme ?? destination.auth_scheme,
      ...(routes.length > 0 ? {
        protocolRoutes: routes.map((route) => ({
          protocol: route.protocol, endpointUrl: route.endpoint_url, authScheme: route.auth_scheme,
        })),
      } : {}),
      models,
      authorizeCredentialIds: [],
    },
  };
}

/** An open editor must not overwrite a newer destination snapshot. */
export function providerModelEditFingerprint(destination: Destination): string {
  return JSON.stringify(destination);
}

export function canEditBuiltinModels(destination: Destination | null): destination is Destination {
  return destination?.legacy.kind === "builtin" && destination.adapter !== "cpa" && destination.adapter !== "http"
    && !destination.capabilities.observer;
}

export function planBuiltinModelEdit(destination: Destination, draft: ProviderModelDraft, modelId: string | null):
  { kind: "save"; input: Omit<CatalogModelEditRequest, "expectedRevision" | "processGeneration"> }
  | { kind: "invalid"; issue: ProviderModelEditIssue } {
  if (!canEditBuiltinModels(destination)) return { kind: "invalid", issue: "immutable_destination" };
  const original = findModel(destination, modelId);
  if (modelId !== null && !original) return { kind: "invalid", issue: "missing_model" };
  const upstream = draft.upstream_model.trim();
  if (!upstream) return { kind: "invalid", issue: "missing_upstream_model" };
  const publicModel = draft.public_model.trim() || upstream;
  if ([upstream, publicModel].some((id) => [...id].length > 200 || /[\p{Cc}]/u.test(id))) {
    return { kind: "invalid", issue: "invalid_model_id" };
  }
  const others = destination.catalog.filter((row) => row !== original);
  if (others.some((row) => modelKey(row.public_model) === modelKey(publicModel))) return { kind: "invalid", issue: "duplicate_public_model" };
  if (others.some((row) => modelKey(row.upstream_model) === modelKey(upstream))) return { kind: "invalid", issue: "duplicate_upstream_model" };
  const available = providerModelProtocols(destination, modelId);
  if (new Set(draft.protocols).size !== draft.protocols.length || draft.protocols.some((p) => !available.includes(p)) || (draft.enabled && !draft.protocols.length)) {
    return { kind: "invalid", issue: "invalid_protocols" };
  }
  if (draft.preferred !== null && !draft.protocols.includes(draft.preferred)) return { kind: "invalid", issue: "invalid_preferred" };
  return { kind: "save", input: { originalModelId: original?.upstream_model ?? null, publicModel,
    upstreamModel: upstream, protocols: [...draft.protocols], preferred: draft.preferred ?? draft.protocols[0] ?? null, enabled: draft.enabled } };
}
