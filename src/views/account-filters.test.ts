import type { AccountCapabilitySource } from "../domain/account-capabilities.ts";
import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import { buildNeedsAttention } from "./dashboard-attention.ts";
import { accountPlanKey, accountStatusKey, filterAccounts, plansInUse } from "./account-filters.ts";
import { providerSurfaces } from "../domain/plans.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";

const NOW = Date.parse("2026-08-21T12:00:00Z");

function account(overrides: Partial<Account>): Account {
  return {
    id: "acc-1",
    name: "Account",
    username: "",
    password: "",
    key: "key",
    enabled: true,
    account_type: "key",
    setup_step: "ready",
    provider_id: "opencode",
    credential_kind: "api_key",
    quota_scope: "key",
    purchase_date: "2026-08-01",
    expires_on: "2026-09-01",
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
    verification_status: "not_required",
    connection_verified_at: null,
    verification_error: null,
    plan_routable: true,
    custom_config: null,
    model_capabilities: [],
    created_at: "2026-08-01T00:00:00Z",
    updated_at: "2026-08-01T00:00:00Z",
    ...overrides,
  };
}

test("needs attention orders auth errors, expiry, cooling, drafts", () => {
  const accounts = [
    account({ id: "ok", name: "Fine" }),
    account({ id: "auth", name: "Broken", auth_error: "401" }),
    account({ id: "cool", name: "Cooling", cooldown_until: "2026-08-21T13:00:00Z" }),
    account({ id: "draft", name: "Draft", setup_step: "key_verification" }),
    account({ id: "gone", name: "Expired", expires_on: "2026-08-01" }),
    account({ id: "off", name: "Disabled", enabled: false }),
  ];
  const items = buildNeedsAttention(accounts, NOW);
  assert.deepEqual(
    items.map((item) => [item.accountId, item.reason]),
    [
      ["auth", "auth-error"],
      ["gone", "expired"],
      ["cool", "cooling"],
      ["draft", "setup-incomplete"],
    ],
  );
});

test("disabled accounts and expired cooldowns never need attention", () => {
  const accounts = [
    account({ id: "off", name: "Off", enabled: false, cooldown_until: "2026-08-21T13:00:00Z" }),
    account({ id: "past", name: "Past", cooldown_until: "2026-08-21T11:00:00Z" }),
    account({ id: "offdraft", name: "OffDraft", enabled: false, setup_step: "payment" }),
  ];
  const items = buildNeedsAttention(accounts, NOW);
  assert.deepEqual(items.map((item) => item.accountId), ["offdraft"]);
});

test("Custom API ignores legacy lifecycle dates", () => {
  const custom = account({
    id: "custom",
    name: "Custom",
    provider_id: "custom",

    purchase_date: "2026-07-01",
    expires_on: "2026-08-01",
  });
  assert.deepEqual(buildNeedsAttention([custom], NOW), []);
});

test("user-defined (dynamic) Provider accounts never raise expiry attention", () => {
  // No built-in plan owns this provider id and no billing cadence is modeled
  // for user-defined Providers; even a synthetic stored date is ignored.
  const dynamic = account({
    id: "dyn",
    name: "Lab",
    provider_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    purchase_date: "2026-07-01",
    expires_on: "2026-08-01",
  });
  assert.deepEqual(buildNeedsAttention([dynamic], NOW), []);
});

test("zen free cooling is reported through the shared free lane", () => {
  const zen = account({
    id: "zen",
    name: "Zen",
    provider_id: "opencode-zen-free",

    credential_kind: "none",
    quota_scope: "egress-ip",
    expires_on: "2026-08-01",
    cooldown_free_until: "2026-08-21T13:00:00Z",
  });
  assert.deepEqual(buildNeedsAttention([zen], NOW), [
    { accountId: "zen", accountName: "Zen", reason: "cooling" },
  ]);
});

test("status buckets mirror the card status labels", () => {
  assert.equal(accountStatusKey(account({}), NOW), "available");
  assert.equal(accountStatusKey(account({ enabled: false }), NOW), "disabled");
  assert.equal(accountStatusKey(account({ auth_error: "401" }), NOW), "auth-error");
  assert.equal(accountStatusKey(account({ setup_step: "payment" }), NOW), "registering");
  for (const [plan_routable, verification_status] of [
    [true, "pending"],
    [false, "failed"],
    [true, "failed"],
    [false, "pending"],
  ] as const) {
    assert.equal(
      accountStatusKey(account({
        enabled: false,
        provider_id: "custom",
        plan_routable,
        verification_status,
      }), NOW),
      "disabled",
      `${plan_routable}/${verification_status}`,
    );
  }
  assert.equal(
    accountStatusKey(account({ cooldown_until: "2026-08-21T13:00:00Z" }), NOW),
    "cooling",
  );
  assert.equal(
    accountStatusKey(account({
      provider_id: "opencode-zen-free",
      cooldown_free_until: "2026-08-21T13:00:00Z",
    }), NOW),
    "cooling",
  );
});

test("plan and status filters keep the existing priority order", () => {
  const accounts = [
    account({ id: "a", name: "A" }),
    account({
      id: "b",
      name: "B",
      provider_id: "opencode-zen-free",
    }),
    account({ id: "c", name: "C", auth_error: "401" }),
  ];
  assert.deepEqual(filterAccounts(accounts, "all", "all", NOW).map((a) => a.id), ["a", "b", "c"]);
  assert.deepEqual(filterAccounts(accounts, "opencode-zen-free", "all", NOW).map((a) => a.id), ["b"]);
  assert.deepEqual(filterAccounts(accounts, "all", "auth-error", NOW).map((a) => a.id), ["c"]);
  assert.deepEqual(filterAccounts(accounts, "opencode", "available", NOW).map((a) => a.id), ["a"]);
  // Unknown providers fall back to the raw provider id.
  assert.equal(
    accountPlanKey(account({ provider_id: "else" })),
    "else",
  );
  const dynamicId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  assert.equal(accountPlanKey(account({ provider_id: dynamicId })), dynamicId);
  assert.deepEqual(
    filterAccounts(
      [account({ id: "dyn", provider_id: dynamicId }), account({ id: "go" })],
      dynamicId,
      "all",
      NOW,
    ).map((row) => row.id),
    ["dyn"],
  );
});

test("plansInUse follows catalog projection order, not account order", () => {
  const accounts = [
    account({ id: "b", name: "B", provider_id: "opencode-zen-free" }),
    account({ id: "a", name: "A" }),
  ];
  assert.deepEqual(
    plansInUse(accounts, providerSurfaces(null)).map((plan) => plan.id),
    ["opencode", "opencode-zen-free"],
  );
});

function catalogRow(provider_id: string, extra: Partial<ProviderCatalogEntry> = {}): ProviderCatalogEntry {
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
    pricing_availability: "available",
    usage_availability: "available",
    manual_usage_calibration: false,
    quota_unit: "tokens",
    model_source: "invented_catalog",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [{ id: "key", kind: "secret", required: true, immutable_after_create: false }],
    model_aliases: [],
    ...extra,
  };
}

test("expired built-in billed accounts raise attention only from catalog facts", () => {
  // The catalog is the authority on built-in billed families; with its rows
  // present, GOAT, MiniMax, Kimi, and Ollama expiry surfaces.
  const catalog = [
    catalogRow("command-code"),
    catalogRow("minimax"),
    catalogRow("kimi"),
    catalogRow("ollama"),
  ];
  const expired = { purchase_date: "2026-07-01", expires_on: "2026-08-01" };
  const accounts = [
    account({ id: "goat", name: "GOAT", provider_id: "command-code", ...expired }),
    account({ id: "kimi", name: "Kimi", provider_id: "kimi", ...expired }),
    account({ id: "mm", name: "MiniMax", provider_id: "minimax", ...expired }),
    account({ id: "ol", name: "Ollama", provider_id: "ollama", ...expired }),
  ];
  assert.deepEqual(
    buildNeedsAttention(accounts, NOW, catalog).map((item) => [item.accountId, item.reason]),
    [
      ["goat", "expired"],
      ["kimi", "expired"],
      ["mm", "expired"],
      ["ol", "expired"],
    ],
  );
  // A successful but empty catalog is authoritative: nothing is invented.
  assert.deepEqual(buildNeedsAttention(accounts, NOW, []), []);
});

test("null catalog keeps only the narrow offline Go/Zen fallback", () => {
  const expired = { purchase_date: "2026-07-01", expires_on: "2026-08-01" };
  const accounts = [
    account({ id: "go", name: "Go", provider_id: "opencode", ...expired }),
    account({ id: "goat", name: "GOAT", provider_id: "command-code", ...expired }),
  ];
  assert.deepEqual(
    buildNeedsAttention(accounts, NOW, null).map((item) => item.accountId),
    ["go"],
  );
});

test("custom, dynamic, CPA, and Zen accounts stay excluded with catalog facts", () => {
  const catalog = [
    catalogRow("custom"),
    catalogRow("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", {
      origin: "custom",
      model_source: "dynamic_provider",
    }),
    catalogRow("opencode-zen-free", {
      credential_kind: "none",
      quota_scope: "egress-ip",
      usage_availability: "unavailable",
    }),
  ];
  const expired = { purchase_date: "2026-07-01", expires_on: "2026-08-01" };
  const accounts = [
    account({ id: "custom", name: "Custom", provider_id: "custom", ...expired }),
    account({ id: "dyn", name: "Lab", provider_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", ...expired }),
    account({ id: "cpa", name: "CPA", provider_id: "cpa", ...expired }),
    account({ id: "zen", name: "Zen", provider_id: "opencode-zen-free", ...expired }),
  ];
  assert.deepEqual(buildNeedsAttention(accounts, NOW, catalog), []);
});

test("loaded destination cadence and cooldown facts drive attention and filters", () => {
  const destination: AccountCapabilitySource = {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    auth_scheme: "bearer", max_credentials: null,
    capabilities: { testable: true, managed_signup: false, external_integration: false, billing_tier_required: false },
    plan: { expiry_cadence: "monthly", manual_calibration: false, pricing_source: "unpriced", usage_source: "none", windows: [{ kind: "month" }] },
  };
  const row = account({ id: "new", provider_id: "unknown", purchase_date: "2026-07-01", expires_on: "2026-08-01" });
  assert.deepEqual(buildNeedsAttention([row], NOW, [], () => destination).map((item) => item.reason), ["expired"]);
  const noCadence = { ...destination, plan: null };
  assert.deepEqual(buildNeedsAttention([row], NOW, null, () => noCadence), []);
  const freeOnly = { ...destination, plan: { ...destination.plan!, expiry_cadence: null, windows: [{ kind: "free" as const }] } };
  const cooling = account({ provider_id: "unknown", cooldown_free_until: new Date(NOW + 60_000).toISOString() });
  assert.equal(accountStatusKey(cooling, NOW, [], freeOnly), "cooling");
  assert.equal(filterAccounts([cooling], "all", "cooling", NOW, [], () => freeOnly).length, 1);
});
