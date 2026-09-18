import type { Account } from "../api/dashboard.ts";
import type { Destination } from "../api/destinations.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { ProviderSurface } from "./plans.ts";
import {
  CPA_PROVIDER_ID,
  DEFAULT_PROVIDER_ID,
  OLLAMA_PROVIDER_ID,
  ZEN_FREE_PROVIDER_ID,
} from "./destination-providers.ts";
import { isCustomApiAccount } from "./custom-account.ts";

export interface AccountCapabilities {
  /** How the enable switch persists: generic account toggle, or the dedicated provider-settings write (Zen Free). */
  toggleWrite: "account" | "provider_settings";
  /** Card exposes Test connection. False for external integrations (CPA). */
  testable: boolean;
  /** Card may show purchase date / expiry UI (built-in billed families only). */
  hasExpiry: boolean;
  /** Endpoint, protocol and model mappings live on the account (Custom API). */
  endpointOnAccount: boolean;
  /** Provider supports the managed browser signup flow (OpenCode Go). */
  managedSignup: boolean;
  /** Keys are held by an external integration; no local Key actions (CPA). */
  externalIntegration: boolean;
  /** Account is the backend-owned keyless singleton (Zen Free). */
  keylessSingleton: boolean;
  /** Usage cannot display until a billing tier is chosen (Ollama Cloud). */
  billingTierRequired: boolean;
  /** Which vendor site the overflow menu may open, if any. */
  consoleLink: "opencode" | "ollama" | null;
  /** Account owns an isolated browser profile that can be reset (OpenCode Go). */
  browserProfile: boolean;
  /** Cooldown is tracked only on the free window (Zen Free). */
  freeCooldownOnly: boolean;
}

type AccountRef = Pick<Account, "id" | "provider_id" | "account_type">;

function catalogEntryFor(
  account: Pick<Account, "provider_id">,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): ProviderCatalogEntry | undefined {
  return catalog == null
    ? undefined
    : catalog.find((entry) => entry.provider_id === account.provider_id);
}

/** True when this credential is currently on the managed-signup onboarding path. */
export function isManagedOnboardingAccount(
  account: Pick<Account, "account_type">,
): boolean {
  return account.account_type === "managed";
}

/**
 * True for the OpenCode Go plan when the catalog failed to load and the UI
 * must use the legacy Go fallback.
 */
export function isLegacyGoFallbackPlan(
  plan: Pick<ProviderSurface, "provider_id" | "legacy">,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): boolean {
  return catalog == null && plan.legacy && plan.provider_id === DEFAULT_PROVIDER_ID;
}

/**
 * True for the OpenCode Go plan whose pricing revision may come from the
 * legacy Go pricing snapshot even after a later catalog load succeeds (the
 * snapshot is not cleared on catalog success). Kept separate from the
 * catalog-failed fallback so both truth tables stay exactly as before.
 */
export function usesLegacyGoPricingSnapshot(
  plan: Pick<ProviderSurface, "provider_id">,
): boolean {
  // RFC stage 4: move to catalog/connection capability
  return plan.provider_id === DEFAULT_PROVIDER_ID;
}

/** Card flags from a destination's sealed capabilities, not account identity. */
export function destinationCapabilities(
  destination: Pick<
    Destination,
    "adapter" | "auth_scheme" | "capabilities" | "max_credentials" | "plan"
  >,
): AccountCapabilities {
  const caps = destination.capabilities;
  const keylessSingleton = destination.auth_scheme === "none"
    && destination.max_credentials === 1;
  const managedSignup = caps.managed_signup;
  const go = destination.adapter === "opencode_go";
  const ollama = destination.adapter === "ollama";
  return {
    toggleWrite: keylessSingleton ? "provider_settings" : "account",
    testable: caps.testable,
    hasExpiry: destination.plan != null
      && destination.adapter !== "http"
      && destination.adapter !== "zen"
      && destination.adapter !== "cpa",
    endpointOnAccount: destination.adapter === "http"
      && destination.max_credentials === 1
      && !caps.observer,
    managedSignup,
    externalIntegration: caps.external_integration,
    keylessSingleton,
    billingTierRequired: caps.billing_tier_required,
    consoleLink: go ? "opencode" : ollama ? "ollama" : null,
    browserProfile: go && managedSignup,
    freeCooldownOnly: destination.adapter === "zen",
  };
}

export function accountCapabilities(
  account: AccountRef,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  destination?: Pick<
    Destination,
    "adapter" | "auth_scheme" | "capabilities" | "max_credentials" | "plan"
  > | null,
): AccountCapabilities {
  if (destination) return destinationCapabilities(destination);
  const entry = catalogEntryFor(account, catalog);
  const providerId = account.provider_id;

  const managedSignup = entry
    ? entry.managed_registration
    : providerId === DEFAULT_PROVIDER_ID;

  const keylessSingleton = providerId === ZEN_FREE_PROVIDER_ID
    || Boolean(entry && entry.credential_kind === "none" && entry.singleton);

  const externalIntegration = providerId === CPA_PROVIDER_ID;
  const billingTierRequired = providerId === OLLAMA_PROVIDER_ID;
  const endpointOnAccount = isCustomApiAccount(account);
  const browserProfile = providerId === DEFAULT_PROVIDER_ID;
  const consoleLink = browserProfile
    ? "opencode"
    : billingTierRequired
      ? "ollama"
      : null;

  const hasExpiry = !endpointOnAccount && !keylessSingleton;

  return {
    toggleWrite: keylessSingleton ? "provider_settings" : "account",
    testable: !externalIntegration,
    hasExpiry,
    endpointOnAccount,
    managedSignup,
    externalIntegration,
    keylessSingleton,
    billingTierRequired,
    consoleLink,
    browserProfile,
    freeCooldownOnly: keylessSingleton,
  };
}
