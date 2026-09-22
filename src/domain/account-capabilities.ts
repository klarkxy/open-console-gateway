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
  /** Card may show purchase date / expiry when the Plan declares a cadence. */
  hasExpiry: boolean;
  /** Endpoint, protocol and model mappings live on the account (Custom API). */
  endpointOnAccount: boolean;
  /** Provider supports the managed browser signup flow (OpenCode Go). */
  managedSignup: boolean;
  /** Keys are held by an external integration; no local Key actions (CPA). */
  externalIntegration: boolean;
  /** Destination needs no credential and permits only one account. */
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

/** The single capability input consumed by both V4 and the legacy boundary. */
export type AccountCapabilitySource = Pick<
  Destination, "account_controls" | "auth_scheme" | "max_credentials" | "plan"
> & { capabilities: Pick<Destination["capabilities"],
  "testable" | "managed_signup" | "external_integration" | "billing_tier_required"
> };

/** Card flags from explicit destination facts, without provider inference. */
export function destinationCapabilities(destination: AccountCapabilitySource): AccountCapabilities {
  const caps = destination.capabilities;
  const controls = destination.account_controls;
  const windows = destination.plan?.windows ?? [];
  return {
    toggleWrite: controls.toggleWrite,
    testable: caps.testable,
    hasExpiry: destination.plan?.expiry_cadence != null,
    endpointOnAccount: controls.configurationOwner === "account",
    managedSignup: caps.managed_signup,
    externalIntegration: caps.external_integration,
    keylessSingleton: destination.auth_scheme === "none" && destination.max_credentials === 1,
    billingTierRequired: caps.billing_tier_required,
    consoleLink: controls.consoleLink,
    browserProfile: controls.browserProfile,
    freeCooldownOnly: windows.length > 0 && windows.every((window) => window.kind === "free"),
  };
}

/**
 * Compatibility boundary for accounts displayed before the V4 projection loads.
 * Only sealed billed families have an expiry cadence; dynamic offerings do not
 * acquire one merely by calling themselves a Plan.
 */
function legacyCapabilitySource(
  account: AccountRef,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): AccountCapabilitySource {
  const entry = catalogEntryFor(account, catalog);
  const provider = account.provider_id;
  const go = provider === DEFAULT_PROVIDER_ID;
  const zen = provider === ZEN_FREE_PROVIDER_ID;
  const ollama = provider === OLLAMA_PROVIDER_ID;
  const external = provider === CPA_PROVIDER_ID;
  const monthly = [DEFAULT_PROVIDER_ID, "command-code", "minimax", "kimi", OLLAMA_PROVIDER_ID].includes(provider);
  return {
    account_controls: {
      toggleWrite: zen ? "provider_settings" : "account",
      configurationOwner: isCustomApiAccount(account) ? "account" : "destination",
      consoleLink: go ? "opencode" : ollama ? "ollama" : null,
      browserProfile: go,
    },
    auth_scheme: zen || entry?.credential_kind === "none" ? "none" : "bearer",
    max_credentials: zen || entry?.singleton ? 1 : null,
    capabilities: {
      testable: !external,
      managed_signup: entry?.managed_registration ?? go,
      external_integration: external,
      billing_tier_required: ollama,
    },
    plan: monthly || zen ? {
      expiry_cadence: monthly ? "monthly" : null,
      windows: [{ kind: zen ? "free" : "month" }],
      manual_calibration: false,
      pricing_source: "unpriced",
      usage_source: "none",
    } : null,
  };
}

export function accountCapabilities(
  account: AccountRef,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  destination?: AccountCapabilitySource | null,
): AccountCapabilities {
  return destinationCapabilities(destination ?? legacyCapabilitySource(account, catalog));
}
