import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import type {
  CustomEndpointContract,
  ProviderCatalogEntry,
  ProviderContractGroup,
  ProviderContractsResponse,
  ProviderProtocol,
} from "../api/providers.ts";
import {
  applyModelContractToResponse,
  buildPreferredProtocolOverrides,
  buildModelToggleOverrides,
  catalogRefreshSupported,
  effectiveModelTestProtocol,
  findAccountScopeView,
  flattenProviderScopes,
  isSafeSourceUrl,
  modelEffectiveOn,
  modelTargetProtocol,
  normalizeProviderContractsResponse,
  providerScopeKey,
  type ProviderModelContract,
  type ProviderScopeView,
} from "./provider-contracts.ts";

const catalogEntry = (
  provider_id: string,
  display_name: string,
): ProviderCatalogEntry => ({
  provider_id,
  origin: "builtin",
  editable: false,
  deletable: false,
  offering: "plan",
  display_name,
  display_family: provider_id,
  credential_kind: "api_key",
  quota_scope: "key",
  singleton: false,
  creation_availability: "available",
  verification_policy: "not_required",
  verification_runtime_availability: "optional",
  routable: true,
  managed_registration: false,
  pricing_availability: "available",
  usage_availability: "available",
  manual_usage_calibration: false,
  quota_unit: "usd",
  model_source: "test",
  auth_schemes: ["bearer"],
  upstream_protocols: ["chat_completions"],
  form_fields: [],
  model_aliases: [],
});

function modelContract(
  modelId: string,
  enabled: Partial<Record<ProviderProtocol, boolean>> = {},
  alias = "",
): ProviderModelContract {
  const protocols = {
    chat_completions: {
      protocol: "chat_completions" as const,
      available: true,
      enabled: enabled.chat_completions ?? false,
      source: "static" as const,
      verified_at: null,
      observed_at: null,
      last_probe_result: null,
      last_probe_at: null,
      last_probe_error: null,
      override: "auto" as const,
    },
    responses: {
      protocol: "responses" as const,
      available: true,
      enabled: enabled.responses ?? false,
      source: "preset" as const,
      verified_at: null,
      observed_at: null,
      last_probe_result: null,
      last_probe_at: null,
      last_probe_error: null,
      override: "auto" as const,
    },
    messages: {
      protocol: "messages" as const,
      available: false,
      enabled: enabled.messages ?? false,
      source: "static" as const,
      verified_at: null,
      observed_at: null,
      last_probe_result: null,
      last_probe_at: null,
      last_probe_error: null,
      override: "auto" as const,
    },
  };
  return {
    alias,
    model_id: modelId,
    preferred_protocol: "responses",
    protocols,
    routable: Object.values(enabled).some(Boolean),
    disabled_reasons: [],
  };
}

function providerGroup(overrides: Partial<ProviderContractGroup> = {}): ProviderContractGroup {
  return {
    scope_kind: "provider",
    scope_id: "opencode",
    provider_id: "opencode",
    static_protocol_snapshot_date: "2026-08-14",
    accounts: [{ id: "go-1", name: "Go 1", enabled: true, verification_status: "not_required" }],
    catalog: {
      source: "static",
      source_url: "https://opencode.ai/docs/go/",
      refreshed_at: null,
      models: ["gpt-5.6-luna"],
      refresh_supported: true,
    },
    models: [modelContract("gpt-5.6-luna", { chat_completions: true, responses: true })],
    pricing: { availability: "available" },
    usage: { availability: "available" },
    card: {
      fetch_zen_models: false,
      discover_models: false,
      protocol_probe: true,
      catalog_refresh: true,
    },
    catalog_routable: true,
    production_inference: true,
    disabled_reasons: [],
    revision: 3,
    ...overrides,
  };
}

function customEndpoint(overrides: Partial<CustomEndpointContract> = {}): CustomEndpointContract {
  return {
    scope_kind: "custom_endpoint",
    scope_id: "custom-1",
    provider_id: "custom",
    account: { id: "custom-1", name: "Home Lab", enabled: true, verification_status: "verified" },
    catalog: {
      source: "account_declared",
      source_url: "",
      refreshed_at: null,
      models: ["local-model"],
      refresh_supported: false,
    },
    models: [modelContract("local-model", { chat_completions: true })],
    pricing: { availability: "unpriced" },
    usage: { availability: "unavailable" },
    card: {
      fetch_zen_models: false,
      discover_models: true,
      protocol_probe: true,
      catalog_refresh: false,
    },
    catalog_routable: true,
    production_inference: true,
    disabled_reasons: [],
    revision: 2,
    ...overrides,
  };
}

function contracts(overrides: Partial<ProviderContractsResponse> = {}): ProviderContractsResponse {
  return {
    revision: 11,
    providers: [providerGroup()],
    custom_endpoints: [customEndpoint()],
    ...overrides,
  };
}

function account(overrides: Partial<Account> = {}): Account {
  return {
    id: "go-1",
    name: "Go 1",
    username: "",
    password: "",
    key: "sk-test",
    enabled: true,
    account_type: "key",
    setup_step: "ready",
    provider_id: "opencode",
    credential_kind: "api_key",
    quota_scope: "key",
    purchase_date: "2026-01-01",
    expires_on: "2027-01-01",
    cooldown_until: null,
    cooldown_generic_until: null,
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: null,
    last_error: null,
    auth_error: null,
    notes: "",
    usage_sync_last_success_at: null,
    usage_sync_next_allowed_at: null,
    created_at: "",
    updated_at: "",
    verification_status: "not_required",
    connection_verified_at: null,
    verification_error: null,
    plan_routable: true,
    model_capabilities: [],
    ...overrides,
  };
}

test("scope keys round-trip and accounts match backend-owned exact scopes", () => {
  assert.equal(providerScopeKey("provider", "command-code"), "provider:command-code");
  const scopes = flattenProviderScopes(normalizeProviderContractsResponse(contracts()));
  assert.equal(findAccountScopeView(scopes, account())?.scope_id, "opencode");
  assert.equal(findAccountScopeView(scopes, account({
    id: "c1",
    provider_id: "custom",
  })), undefined);
  assert.equal(findAccountScopeView(scopes, account({
    id: "custom-1",
    provider_id: "custom",
  }))?.scope_id, "custom-1");
});

test("account scope matching distinguishes providers", () => {
  const scopes = flattenProviderScopes(normalizeProviderContractsResponse(contracts({
    providers: [
      providerGroup(),
      providerGroup({
        scope_id: "command-code",
        provider_id: "command-code",
        accounts: [{ id: "goat-1", name: "GOAT 1", enabled: true, verification_status: "not_required" }],
      }),
    ],
  })));
  assert.equal(findAccountScopeView(scopes, account({
    id: "goat-1",
    provider_id: "command-code",
  }))?.scope_id, "command-code");
});

test("flatten keeps built-in providers grouped and Custom endpoints unflattened", () => {
  const catalog = [
    catalogEntry("opencode", "OpenCode Go"),
    catalogEntry("custom", "Custom API"),
  ];
  const scopes = flattenProviderScopes(normalizeProviderContractsResponse(contracts({
    providers: [
      providerGroup(),
    ],
    custom_endpoints: [
      customEndpoint(),
      customEndpoint({
        scope_id: "custom-2",
        account: { id: "custom-2", name: "Office", enabled: false, verification_status: "pending" },
      }),
    ],
  })), catalog);

  assert.deepEqual(scopes.map(({ key }) => key), [
    "provider:opencode",
    "custom_endpoint:custom-1",
    "custom_endpoint:custom-2",
  ]);
  assert.equal(scopes[1]?.label, "Home Lab");
  assert.equal(scopes[2]?.label, "Office");
});

test("normalization preserves a provider model alias alongside its raw id", () => {
  const response = normalizeProviderContractsResponse(contracts({
    providers: [providerGroup({
      models: [modelContract("upstream-model-2026", { responses: true }, "gpt-5.6-luna")],
    })],
  }));
  const model = flattenProviderScopes(response)[0]?.models[0];
  assert.equal(model?.alias, "gpt-5.6-luna");
  assert.equal(model?.model_id, "upstream-model-2026");
});

test("refresh and probe capability follow card/catalog facts, not raw provider ids", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const custom = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[1]!;
  assert.equal(catalogRefreshSupported(go), true);
  assert.equal(catalogRefreshSupported(custom), false);
});

test("source URLs with credentials are not treated as safe to render", () => {
  assert.equal(isSafeSourceUrl("https://opencode.ai/zen/v1/models"), true);
  assert.equal(isSafeSourceUrl("http://127.0.0.1:8080/v1/models"), true);
  assert.equal(isSafeSourceUrl("https://user:secret@opencode.ai/zen/v1/models"), false);
  assert.equal(isSafeSourceUrl("javascript:alert(1)"), false);
});

test("returned model contracts merge into the last good provider response", () => {
  const next = modelContract("gpt-5.6-luna", { messages: true });
  next.protocols.messages = {
    ...next.protocols.messages!,
    available: true,
    enabled: true,
    source: "probe_confirmed",
    last_probe_result: "success",
    last_probe_at: "2026-08-22T01:00:00Z",
  };
  const merged = applyModelContractToResponse(
    contracts(),
    { scope_kind: "provider", scope_id: "opencode" },
    next,
  );
  assert.equal(
    merged.providers[0]?.models[0]?.protocols.messages?.source,
    "probe_confirmed",
  );
});

test("a row test submits only the effective preferred enabled protocol or the first enabled fallback", () => {
  // Preferred protocol enabled: it wins.
  assert.equal(
    effectiveModelTestProtocol(modelContract("m", { responses: true, chat_completions: true })),
    "responses",
  );
  // Preferred disabled: the first enabled protocol in fixed order is the fallback.
  assert.equal(
    effectiveModelTestProtocol(modelContract("m", { chat_completions: true })),
    "chat_completions",
  );
  const messagesOnly = modelContract("m", {});
  messagesOnly.protocols.messages = { ...messagesOnly.protocols.messages!, enabled: true };
  assert.equal(effectiveModelTestProtocol(messagesOnly), "messages");
  // Nothing enabled means no test is submitted at all.
  assert.equal(effectiveModelTestProtocol(modelContract("m", {})), null);
  assert.equal(effectiveModelTestProtocol(undefined), null);
});

// Helpers for the per-row protocol choice tests below.
function cnScope(
  overrides: Partial<ProviderContractGroup> = {},
  models: ProviderModelContract[] = [],
): ProviderScopeView {
  return {
    key: "provider:minimax",
    scope_kind: "provider",
    scope_id: "minimax",
    provider_id: "minimax",
    static_protocol_snapshot_date: "2026-08-14",
    label: "MiniMax CN",
    accounts: [{ id: "minimax-1", name: "MiniMax CN 1", enabled: true, verification_status: "not_required" }],
    catalog: {
      source: "static",
      source_url: "",
      refreshed_at: null,
      models: [],
      refresh_supported: false,
    },
    models,
    pricing: { availability: "unpriced" },
    usage: { availability: "unavailable" },
    card: {
      fetch_zen_models: false,
      discover_models: false,
      protocol_probe: false,
      catalog_refresh: false,
    },
    catalog_routable: true,
    production_inference: true,
    disabled_reasons: [],
    revision: 1,
    ...overrides,
  };
}

function cnModel(
  modelId: string,
  preferred: ProviderProtocol,
  enabled: Partial<Record<ProviderProtocol, boolean>> = {},
): ProviderModelContract {
  const base = modelContract(modelId, enabled);
  // Real CN models publish no Responses evidence (the slot is null upstream
  // and absent from the DTO), so override batches can never touch it.
  delete base.protocols.responses;
  return {
    ...base,
    preferred_protocol: preferred,
  };
}

function noProtocolModel(modelId: string): ProviderModelContract {
  return {
    alias: "",
    model_id: modelId,
    preferred_protocol: "chat_completions",
    protocols: {},
    routable: false,
    disabled_reasons: ["no_supported_protocol"],
  };
}

test("modelTargetProtocol returns the enabled choice or preferred for a CN two-protocol scope", () => {
  const scope = cnScope();
  // Preferred enabled → it wins.
  const preferred = cnModel("m", "messages", { messages: true });
  assert.equal(modelTargetProtocol(preferred, scope), "messages");
  // Preferred disabled but the other one enabled → the other one wins.
  const otherEnabled = cnModel("m", "messages", { chat_completions: true });
  assert.equal(modelTargetProtocol(otherEnabled, scope), "chat_completions");
  // Nothing enabled → preferred is still surfaced (so the radio group has a
  // current selection rather than blank).
  const noneEnabled = cnAvailableModel("m", "messages", {});
  assert.equal(modelTargetProtocol(noneEnabled, scope), "messages");
});

test("modelTargetProtocol derives the per-model protocol for single-protocol scopes", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const m = go.models[0]!;
  // The fixture model's preferred protocol is Responses and it is available,
  // so the row targets Responses even though the scope's other rows are Chat.
  assert.equal(modelTargetProtocol(m, go), "responses");
  // A model with no evidence under a single-protocol scope is null (no static
  // protocol to display, no toggle target either).
  const noProto = noProtocolModel("ghost");
  assert.equal(modelTargetProtocol(noProto, go), null);
});

test("modelTargetProtocol follows the model's own protocol in mixed single-protocol scopes", () => {
  // Regression: OpenCode Go mixes Chat, Responses and Messages rows. A
  // Responses-only model (e.g. grok-4.6) must not inherit the scope's first
  // Chat model's protocol — its chat row exists but is not available.
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const responsesOnly = modelContract("grok-4.6", { responses: true });
  responsesOnly.protocols.chat_completions = {
    ...responsesOnly.protocols.chat_completions!,
    available: false,
    enabled: false,
  };
  responsesOnly.protocols.messages = {
    ...responsesOnly.protocols.messages!,
    available: false,
    enabled: false,
  };
  const scope: ProviderScopeView = { ...go, models: [go.models[0]!, responsesOnly] };
  assert.equal(modelTargetProtocol(responsesOnly, scope), "responses");
  assert.equal(modelEffectiveOn(responsesOnly, scope), true);
  // Preferred unavailable and nothing else available → no operable target.
  const dead = modelContract("minimax-m2.7-highspeed", {});
  dead.protocols.chat_completions = { ...dead.protocols.chat_completions!, available: false };
  dead.protocols.responses = { ...dead.protocols.responses!, available: false };
  dead.protocols.messages = { ...dead.protocols.messages!, available: false };
  assert.equal(modelTargetProtocol(dead, scope), null);
  assert.equal(modelEffectiveOn(dead, scope), false);
});

test("modelEffectiveOn follows the target protocol's enabled flag", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const m = go.models[0]!;
  assert.equal(modelEffectiveOn(m, go), true);
  const off = modelContract("gpt-5.6-luna", {});
  off.protocols.chat_completions = { ...off.protocols.chat_completions!, enabled: false };
  assert.equal(modelEffectiveOn(off, go), false);
  // Refresh-discovered model with no evidence defaults to false.
  assert.equal(modelEffectiveOn(noProtocolModel("ghost"), go), false);
});

test("buildModelToggleOverrides force-enables every available protocol when toggling on", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const overrides = buildModelToggleOverrides(go, ["gpt-5.6-luna"], true);
  assert.ok(overrides.every((row) => row.state === "force_on"));
  assert.ok(overrides.some((row) => row.protocol === "responses"));
  assert.ok(!overrides.some((row) => row.state === "force_off"));
});

test("buildModelToggleOverrides on=true enables GOAT extra models that auto would keep off", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const extra = modelContract("gpt-5.6-extra", {});
  const scope: ProviderScopeView = { ...go, models: [...go.models, extra] };
  const overrides = buildModelToggleOverrides(scope, ["gpt-5.6-extra"], true);
  assert.deepEqual(overrides, [
    { model_id: "gpt-5.6-extra", protocol: "chat_completions", state: "force_on" },
    { model_id: "gpt-5.6-extra", protocol: "responses", state: "force_on" },
  ]);
});

test("buildModelToggleOverrides on=true enables every available protocol when preferred is unavailable", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const fallback = modelContract("fallback-model", {});
  fallback.protocols.responses = { ...fallback.protocols.responses!, available: false };
  const scope: ProviderScopeView = { ...go, models: [...go.models, fallback] };
  const overrides = buildModelToggleOverrides(scope, ["fallback-model"], true);
  assert.deepEqual(overrides, [
    { model_id: "fallback-model", protocol: "chat_completions", state: "force_on" },
  ]);
});

test("buildModelToggleOverrides emits force_off for all writable protocols when toggling off", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const overrides = buildModelToggleOverrides(go, ["gpt-5.6-luna"], false);
  assert.ok(overrides.length > 0);
  assert.ok(overrides.every((row) => row.state === "force_off"));
  assert.ok(overrides.some((row) => row.preferred === true));
});

test("buildModelToggleOverrides handles multi-model batches and skips no-evidence rows", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const ghost = noProtocolModel("ghost");
  const scope: ProviderScopeView = { ...go, models: [...go.models, ghost] };
  const overrides = buildModelToggleOverrides(scope, ["gpt-5.6-luna", "ghost"], true);
  assert.ok(overrides.every((row) => row.model_id === "gpt-5.6-luna"));
  assert.ok(overrides.every((row) => row.state === "force_on"));
});

test("buildPreferredProtocolOverrides force_on the chosen protocol without disabling siblings", () => {
  const scope = cnScope({}, [cnAvailableModel("m", "chat_completions", { chat_completions: true })]);
  const overrides = buildPreferredProtocolOverrides(scope, "m", "messages");
  assert.deepEqual(overrides, [
    { model_id: "m", protocol: "messages", state: "force_on", preferred: true },
  ]);
  const same = buildPreferredProtocolOverrides(scope, "m", "chat_completions");
  assert.deepEqual(same, [
    { model_id: "m", protocol: "chat_completions", state: "force_on", preferred: true },
  ]);
});

test("buildPreferredProtocolOverrides while disabled keeps all protocols off and stores the choice", () => {
  const scope = cnScope({}, [cnAvailableModel("m", "chat_completions", {})]);
  assert.equal(modelEffectiveOn(scope.models[0]!, scope), false);
  const overrides = buildPreferredProtocolOverrides(scope, "m", "messages");
  assert.deepEqual(overrides, [
    { model_id: "m", protocol: "chat_completions", state: "force_off" },
    { model_id: "m", protocol: "messages", state: "force_off", preferred: true },
  ]);
});

test("buildPreferredProtocolOverrides is a no-op when the protocol is not available", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  assert.deepEqual(buildPreferredProtocolOverrides(go, "gpt-5.6-luna", "messages"), []);
  const cnScope_ = cnScope({}, [cnAvailableModel("m", "messages", { messages: true })]);
  assert.deepEqual(
    buildPreferredProtocolOverrides(cnScope_, "m", "responses" as ProviderProtocol),
    [],
  );
});

/** CN fixture where both selectable protocols are genuinely available. */
function cnAvailableModel(
  modelId: string,
  preferred: ProviderProtocol,
  enabled: Partial<Record<ProviderProtocol, boolean>> = {},
): ProviderModelContract {
  const base = cnModel(modelId, preferred, enabled);
  base.protocols.messages = { ...base.protocols.messages!, available: true };
  return base;
}

test("modelTargetProtocol never shows off while an alternate protocol is enabled", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  // Preferred (Responses) is available but disabled; Chat is enabled. The
  // enabled alternate must win so the row reads on.
  const mixed = modelContract("mixed-model", { chat_completions: true });
  assert.equal(mixed.protocols.responses!.available, true);
  assert.equal(mixed.protocols.responses!.enabled, false);
  assert.equal(modelTargetProtocol(mixed, go), "chat_completions");
  assert.equal(modelEffectiveOn(mixed, go), true);
});

test("CN sequence: switch persists the choice, disable keeps it, reload shows it, enable returns both on", () => {
  const onChatScope = cnScope({}, [cnAvailableModel("m", "chat_completions", { chat_completions: true })]);
  assert.deepEqual(buildPreferredProtocolOverrides(onChatScope, "m", "messages"), [
    { model_id: "m", protocol: "messages", state: "force_on", preferred: true },
  ]);
  const onMessagesScope = cnScope({}, [cnAvailableModel("m", "messages", { messages: true })]);
  assert.deepEqual(buildModelToggleOverrides(onMessagesScope, ["m"], false), [
    { model_id: "m", protocol: "chat_completions", state: "force_off" },
    { model_id: "m", protocol: "messages", state: "force_off", preferred: true },
  ]);
  const reloadedScope = cnScope({}, [cnAvailableModel("m", "messages", {})]);
  const reloaded = reloadedScope.models[0]!;
  assert.equal(modelTargetProtocol(reloaded, reloadedScope), "messages");
  assert.equal(modelEffectiveOn(reloaded, reloadedScope), false);
  assert.deepEqual(buildModelToggleOverrides(reloadedScope, ["m"], true), [
    { model_id: "m", protocol: "chat_completions", state: "force_on" },
    { model_id: "m", protocol: "messages", state: "force_on" },
  ]);
});

test("CN enable only force-on available protocols when preferred is unavailable", () => {
  const scope = cnScope({}, [cnModel("m", "messages", {})]);
  assert.deepEqual(buildModelToggleOverrides(scope, ["m"], true), [
    { model_id: "m", protocol: "chat_completions", state: "force_on" },
  ]);
});

test("GOAT-style chat-only models write only their single legal protocol row", () => {
  const go = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[0]!;
  const chatOnly = modelContract("goat-chat-only", {});
  delete chatOnly.protocols.responses;
  delete chatOnly.protocols.messages;
  const scope: ProviderScopeView = { ...go, models: [...go.models, chatOnly] };
  // Enabling force-enables Chat alone — no Responses/Messages rows exist to write.
  assert.deepEqual(buildModelToggleOverrides(scope, ["goat-chat-only"], true), [
    { model_id: "goat-chat-only", protocol: "chat_completions", state: "force_on" },
  ]);
  // Disabling writes only the same single legal row.
  assert.deepEqual(buildModelToggleOverrides(scope, ["goat-chat-only"], false), [
    { model_id: "goat-chat-only", protocol: "chat_completions", state: "force_off", preferred: true },
  ]);
});

test("Custom endpoint batches write only declared (available) protocol rows", () => {
  // The merged Custom contract retains all three rows, but only the declared
  // protocol is available — and only that row is writable upstream.
  const custom = flattenProviderScopes(normalizeProviderContractsResponse(contracts()))[1]!;
  assert.equal(custom.scope_kind, "custom_endpoint");
  const declared = modelContract("local-model", {});
  declared.protocols.responses = { ...declared.protocols.responses!, available: false };
  const scope: ProviderScopeView = { ...custom, models: [declared] };
  assert.deepEqual(buildModelToggleOverrides(scope, ["local-model"], true), [
    { model_id: "local-model", protocol: "chat_completions", state: "force_on" },
  ]);
  assert.deepEqual(buildModelToggleOverrides(scope, ["local-model"], false), [
    { model_id: "local-model", protocol: "chat_completions", state: "force_off" },
  ]);
});
