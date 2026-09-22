import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  OLLAMA_PROVIDER_ID,
  ZEN_FREE_PROVIDER_ID,
} from "./destination-providers.ts";
import type { Destination } from "../api/destinations.ts";
import {
  accountCapabilities,
  destinationCapabilities,
  isLegacyGoFallbackPlan,
  type AccountCapabilities,
} from "./account-capabilities.ts";

const LAB_PROVIDER_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

function catalogEntry(
  provider_id: string,
  extra: Partial<ProviderCatalogEntry> = {},
): ProviderCatalogEntry {
  return {
    provider_id,
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: provider_id,
    display_family: provider_id,
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    verification_policy: "required",
    verification_runtime_availability: "available",
    routable: true,
    managed_registration: false,
    pricing_availability: "unavailable",
    usage_availability: "unavailable",
    manual_usage_calibration: false,
    quota_unit: "credits",
    model_source: "test",
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
    ...extra,
  };
}

function builtinCatalog(): ProviderCatalogEntry[] {
  return [
    catalogEntry("opencode", {
      display_name: "OpenCode Go",
      managed_registration: true,
      usage_availability: "available",
      pricing_availability: "available",
      quota_unit: "tokens",
    }),
    catalogEntry(ZEN_FREE_PROVIDER_ID, {
      display_name: "Zen Free",
      credential_kind: "none",
      singleton: true,
      usage_availability: "unavailable",
    }),
    catalogEntry("custom", {
      offering: "api",
      display_name: "Custom API",
    }),
    catalogEntry("cpa", {
      offering: "api",
      display_name: "CPA",
      singleton: true,
    }),
    catalogEntry(OLLAMA_PROVIDER_ID, {
      display_name: "Ollama Cloud",
    }),
    catalogEntry(LAB_PROVIDER_ID, {
      origin: "custom",
      offering: "api",
      display_name: "Lab",
      model_source: "dynamic_provider",
    }),
  ];
}

const PLAN_GO: AccountCapabilities = {
  toggleWrite: "account",
  testable: true,
  hasExpiry: true,
  endpointOnAccount: false,
  managedSignup: true,
  externalIntegration: false,
  keylessSingleton: false,
  billingTierRequired: false,
  consoleLink: "opencode",
  browserProfile: true,
  freeCooldownOnly: false,
};

const CUSTOM_API: AccountCapabilities = {
  toggleWrite: "account",
  testable: true,
  hasExpiry: false,
  endpointOnAccount: true,
  managedSignup: false,
  externalIntegration: false,
  keylessSingleton: false,
  billingTierRequired: false,
  consoleLink: null,
  browserProfile: false,
  freeCooldownOnly: false,
};

const USER_DEFINED: AccountCapabilities = {
  toggleWrite: "account",
  testable: true,
  hasExpiry: false,
  endpointOnAccount: false,
  managedSignup: false,
  externalIntegration: false,
  keylessSingleton: false,
  billingTierRequired: false,
  consoleLink: null,
  browserProfile: false,
  freeCooldownOnly: false,
};

const CPA: AccountCapabilities = {
  toggleWrite: "account",
  testable: false,
  hasExpiry: false,
  endpointOnAccount: false,
  managedSignup: false,
  externalIntegration: true,
  keylessSingleton: false,
  billingTierRequired: false,
  consoleLink: null,
  browserProfile: false,
  freeCooldownOnly: false,
};

const ZEN_FREE: AccountCapabilities = {
  toggleWrite: "provider_settings",
  testable: true,
  hasExpiry: false,
  endpointOnAccount: false,
  managedSignup: false,
  externalIntegration: false,
  keylessSingleton: true,
  billingTierRequired: false,
  consoleLink: null,
  browserProfile: false,
  freeCooldownOnly: true,
};

const OLLAMA_CLOUD: AccountCapabilities = {
  toggleWrite: "account",
  testable: true,
  hasExpiry: true,
  endpointOnAccount: false,
  managedSignup: false,
  externalIntegration: false,
  keylessSingleton: false,
  billingTierRequired: true,
  consoleLink: "ollama",
  browserProfile: false,
  freeCooldownOnly: false,
};

test("plan account capabilities follow the OpenCode Go catalog row", () => {
  assert.deepEqual(
    accountCapabilities({ id: "go-1", provider_id: "opencode", account_type: "key" }, builtinCatalog()),
    PLAN_GO,
  );
});

test("custom API capabilities mark the endpoint as account-owned", () => {
  assert.deepEqual(
    accountCapabilities({ id: "custom-1", provider_id: "custom", account_type: "key" }, builtinCatalog()),
    CUSTOM_API,
  );
});

test("user-defined provider capabilities stay generic and have no inferred expiry", () => {
  assert.deepEqual(
    accountCapabilities(
      { id: "lab-1", provider_id: LAB_PROVIDER_ID, account_type: "key" },
      builtinCatalog(),
    ),
    USER_DEFINED,
  );
});

test("CPA capabilities mark the external integration and hide local key actions", () => {
  assert.deepEqual(
    accountCapabilities(
      { id: "cpa-1", provider_id: "cpa", account_type: "key" },
      builtinCatalog(),
    ),
    CPA,
  );
});

test("Zen Free capabilities mark the keyless singleton and provider-settings toggle", () => {
  assert.deepEqual(
    accountCapabilities(
      { id: "zen-1", provider_id: ZEN_FREE_PROVIDER_ID, account_type: "key" },
      builtinCatalog(),
    ),
    ZEN_FREE,
  );
});

test("managed OpenCode Go drafts share the Go destination capabilities", () => {
  assert.deepEqual(
    accountCapabilities(
      { id: "go-draft", provider_id: "opencode", account_type: "managed" },
      builtinCatalog(),
    ),
    PLAN_GO,
  );
});

test("Ollama Cloud capabilities require a billing tier and expose the vendor site", () => {
  assert.deepEqual(
    accountCapabilities(
      { id: "ollama-1", provider_id: OLLAMA_PROVIDER_ID, account_type: "key" },
      builtinCatalog(),
    ),
    OLLAMA_CLOUD,
  );
});

test("catalog-null fallbacks match catalog-present results for built-in kinds", () => {
  const catalog = builtinCatalog();
  const kinds = [
    { id: "go-1", provider_id: "opencode", account_type: "key" as const },
    { id: "custom-1", provider_id: "custom", account_type: "key" as const },
    { id: "cpa-1", provider_id: "cpa", account_type: "key" as const },
    { id: "zen-1", provider_id: ZEN_FREE_PROVIDER_ID, account_type: "key" as const },
    { id: "ollama-1", provider_id: OLLAMA_PROVIDER_ID, account_type: "key" as const },
  ];
  for (const account of kinds) {
    assert.deepEqual(
      accountCapabilities(account, null),
      accountCapabilities(account, catalog),
      account.provider_id,
    );
  }
  assert.equal(
    isLegacyGoFallbackPlan({ provider_id: "opencode", legacy: true }, null),
    true,
  );
  assert.equal(
    isLegacyGoFallbackPlan({ provider_id: "opencode", legacy: true }, catalog),
    false,
  );
});

const DEFAULT_CONTROLS: Destination["account_controls"] = {
  toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false,
};
const monthly: NonNullable<Destination["plan"]> = {
  expiry_cadence: "monthly", manual_calibration: false, pricing_source: "official",
  usage_source: "official_api", windows: [{ kind: "month" }],
};
function destinationFixture(overrides: Partial<Destination> = {}): Destination {
  return {
    id: "destination", name: "Destination", adapter: "http", legacy: { kind: "dynamic", id: "provider" },
    account_controls: DEFAULT_CONTROLS, auth_scheme: "bearer", base_url: null, brand_family: null,
    catalog: [], enabled: true, observer_credential_id: null, protocols: [],
    capabilities: {
      billing_tier_required: false, discoverable_models: false, external_integration: false,
      identity_headers: false, managed_signup: false, observer: false, official_balance_probe: [],
      redirect_policy: "no_follow", testable: true,
    },
    max_credentials: null, plan: null, ...overrides,
  };
}

test("explicit ownership survives multiple Custom credentials and a singleton HTTP provider", () => {
  assert.equal(destinationCapabilities(destinationFixture({
    legacy: { kind: "custom_account", id: "custom" }, max_credentials: null,
    account_controls: { ...DEFAULT_CONTROLS, configurationOwner: "account" },
  })).endpointOnAccount, true);
  assert.equal(destinationCapabilities(destinationFixture({ max_credentials: 1 })).endpointOnAccount, false);
});

test("expiry and free cooldown follow commercial facts independently of adapter", () => {
  assert.equal(destinationCapabilities(destinationFixture({ plan: monthly })).hasExpiry, true);
  assert.equal(destinationCapabilities(destinationFixture({ adapter: "opencode_go" })).hasExpiry, false);
  for (const windows of [[], [{ kind: "free" as const }, { kind: "month" as const }]]) {
    assert.equal(destinationCapabilities(destinationFixture({ plan: { ...monthly, windows } })).freeCooldownOnly, false);
  }
  assert.equal(destinationCapabilities(destinationFixture({
    plan: { ...monthly, expiry_cadence: null, windows: [{ kind: "free" }] },
  })).freeCooldownOnly, true);
});

test("keyless singleton does not imply provider settings or free cooldown", () => {
  const caps = destinationCapabilities(destinationFixture({ auth_scheme: "none", max_credentials: 1 }));
  assert.equal(caps.keylessSingleton, true);
  assert.equal(caps.toggleWrite, "account");
  assert.equal(caps.freeCooldownOnly, false);
});

test("explicit actions remain independent of adapter and managed signup", () => {
  const dest = destinationFixture({ account_controls: {
    toggleWrite: "provider_settings", configurationOwner: "account", consoleLink: "ollama", browserProfile: true,
  } });
  const caps = accountCapabilities({ id: "a", provider_id: "opencode", account_type: "key" }, builtinCatalog(), dest);
  assert.equal(caps.toggleWrite, "provider_settings");
  assert.equal(caps.consoleLink, "ollama");
  assert.equal(caps.browserProfile, true);
  assert.equal(caps.managedSignup, false);
});

test("legacy and destination paths converge for all supported account families", () => {
  const base = destinationFixture();
  const cases: [string, Partial<Destination>][] = [
    ["opencode", { plan: monthly, capabilities: { ...base.capabilities, managed_signup: true },
      account_controls: { ...DEFAULT_CONTROLS, consoleLink: "opencode", browserProfile: true } }],
    ["command-code", { plan: monthly }], ["kimi", { plan: monthly }], ["minimax", { plan: monthly }],
    ["custom", { account_controls: { ...DEFAULT_CONTROLS, configurationOwner: "account" } }],
    ["cpa", { capabilities: { ...base.capabilities, testable: false, external_integration: true } }],
    [LAB_PROVIDER_ID, {}],
    [OLLAMA_PROVIDER_ID, { plan: monthly, capabilities: { ...base.capabilities, billing_tier_required: true },
      account_controls: { ...DEFAULT_CONTROLS, consoleLink: "ollama" } }],
    [ZEN_FREE_PROVIDER_ID, { auth_scheme: "none", max_credentials: 1,
      plan: { ...monthly, expiry_cadence: null, windows: [{ kind: "free" }] },
      account_controls: { ...DEFAULT_CONTROLS, toggleWrite: "provider_settings" } }],
  ];
  for (const [provider_id, facts] of cases) {
    const account = { id: "a", provider_id, account_type: "key" as const };
    for (const catalog of [null, builtinCatalog()]) {
      assert.deepEqual(accountCapabilities(account, catalog), accountCapabilities(account, catalog, destinationFixture(facts)), provider_id);
    }
  }
});
