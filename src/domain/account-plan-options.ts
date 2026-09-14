import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { MessageKey } from "../i18n/index.ts";
import type { PlanDefinition } from "./plans.ts";
import {
  PLAN_DEFINITIONS,
  dynamicPlanDefinition,
  planFamilyLabel,
  planCreateDisabledReason,
} from "./plans.ts";
import { isDynamicCatalogEntry } from "./dynamic-provider.ts";
import { providerPresetOfferingForId } from "./provider-presets.ts";

/**
 * Plan-option list for the Add Account chooser. Backend-owned singletons
 * (Zen Free) are omitted: they are not created here. Remaining families stay
 * visible so unavailable choices still explain why they cannot be created.
 */

export interface PlanOption {
  optionId: string;
  plan: PlanDefinition;
  label: string;
  source: "builtin" | "user-defined";
  disabled: boolean;
  disabledReason: MessageKey | "";
  /** Honest copy for selectable-but-not-yet-routable families. */
  creationHint: MessageKey | "";
  managed: boolean;
}

function builtinOption(
  plan: PlanDefinition,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): PlanOption {
  const reason = planCreateDisabledReason(plan, catalog);
  return {
    optionId: plan.id,
    plan,
    label: planFamilyLabel(plan, catalog),
    source: "builtin",
    disabled: Boolean(reason),
    disabledReason: reason ?? "",
    creationHint: "",
    managed: !reason && plan.managed_registration,
  };
}

function dynamicOption(entry: ProviderCatalogEntry): PlanOption {
  const plan = dynamicPlanDefinition(entry);
  const blocked = entry.singleton || entry.creation_availability !== "available";
  const noAuthSingleton = entry.singleton || entry.credential_kind === "none";
  return {
    optionId: entry.provider_id,
    plan,
    label: entry.display_name || entry.provider_id,
    source: "user-defined",
    disabled: blocked,
    disabledReason: blocked
      ? (noAuthSingleton ? "无鉴权供应商只能有一个账号。" : "该方案暂不可用")
      : "",
    creationHint: blocked ? "" : "账号不拥有 Endpoint、协议或模型映射。",
    managed: false,
  };
}

export function buildPlanOptions(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): PlanOption[] {
  const builtin = PLAN_DEFINITIONS.filter((plan) => !plan.singleton).map((plan) => (
    builtinOption(plan, catalog)
  ));
  const dynamic = (catalog ?? [])
    .filter(isDynamicCatalogEntry)
    .map(dynamicOption);
  return [...builtin, ...dynamic];
}

export interface PlanOfferingSplit {
  /** Built-in subscription families first, then saved plan-offering Providers. */
  plan: PlanOption[];
  /** Custom API first, then account-owned user-defined API Providers. */
  api: PlanOption[];
}

/**
 * Offering split for the Add Account chooser. Structural only: the custom
 * plan kind heads the API side and every option keeps its own disabled reason
 * instead of a status group. Saved user-defined Providers follow their
 * persisted preset's offering via `dynamicPresetIds` (provider_id → preset_id
 * from the dynamic Provider detail); unknown or unloaded IDs are API.
 */
export function splitPlanOptionsByOffering(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  dynamicPresetIds?: ReadonlyMap<string, string | null> | null,
): PlanOfferingSplit {
  const options = buildPlanOptions(catalog);
  const dynamicOffering = (option: PlanOption): "plan" | "api" => (
    providerPresetOfferingForId(dynamicPresetIds?.get(option.optionId))
  );
  return {
    plan: [
      ...options.filter((option) => option.source === "builtin" && option.plan.kind !== "custom"),
      ...options.filter((option) => option.source === "user-defined" && dynamicOffering(option) === "plan"),
    ],
    api: [
      ...options.filter((option) => option.source === "builtin" && option.plan.kind === "custom"),
      ...options.filter((option) => option.source === "user-defined" && dynamicOffering(option) === "api"),
    ],
  };
}
