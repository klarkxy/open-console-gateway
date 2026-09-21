import assert from "node:assert/strict";
import test from "node:test";
import type { Destination } from "../api/destinations.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  mergeDiscoveredAccountCapabilities,
  mergeDiscoveredCatalogModels,
  usageCompanionCatalog,
  usageCompanionCatalogLockKey,
} from "./usage-refresh-catalog.ts";

function destination(overrides: Partial<Destination> = {}): Destination {
  return {
    adapter: "http",
    legacy: { kind: "dynamic", id: "dest-1" },
    auth_scheme: "bearer",
    base_url: "https://api.stepfun.com/v1/chat/completions",
    brand_family: "stepfun",
    capabilities: {
      billing_tier_required: false,
      discoverable_models: true,
      external_integration: false,
      identity_headers: false,
      managed_signup: false,
      observer: false,
      official_balance_probe: [],
      redirect_policy: "no_follow",
      testable: true,
    },
    catalog: [],
    enabled: true,
    id: "dest-1",
    max_credentials: null,
    name: "StepFun API (CN)",
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

function catalogEntry(
  providerId: string,
  usageAvailability: ProviderCatalogEntry["usage_availability"],
  extras: Partial<ProviderCatalogEntry> = {},
): ProviderCatalogEntry {
  return {
    provider_id: providerId,
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: providerId,
    display_family: providerId,
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    creation_unavailable_reason: null,
    verification_policy: "not_required",
    verification_runtime_availability: "available",
    routable: true,
    managed_registration: false,
    pricing_availability: "available",
    usage_availability: usageAvailability,
    manual_usage_calibration: false,
    quota_unit: "tokens",
    model_source: "official",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
    ...extras,
  };
}

test("built-in quota cards refresh the shared Provider catalog", () => {
  assert.deepEqual(
    usageCompanionCatalog({
      providerId: "opencode",
      catalog: [catalogEntry("opencode", "available")],
      destination: destination({
        adapter: "opencode_go",
        legacy: { kind: "builtin", id: "opencode" },
        capabilities: { ...destination().capabilities, discoverable_models: false },
      }),
    }),
    { kind: "provider_catalog", providerId: "opencode" },
  );
  assert.deepEqual(
    usageCompanionCatalog({
      providerId: "command-code",
      catalog: [catalogEntry("command-code", "available")],
      destination: destination({
        adapter: "goat",
        legacy: { kind: "builtin", id: "command-code" },
        capabilities: { ...destination().capabilities, discoverable_models: false },
      }),
    }),
    { kind: "provider_catalog", providerId: "command-code" },
  );
});

test("Ollama and Zen cards do not piggyback a catalog refresh on quota", () => {
  assert.equal(
    usageCompanionCatalog({
      providerId: "ollama",
      catalog: [catalogEntry("ollama", "local_state")],
      destination: destination({
        adapter: "ollama",
        legacy: { kind: "builtin", id: "ollama" },
        capabilities: { ...destination().capabilities, discoverable_models: false },
      }),
    }).kind,
    "none",
  );
  assert.equal(
    usageCompanionCatalog({
      providerId: "opencode-zen-free",
      catalog: null,
      destination: destination({
        adapter: "zen",
        legacy: { kind: "builtin", id: "opencode-zen-free" },
        capabilities: { ...destination().capabilities, discoverable_models: false },
      }),
    }).kind,
    "none",
  );
});

test("platform parents keep their own Key-model fetch path", () => {
  assert.equal(
    usageCompanionCatalog({
      providerId: "custom",
      catalog: [catalogEntry("custom", "unavailable", { offering: "api", model_source: "custom" })],
      destination: destination({
        legacy: { kind: "platform_parent", id: "plat-1" },
        capabilities: { ...destination().capabilities, observer: true, discoverable_models: true },
      }),
    }).kind,
    "none",
  );
});

test("user-defined HTTP destinations discover into the destination catalog", () => {
  assert.deepEqual(
    usageCompanionCatalog({
      providerId: "11111111-1111-1111-1111-111111111111",
      catalog: [catalogEntry("11111111-1111-1111-1111-111111111111", "unavailable", {
        origin: "preset",
        offering: "api",
        model_source: "dynamic_provider",
      })],
      destination: destination(),
    }),
    { kind: "http_destination" },
  );
});

test("legacy Custom API discovers into the account model list", () => {
  assert.deepEqual(
    usageCompanionCatalog({
      providerId: "custom",
      catalog: [catalogEntry("custom", "unavailable", { offering: "api", model_source: "custom" })],
      destination: destination({
        legacy: { kind: "custom_account", id: "acc-1" },
      }),
    }),
    { kind: "http_account" },
  );
});

test("catalog merge keeps existing enablement and defaults new rows off", () => {
  const merged = mergeDiscoveredCatalogModels(
    [{
      enabled: true,
      preferred: "chat_completions",
      protocols: ["chat_completions"],
      public_model: "step-3.5-flash",
      upstream_model: "step-3.5-flash",
      upstream_override: null,
    }],
    ["step-3.5-flash", "step-5-preview", ""],
    "chat_completions",
  );
  assert.equal(merged.added, 1);
  assert.equal(merged.catalog[0]?.enabled, true);
  assert.equal(merged.catalog[1]?.public_model, "step-5-preview");
  assert.equal(merged.catalog[1]?.enabled, false);
});

test("account capability merge keeps curated rows and appends discoveries", () => {
  const merged = mergeDiscoveredAccountCapabilities(
    [{
      public_model: "deepseek-chat",
      upstream_model: "deepseek-chat",
      protocol: "chat_completions",
      source: "manual",
      verified_at: null,
    }],
    ["deepseek-chat", "deepseek-reasoner"],
    "chat_completions",
  );
  assert.equal(merged.added, 1);
  assert.equal(merged.capabilities[0]?.source, "manual");
  assert.equal(merged.capabilities[1]?.public_model, "deepseek-reasoner");
  assert.equal(merged.capabilities[1]?.source, "discovery");
});

test("in-flight lock is per Provider catalog, not per Key", () => {
  assert.equal(
    usageCompanionCatalogLockKey({ kind: "provider_catalog", providerId: "command-code" }, "acc-a", "dest-a"),
    "provider:command-code",
  );
  assert.equal(
    usageCompanionCatalogLockKey({ kind: "http_account" }, "acc-a", "dest-a"),
    "account:acc-a",
  );
  assert.equal(usageCompanionCatalogLockKey({ kind: "none" }, "acc-a", null), null);
});
