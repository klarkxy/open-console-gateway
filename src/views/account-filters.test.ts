import type { AccountCapabilitySource } from "../domain/account-capabilities.ts";
import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import { accountPlanKey, accountStatusKey, filterAccounts, plansInUse } from "./account-filters.ts";
import { providerSurfaces } from "../domain/plans.ts";

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

test("loaded destination cooldown facts drive account filters", () => {
  const destination: AccountCapabilitySource = {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    auth_scheme: "bearer", max_credentials: null,
    capabilities: { testable: true, managed_signup: false, external_integration: false, billing_tier_required: false },
    plan: { expiry_cadence: "monthly", manual_calibration: false, usage_source: "none", windows: [{ kind: "month" }] },
  };
  const freeOnly = { ...destination, plan: { ...destination.plan!, expiry_cadence: null, windows: [{ kind: "free" as const }] } };
  const cooling = account({ provider_id: "unknown", cooldown_free_until: new Date(NOW + 60_000).toISOString() });
  assert.equal(accountStatusKey(cooling, NOW, [], freeOnly), "cooling");
  assert.equal(filterAccounts([cooling], "all", "cooling", NOW, [], () => freeOnly).length, 1);
});
