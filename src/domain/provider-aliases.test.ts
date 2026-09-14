import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import type { ProviderScopeView } from "./provider-contracts.ts";
import {
  cpaAliasRows,
  cpaPublicModelName,
  dynamicProviderAliasRows,
  mergeProviderAliasRows,
  providerAliasRows,
  aliasAccountCounts,
  aliasNameOverlaps,
} from "./provider-aliases.ts";

const protocol = {
  protocol: "chat_completions" as const,
  available: true,
  enabled: true,
  source: "static" as const,
  verified_at: null,
  observed_at: null,
  last_probe_result: null,
  last_probe_at: null,
  last_probe_error: null,
  override: "auto" as const,
};

const builtinScope = {
  key: "provider:go",
  scope_kind: "provider",
  scope_id: "opencode",
  provider_id: "opencode",
  label: "OpenCode Go",
  models: [{
    alias: "gpt-5.6",
    model_id: "gpt-5.6-upstream",
    preferred_protocol: "chat_completions",
    protocols: { chat_completions: protocol },
    routable: true,
    disabled_reasons: [],
  }, {
    alias: "",
    model_id: "raw-only-model",
    preferred_protocol: "chat_completions",
    protocols: { chat_completions: protocol },
    routable: true,
    disabled_reasons: [],
  }],
} as unknown as ProviderScopeView;

const customScope = {
  key: "custom_endpoint:custom-1",
  scope_kind: "custom_endpoint",
  scope_id: "custom-1",
  label: "Home Lab",
  accounts: [{ id: "custom-1", name: "Home Lab", enabled: true, verification_status: "verified" }],
  models: [{
    alias: "",
    model_id: "public-model",
    preferred_protocol: "chat_completions",
    protocols: { chat_completions: protocol },
    routable: false,
    disabled_reasons: [],
  }],
} as unknown as ProviderScopeView;

const customAccount = {
  id: "custom-1",
  name: "Home Lab",
  provider_id: "custom",
  enabled: true,
  plan_routable: true,
  model_capabilities: [{
    public_model: "public-model",
    upstream_model: "vendor/model:free",
    protocol: "chat_completions",
    verified_at: null,
    source: "discovered",
  }],
} as Account;

const goAccount = {
  ...customAccount,
  id: "go-1",
  provider_id: "opencode",
  model_capabilities: [],
} as Account;

const cpaAccount = {
  ...customAccount,
  id: "00000000-0000-0000-0000-000000000003",
  provider_id: "cpa",
  model_capabilities: [],
} as Account;

const dynamic = {
  id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  name: "Lab",
  endpoint_url: "http://127.0.0.1:9",
  upstream_protocol: "chat_completions" as const,
  auth_kind: "bearer" as const,
  models: [{ public_model: "lab-opus", upstream_model: "vendor/opus", upstream_override: null }],
  origin: "custom" as const,
  offering: "api" as const,
  editable: true,
  deletable: true,
  preset_id: null,
  created_at: "",
  updated_at: "",
  revision: 1,
  process_generation: 1,
};

test("Alias rows combine provider contracts with Custom public-to-upstream mappings", () => {
  assert.deepEqual(providerAliasRows([builtinScope, customScope], [goAccount, customAccount]), [
    {
      provider_id: "opencode",
      key: "provider:go:gpt-5.6:gpt-5.6-upstream",
      public_model: "gpt-5.6",
      provider_plan: "OpenCode Go",
      custom_account: null,
      upstream_model: "gpt-5.6-upstream",
      routable: true,
      custom_account_id: null,
    },
    {
      provider_id: "custom",
      key: "custom:custom-1:public-model:vendor/model:free",
      public_model: "public-model",
      provider_plan: "Home Lab",
      custom_account: "Home Lab",
      upstream_model: "vendor/model:free",
      routable: false,
      custom_account_id: "custom-1",
    },
  ]);
});

test("built-in Alias rows stay hidden until that provider has an enabled account", () => {
  assert.deepEqual(providerAliasRows([builtinScope], []), []);
  assert.deepEqual(providerAliasRows([builtinScope], [{ ...goAccount, enabled: false }]), []);
  assert.equal(providerAliasRows([builtinScope], [goAccount])[0]?.provider_id, "opencode");
});

test("disabled Custom accounts do not appear as Alias rows", () => {
  assert.deepEqual(providerAliasRows([customScope], [{ ...customAccount, enabled: false }]), []);
});

test("Alias account inventory separates missing and disabled accounts from model configuration", () => {
  const row = providerAliasRows([builtinScope], [goAccount])[0]!;
  assert.deepEqual(aliasAccountCounts(row, []), { total: 0, enabled: 0 });
  assert.deepEqual(aliasAccountCounts(row, [{ ...customAccount, provider_id: "opencode", enabled: false }, customAccount]), { total: 1, enabled: 0 });
  assert.deepEqual(aliasAccountCounts(row, [{ ...customAccount, provider_id: "opencode" }]), { total: 1, enabled: 1 });
  const custom = providerAliasRows([customScope], [customAccount])[0]!;
  assert.deepEqual(aliasAccountCounts(custom, [customAccount, { ...customAccount, id: "another" }]), { total: 1, enabled: 1 });
});

test("Alias overlap warning identifies another provider's raw ID without treating shared public aliases as conflicts", () => {
  const row = { ...providerAliasRows([builtinScope], [goAccount])[0]!, public_model: "audit-model" };
  const other = { ...row, provider_id: "dynamic", public_model: "audit-provider-model", upstream_model: "audit-model" };
  assert.equal(aliasNameOverlaps(row, [row, other]), true);
  assert.equal(aliasNameOverlaps(other, [row, other]), false);
  assert.equal(aliasNameOverlaps(row, [row, { ...other, public_model: "audit-model" }]), false);
});

test("Custom Alias routeability includes account readiness and built-in raw conflicts", () => {
  const scope = {
    ...customScope,
    models: [{
      ...customScope.models[0],
      alias: "",
      model_id: "raw-only-model",
      routable: true,
    }],
  } as ProviderScopeView;
  const account = {
    ...customAccount,
    setup_step: "ready",
    model_capabilities: [{
      ...customAccount.model_capabilities[0],
      public_model: "raw-only-model",
      upstream_model: "vendor/mapped:latest",
    }],
  } as Account;
  const row = providerAliasRows([builtinScope, scope], [account]).at(-1);
  assert.equal(row?.public_model, "raw-only-model");
  assert.equal(row?.upstream_model, "vendor/mapped:latest");
  assert.equal(row?.routable, false);
});

test("user-defined Provider mappings appear as Alias rows labelled by Provider name", () => {
  assert.deepEqual(dynamicProviderAliasRows([dynamic]), [{
    provider_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    key: "dynamic:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:lab-opus:vendor/opus",
    public_model: "lab-opus",
    provider_plan: "Lab",
    custom_account: null,
    upstream_model: "vendor/opus",
    routable: true,
    custom_account_id: null,
  }]);
});

test("production Alias merge keeps only enabled-account providers and selected CPA models", () => {
  const rows = mergeProviderAliasRows(
    [builtinScope, customScope],
    [goAccount, customAccount, cpaAccount, { ...customAccount, id: "dyn-1", provider_id: dynamic.id }],
    [dynamic],
    [
      { id: "gpt-5.6", enabled: true },
      { id: "grok-4", enabled: false },
      { id: "grok-3-mini", enabled: true },
    ],
  );
  assert.deepEqual(rows.map((row) => row.key), [
    "provider:go:gpt-5.6:gpt-5.6-upstream",
    "custom:custom-1:public-model:vendor/model:free",
    "dynamic:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:lab-opus:vendor/opus",
    "cpa:gpt-5.6",
    "cpa:grok-3-mini",
  ]);
  const cpaJoined = rows.find((row) => row.key === "cpa:gpt-5.6")!;
  assert.equal(cpaJoined.public_model, "gpt-5.6");
  assert.equal(cpaJoined.provider_plan, "CPA");
  assert.equal(cpaJoined.upstream_model, "gpt-5.6");
  assert.equal(cpaJoined.routable, true);
  const cpaRaw = rows.find((row) => row.key === "cpa:grok-3-mini")!;
  assert.equal(cpaRaw.public_model, "grok-3-mini");
  assert.equal("endpoint_url" in rows.at(-1)!, false);
  assert.equal("auth_kind" in rows.at(-1)!, false);
});

test("CPA catalog IDs join a code-owned Alias when one exists", () => {
  assert.equal(cpaPublicModelName([builtinScope], "gpt-5.6"), "gpt-5.6");
  assert.equal(cpaPublicModelName([builtinScope], "gpt-5.6-upstream"), "gpt-5.6");
  assert.equal(cpaPublicModelName([builtinScope], "grok-3-mini"), "grok-3-mini");
  assert.deepEqual(
    cpaAliasRows([{ id: "off", enabled: false }, { id: "on", enabled: true }], []),
    [{
      provider_id: "cpa",
      key: "cpa:on",
      public_model: "on",
      provider_plan: "CPA",
      custom_account: null,
      upstream_model: "on",
      routable: true,
      custom_account_id: null,
    }],
  );
});

test("CPA rows stay hidden when the subscription pool is disabled", () => {
  const rows = mergeProviderAliasRows(
    [builtinScope],
    [{ ...cpaAccount, enabled: false }],
    [],
    [{ id: "gpt-5.6", enabled: true }],
  );
  assert.deepEqual(rows, []);
});
