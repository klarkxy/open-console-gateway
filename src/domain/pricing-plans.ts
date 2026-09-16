import type { PricingSnapshot } from "../api/dashboard";
import type { MessageKey } from "../i18n/index.ts";
import type {
  ProviderCatalogEntry,
  ProviderNeutralPricingSnapshot,
  ProviderPricingResponse,
  StoredProviderPricingSnapshot,
} from "../api/providers.ts";
import {
  type ProviderSurface,
  planFamilyLabel,
  providerSurfaces,
} from "./plans.ts";

export type PricingAvailability = "available" | "unavailable" | "not_applicable" | "unpriced";

export type PlanPricingContent =
  | { kind: "models"; snapshot: PricingSnapshot }
  | { kind: "values"; snapshot: ProviderNeutralPricingSnapshot }
  | { kind: "opaque"; snapshot: Exclude<StoredProviderPricingSnapshot, PricingSnapshot | ProviderNeutralPricingSnapshot> }
  | { kind: "none"; snapshot: null };

export interface PlanPricingGroup {
  plan: ProviderSurface;
  label: string;
  pricingAvailability: PricingAvailability;
  content: PlanPricingContent;
}

export type PlanPricingState =
  | "error"
  | "unavailable"
  | "unpriced"
  | "not_applicable"
  | "available-empty"
  | "available-table";

export type PlanPricingMessageCode =
  | "load_failed"
  | "unavailable"
  | "unpriced"
  | "not_applicable"
  | "empty"
  | "models_note"
  | "estimate_note";

export const PLAN_PRICING_MESSAGE_KEYS: Record<PlanPricingMessageCode, MessageKey> = {
  load_failed: "加载价格表失败：{error}",
  unavailable: "暂无该方案的价格数据",
  unpriced: "该方案未定价",
  not_applicable: "该方案无需价格表",
  empty: "暂无该方案的价格数据",
  models_note: "仅在你主动刷新时访问官方文档，刷新失败则沿用当前快照。",
  estimate_note: "未知价格不会参与费用估算",
};

export interface PlanPricingDisplay {
  state: PlanPricingState;
  messageCode: PlanPricingMessageCode;
  error: string | null;
}

/** Provider-id keyed cache; no frontend family id exists. */
export type ProviderSnapshots = Partial<Record<string, ProviderPricingResponse>>;

function contentFromSnapshot(
  snapshot: StoredProviderPricingSnapshot | undefined,
): PlanPricingContent {
  if (!snapshot) return { kind: "none", snapshot: null };
  if ("models" in snapshot) return { kind: "models", snapshot };
  if ("values" in snapshot) return { kind: "values", snapshot };
  return { kind: "opaque", snapshot };
}

function contentHasRows(content: PlanPricingContent): boolean {
  if (content.kind === "models") return content.snapshot.models.length > 0;
  if (content.kind === "values") return content.snapshot.values.length > 0;
  return content.kind === "opaque";
}

export function resolvePlanPricingDisplay(
  group: PlanPricingGroup,
  error: string | null = null,
): PlanPricingDisplay {
  if (error) return { state: "error", messageCode: "load_failed", error };
  if (group.pricingAvailability === "unavailable") {
    return { state: "unavailable", messageCode: "unavailable", error: null };
  }
  if (group.pricingAvailability === "unpriced") {
    return { state: "unpriced", messageCode: "unpriced", error: null };
  }
  if (group.pricingAvailability === "not_applicable") {
    return { state: "not_applicable", messageCode: "not_applicable", error: null };
  }
  if (!contentHasRows(group.content)) {
    return { state: "available-empty", messageCode: "empty", error: null };
  }
  return {
    state: "available-table",
    messageCode: group.content.kind === "models" ? "models_note" : "estimate_note",
    error: null,
  };
}

function groupForSurface(
  surface: ProviderSurface,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  legacyGoSnapshot: PricingSnapshot | null,
  providerSnapshots: ProviderSnapshots,
): PlanPricingGroup {
  const response = providerSnapshots[surface.provider_id];
  const availability = response?.availability ?? surface.pricing_availability;
  const snapshot = response?.snapshot
    ?? (catalog == null && surface.provider_id === "opencode" ? legacyGoSnapshot ?? undefined : undefined);
  return {
    plan: surface,
    label: planFamilyLabel(surface, catalog),
    pricingAvailability: availability,
    content: availability === "available"
      ? contentFromSnapshot(snapshot)
      : { kind: "none", snapshot: null },
  };
}

/** Default pricing landing: every catalog Plan whose pricing is available. */
export function buildPlanPricingGroups(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  legacyGoSnapshot: PricingSnapshot | null,
  providerSnapshots: ProviderSnapshots,
): PlanPricingGroup[] {
  return providerSurfaces(catalog)
    .filter((surface) => surface.offering === "plan" && surface.pricing_availability === "available")
    .map((surface) => groupForSurface(surface, catalog, legacyGoSnapshot, providerSnapshots));
}

/** Provider detail: preserve the selected row's available/unpriced/unavailable state. */
export function buildScopedPlanPricingGroups(
  providerId: string,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  legacyGoSnapshot: PricingSnapshot | null,
  providerSnapshots: ProviderSnapshots,
): PlanPricingGroup[] {
  if (!providerId) return buildPlanPricingGroups(catalog, legacyGoSnapshot, providerSnapshots);
  return providerSurfaces(catalog)
    .filter((surface) => surface.provider_id === providerId)
    .map((surface) => groupForSurface(surface, catalog, legacyGoSnapshot, providerSnapshots));
}
