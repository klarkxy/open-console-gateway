import type { Account } from "../api/dashboard.ts";
import type {
  ProviderCatalogEntry,
  ProviderCatalogFormField,
} from "../api/providers.ts";
import type { MessageKey } from "../i18n/index.ts";

export type PlanKind = "quota" | "free" | "api-key" | "custom";

/**
 * Open frontend projection of one V3 Provider Catalog row. `id` is the
 * backend-owned provider id used by filters, dialogs, pricing, and cards.
 */
export interface ProviderSurface extends ProviderCatalogEntry {
  id: string;
  label: string;
  kind: PlanKind;
  /** True only for the two offline compatibility surfaces. */
  legacy: boolean;
  /** User-defined Provider definitions own endpoint/protocol/model mappings. */
  dynamic: boolean;
}

/** Kept as a source-compatible name while consumers move to ProviderSurface. */
export type PlanDefinition = ProviderSurface;

const LEGACY_GO_FIELDS: ProviderCatalogFormField[] = [
  { id: "name", kind: "text", required: true, immutable_after_create: false },
  { id: "key", kind: "secret", required: true, immutable_after_create: false },
  { id: "purchase_date", kind: "date", required: false, immutable_after_create: false },
  { id: "notes", kind: "text", required: false, immutable_after_create: false },
];

const OFFLINE_SURFACES: readonly ProviderSurface[] = [
  {
    id: "opencode",
    provider_id: "opencode",
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: "OpenCode Go",
    display_family: "OpenCode",
    label: "OpenCode Go",
    kind: "quota",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    creation_unavailable_reason: null,
    verification_policy: "required",
    verification_runtime_availability: "available",
    routable: true,
    managed_registration: true,
    pricing_availability: "available",
    usage_availability: "available",
    manual_usage_calibration: false,
    quota_unit: "tokens",
    model_source: "opencode_get_models",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions", "responses", "messages"],
    form_fields: LEGACY_GO_FIELDS,
    model_aliases: [],
    legacy: true,
    dynamic: false,
  },
  {
    id: "opencode-zen-free",
    provider_id: "opencode-zen-free",
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: "Zen Free",
    display_family: "OpenCode",
    label: "Zen Free",
    kind: "free",
    credential_kind: "none",
    quota_scope: "egress-ip",
    singleton: true,
    creation_availability: "unavailable",
    creation_unavailable_reason: "singleton_managed",
    verification_policy: "not_required",
    verification_runtime_availability: "not_applicable",
    routable: true,
    managed_registration: false,
    pricing_availability: "not_applicable",
    usage_availability: "unavailable",
    manual_usage_calibration: false,
    quota_unit: "requests",
    model_source: "official_zen",
    key_prefix: null,
    auth_schemes: [],
    upstream_protocols: ["chat_completions", "responses", "messages"],
    form_fields: [],
    model_aliases: [],
    legacy: true,
    dynamic: false,
  },
];

function surfaceKind(entry: ProviderCatalogEntry): PlanKind {
  if (entry.provider_id === "custom") return "custom";
  if (entry.credential_kind === "none") return "free";
  if (entry.offering === "plan" && entry.usage_availability !== "unavailable") return "quota";
  return "api-key";
}

export function providerSurfaceFromCatalog(entry: ProviderCatalogEntry): ProviderSurface {
  const label = entry.display_name.trim() || entry.provider_id;
  return {
    ...entry,
    id: entry.provider_id,
    label,
    kind: surfaceKind(entry),
    legacy: false,
    dynamic: entry.model_source === "dynamic_provider",
    form_fields: entry.form_fields.map((field) => ({ ...field })),
    auth_schemes: [...entry.auth_schemes],
    upstream_protocols: [...entry.upstream_protocols],
    model_aliases: [...entry.model_aliases],
  };
}

/**
 * Catalog success is complete authority, including an empty array. Only an
 * unavailable catalog receives the narrow Go/Zen offline projection.
 */
export function providerSurfaces(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): ProviderSurface[] {
  if (catalog != null) return catalog.map(providerSurfaceFromCatalog);
  return OFFLINE_SURFACES.map((surface) => ({
    ...surface,
    form_fields: surface.form_fields.map((field) => ({ ...field })),
    auth_schemes: [...surface.auth_schemes],
    upstream_protocols: [...surface.upstream_protocols],
    model_aliases: [...surface.model_aliases],
  }));
}

/** Stable fallback import target; the chooser must never open without a plan. */
export const OPENCODE_GO_PLAN = OFFLINE_SURFACES[0]!;
export const ZEN_FREE_PLAN = OFFLINE_SURFACES[1]!;

export function findCatalogEntry(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  providerId: string,
): ProviderCatalogEntry | undefined {
  return catalog?.find((entry) => entry.provider_id === providerId);
}

export function findPlanDefinition(
  providerId: string,
  catalog?: readonly ProviderCatalogEntry[] | null,
): ProviderSurface | undefined {
  return providerSurfaces(catalog).find((surface) => surface.provider_id === providerId);
}

/** The provider surface an account belongs to; unknown providers return null. */
export function planForAccount(
  account: Pick<Account, "provider_id">,
  catalog?: readonly ProviderCatalogEntry[] | null,
): ProviderSurface | null {
  return findPlanDefinition(account.provider_id, catalog) ?? null;
}

/** Catalog display name, narrow offline label, then the raw provider id. */
export function planLabel(
  account: Pick<Account, "provider_id">,
  catalog?: readonly ProviderCatalogEntry[] | null,
): string {
  return findPlanDefinition(account.provider_id, catalog)?.label ?? account.provider_id;
}

export function planFamilyLabel(
  plan: ProviderSurface,
  catalog?: readonly ProviderCatalogEntry[] | null,
): string {
  return findCatalogEntry(catalog, plan.provider_id)?.display_name.trim()
    || plan.label
    || plan.provider_id;
}

/** Semantic reason a catalog surface cannot create an account. */
export type PlanCreateDisabledReasonCode =
  | "singleton_managed"
  | "catalog_unavailable"
  | "catalog_entry_missing"
  | "creation_unavailable";

export const PLAN_CREATE_DISABLED_REASON_KEYS: Record<PlanCreateDisabledReasonCode, MessageKey> = {
  singleton_managed: "单例方案由系统自动管理",
  catalog_unavailable: "供应商目录加载失败",
  catalog_entry_missing: "供应商目录未提供该方案",
  creation_unavailable: "该方案暂不可用",
};

/** Reason this catalog surface cannot create an account, or null when allowed. */
export function planCreateDisabledReason(
  plan: ProviderSurface,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): PlanCreateDisabledReasonCode | null {
  if (plan.singleton) return "singleton_managed";
  if (catalog == null) return plan.provider_id === "opencode" ? null : "catalog_unavailable";
  const entry = findCatalogEntry(catalog, plan.provider_id);
  if (!entry) return "catalog_entry_missing";
  if (entry.creation_availability !== "available") return "creation_unavailable";
  return null;
}
