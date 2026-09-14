import type {
  Account,
  AccountCredentialKind,
  AccountQuotaScope,
} from "../api/dashboard.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { MessageKey } from "../i18n/index.ts";
import { isDynamicCatalogEntry } from "./dynamic-provider.ts";

/**
 * The hardcoded plan families of the console. A family maps to one
 * backend provider. The backend owns the DTO fields; this module owns
 * the stable ordering, family ids, and fallback metadata.
 *
 * Availability is never hardcoded: creation/routing/pricing semantics are
 * resolved from `/dashboard/api/v3/providers`. Legacy families
 * (OpenCode Go, Zen Free) keep pre-catalog fallback behavior when the catalog
 * is unreachable; every other family fails closed in that case.
 */

export type PlanId =
  | "opencode-go"
  | "zen-free"
  | "command-code-goat"
  | "minimax-cn"
  | "kimi-cn"
  | "ollama-cloud"
  | "custom-endpoint"
  | "dynamic-http";

export type PlanKind = "quota" | "free" | "api-key" | "custom";

export interface PlanDefinition {
  id: PlanId;
  provider_id: string;
  /** Brand label; shown only when the catalog is absent. */
  label: string;
  kind: PlanKind;
  credential_kind: AccountCredentialKind;
  quota_scope: AccountQuotaScope;
  /** Singleton families (Zen Free) are created and owned by the backend. */
  singleton: boolean;
  /** Managed registration wizard availability (OpenCode Go only). */
  managed_registration: boolean;
  /** Legacy families keep their pre-catalog behavior when the catalog fails. */
  legacy: boolean;
}

export const PLAN_DEFINITIONS: readonly PlanDefinition[] = [
  {
    id: "opencode-go",
    provider_id: "opencode",
    label: "OpenCode Go",
    kind: "quota",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    managed_registration: true,
    legacy: true,
  },
  {
    id: "zen-free",
    provider_id: "opencode-zen-free",
    label: "Zen Free",
    kind: "free",
    credential_kind: "none",
    quota_scope: "egress-ip",
    singleton: true,
    managed_registration: false,
    legacy: true,
  },
  {
    id: "command-code-goat",
    provider_id: "command-code",
    label: "Command Code GOAT",
    kind: "api-key",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    managed_registration: false,
    legacy: false,
  },
  {
    id: "minimax-cn",
    provider_id: "minimax",
    label: "MiniMax CN Token Plan",
    kind: "api-key",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    managed_registration: false,
    legacy: false,
  },
  {
    id: "kimi-cn",
    provider_id: "kimi",
    label: "Kimi Code CN",
    kind: "api-key",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    managed_registration: false,
    legacy: false,
  },
  {
    id: "ollama-cloud",
    provider_id: "ollama",
    label: "Ollama Cloud",
    kind: "api-key",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    managed_registration: false,
    legacy: false,
  },
  {
    id: "custom-endpoint",
    provider_id: "custom",
    label: "Custom API",
    kind: "custom",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    managed_registration: false,
    legacy: false,
  },
];

export function dynamicPlanDefinition(entry: ProviderCatalogEntry): PlanDefinition {
  return {
    id: "dynamic-http",
    provider_id: entry.provider_id,
    label: entry.display_name || entry.provider_id,
    kind: entry.credential_kind === "none" ? "free" : "api-key",
    credential_kind: entry.credential_kind,
    quota_scope: entry.quota_scope,
    singleton: entry.singleton,
    managed_registration: false,
    legacy: false,
  };
}

/** Stable legacy import target; the chooser must never open without a plan. */
export const OPENCODE_GO_PLAN = PLAN_DEFINITIONS.find((plan) => plan.id === "opencode-go")!;

export function findCatalogEntry(
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  providerId: string,
): ProviderCatalogEntry | undefined {
  return catalog?.find((entry) => entry.provider_id === providerId);
}

/**
 * Find a family definition by the exact backend provider id.
 * Custom maps to "custom-endpoint".
 */
export function findPlanDefinition(providerId: string): PlanDefinition | undefined {
  return PLAN_DEFINITIONS.find((plan) => plan.provider_id === providerId);
}

/** The family an account belongs to; unknown providers return null (render raw). */
export function planForAccount(
  account: Pick<Account, "provider_id">,
  catalog?: readonly ProviderCatalogEntry[] | null,
): PlanDefinition | null {
  const builtin = findPlanDefinition(account.provider_id);
  if (builtin) return builtin;
  const entry = findCatalogEntry(catalog, account.provider_id);
  if (entry && isDynamicCatalogEntry(entry)) return dynamicPlanDefinition(entry);
  return null;
}

/**
 * Display label for a provider. Prefers the catalog's display_name, then the
 * static family label, then the raw provider id.
 */
export function planLabel(
  account: Pick<Account, "provider_id">,
  catalog?: readonly ProviderCatalogEntry[] | null,
): string {
  const entry = findCatalogEntry(catalog, account.provider_id);
  if (entry?.display_name) return entry.display_name;
  const definition = findPlanDefinition(account.provider_id);
  if (definition) return definition.label;
  return account.provider_id;
}

/**
 * Label for a plan-family control. Prefers the catalog display name for the
 * family's provider, then the static family label.
 */
export function planFamilyLabel(
  plan: PlanDefinition,
  catalog?: readonly ProviderCatalogEntry[] | null,
): string {
  const entry = findCatalogEntry(catalog, plan.provider_id);
  if (entry?.display_name.trim()) return entry.display_name.trim();
  return plan.label;
}

/** Reason the family cannot be created, or null when it is creatable. */
export function planCreateDisabledReason(
  plan: PlanDefinition,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): MessageKey | null {
  if (plan.singleton) return "单例方案由系统自动管理";
  if (!catalog?.length) return plan.legacy ? null : "服务商目录加载失败";
  const entry = findCatalogEntry(catalog, plan.provider_id);
  if (!entry) return "服务商目录未提供该方案";
  if (entry.creation_availability !== "available") {
    return "该方案暂不可用";
  }
  return null;
}
