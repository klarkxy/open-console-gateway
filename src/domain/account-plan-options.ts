import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { MessageKey } from "../i18n/index.ts";
import type { PlanDefinition } from "./plans.ts";
import {
  providerSurfaces,
  planFamilyLabel,
  planCreateDisabledReason,
} from "./plans.ts";

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

function surfaceOption(
  plan: PlanDefinition,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): PlanOption {
  const reason = planCreateDisabledReason(plan, catalog);
  const dynamic = plan.dynamic;
  return {
    optionId: plan.provider_id,
    plan,
    label: planFamilyLabel(plan, catalog),
    source: dynamic ? "user-defined" : "builtin",
    disabled: Boolean(reason),
    disabledReason: reason ?? "",
    creationHint: dynamic && !reason ? "账号不拥有 Endpoint、协议或模型映射。" : "",
    managed: !reason && plan.managed_registration,
  };
}

export function buildPlanOptions(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): PlanOption[] {
  return providerSurfaces(catalog)
    .filter((surface) => !surface.singleton)
    .map((surface) => surfaceOption(surface, catalog));
}

export interface PlanOfferingSplit {
  /** Built-in subscription families first, then saved plan-offering Providers. */
  plan: PlanOption[];
  /** Custom API first, then account-owned user-defined API Providers. */
  api: PlanOption[];
}

/**
 * Offering split for the Add Account chooser. Structural only: the custom
 * plan kind heads the API side and every option keeps its own disabled reason.
 * Offering is the catalog row's persisted value; no preset inference occurs.
 */
export function splitPlanOptionsByOffering(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  _dynamicPresetIds?: ReadonlyMap<string, string | null> | null,
): PlanOfferingSplit {
  const options = buildPlanOptions(catalog);
  return {
    plan: options.filter((option) => option.plan.offering === "plan"),
    api: options.filter((option) => option.plan.offering === "api"),
  };
}
