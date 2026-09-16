import assert from "node:assert/strict";
import test from "node:test";
import type { Account, AccountSetupStep } from "../api/dashboard.ts";
import {
  ACCOUNT_MENU_LABEL_KEYS,
  MANAGED_STEP_LABEL_KEYS,
  ROUTING_DRAFT_DESCRIPTION_KEYS,
  ROUTING_DRAFT_LABEL_KEYS,
  accountExpiry,
  accountMenuOptions,
  accountRoutingDraftState,
  accountStatus,
  accountStatusTagType,
  cooldownDetails,
  cooldownRemainingUntil,
  usageSyncStatus,
} from "./account-display.ts";
import { localDateString } from "./account-lifecycle.ts";

function draftAccount(overrides: Partial<Account> = {}): Account {
  return {
    id: "draft",
    name: "Draft",
    username: "",
    password: "",
    key: "key",
    enabled: false,
    account_type: "key",
    setup_step: "ready",
    provider_id: "custom",
    credential_kind: "api_key",
    quota_scope: "key",
    purchase_date: "2026-08-21",
    expires_on: "2026-09-21",
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
    created_at: "2026-08-21T00:00:00Z",
    updated_at: "2026-08-21T00:00:00Z",
    verification_status: "pending",
    connection_verified_at: null,
    verification_error: null,
    plan_routable: false,
    model_capabilities: [],
    ...overrides,
  };
}

test("missing expiry date is unset rather than zero days overdue", () => {
  assert.deepEqual(accountExpiry(draftAccount({ expires_on: "" })), { kind: "unset" });
});

test("expiry state carries raw day counts for remaining, today, and overdue", () => {
  const now = Date.parse("2026-09-01T12:00:00Z");
  const offsetLocalDate = (days: number) => {
    const date = new Date(now);
    date.setDate(date.getDate() + days);
    return localDateString(date);
  };
  assert.deepEqual(
    accountExpiry(draftAccount({ expires_on: offsetLocalDate(3) }), now),
    { kind: "remaining", days: 3 },
  );
  assert.deepEqual(
    accountExpiry(draftAccount({ expires_on: offsetLocalDate(1) }), now),
    { kind: "remaining", days: 1 },
  );
  assert.deepEqual(
    accountExpiry(draftAccount({ expires_on: offsetLocalDate(0) }), now),
    { kind: "today" },
  );
  assert.deepEqual(
    accountExpiry(draftAccount({ expires_on: offsetLocalDate(-4) }), now),
    { kind: "expired", days: 4 },
  );
});

test("unroutable account drafts use verification status rather than provider-specific branches", () => {
  const base = { setup_step: "ready" as const, plan_routable: false };
  assert.equal(
    accountRoutingDraftState({ ...base, verification_status: "pending" }),
    "pending",
  );
  assert.equal(
    accountRoutingDraftState({ ...base, verification_status: "failed" }),
    "failed",
  );
  assert.equal(
    accountRoutingDraftState({ ...base, verification_status: "not_required" }),
    "unsupported",
  );
  assert.equal(
    accountRoutingDraftState({ ...base, plan_routable: true, verification_status: "verified" }),
    null,
  );
});

test("routing draft maps cover every state with distinct keys", () => {
  const states = ["pending", "failed", "unsupported"];
  assert.deepEqual(Object.keys(ROUTING_DRAFT_LABEL_KEYS).sort(), [...states].sort());
  assert.deepEqual(Object.keys(ROUTING_DRAFT_DESCRIPTION_KEYS).sort(), [...states].sort());
  assert.equal(new Set(Object.values(ROUTING_DRAFT_LABEL_KEYS)).size, states.length);
  assert.equal(new Set(Object.values(ROUTING_DRAFT_DESCRIPTION_KEYS)).size, states.length);
});

test("draft status replaces disabled instead of rendering a second competing state", () => {
  const pending = draftAccount();
  assert.deepEqual(accountStatus(pending), { kind: "draft", state: "pending" });
  assert.equal(accountStatusTagType(pending), "warning");

  const unsupported = draftAccount({ verification_status: "not_required" });
  assert.deepEqual(accountStatus(unsupported), { kind: "draft", state: "unsupported" });
  assert.equal(accountStatusTagType(unsupported), "warning");

  const failed = draftAccount({ verification_status: "failed" });
  assert.deepEqual(accountStatus(failed), { kind: "draft", state: "failed" });
  assert.equal(accountStatusTagType(failed), "error");

  const ordinaryDisabled = draftAccount({ plan_routable: true, verification_status: "verified" });
  assert.deepEqual(accountStatus(ordinaryDisabled), { kind: "disabled" });
  assert.equal(accountStatusTagType(ordinaryDisabled), "error");
});

test("cooling status carries the structured remaining time", () => {
  const now = Date.parse("2026-09-01T00:00:00Z");
  const cooling = draftAccount({
    plan_routable: true,
    verification_status: "verified",
    enabled: true,
    cooldown_until: new Date(now + 90_000).toISOString(),
  });
  assert.deepEqual(accountStatus(cooling, now), {
    kind: "cooling",
    remaining: { unit: "minutes", minutes: 1 },
  });
  assert.equal(accountStatusTagType(cooling, now), "warning");
});

test("cooldown remaining buckets by magnitude", () => {
  const now = Date.parse("2026-09-01T00:00:00Z");
  const at = (ms: number) => new Date(now + ms).toISOString();
  assert.equal(cooldownRemainingUntil(null, now), null);
  assert.deepEqual(cooldownRemainingUntil(at(-1_000), now), { unit: "seconds", seconds: 0 });
  assert.deepEqual(cooldownRemainingUntil(at(30_000), now), { unit: "seconds", seconds: 30 });
  assert.deepEqual(cooldownRemainingUntil(at(90_000), now), { unit: "minutes", minutes: 1 });
  assert.deepEqual(
    cooldownRemainingUntil(at(90 * 60_000), now),
    { unit: "hours-minutes", hours: 1, minutes: 30 },
  );
  assert.deepEqual(
    cooldownRemainingUntil(at(50 * 3_600_000), now),
    { unit: "days-hours", days: 2, hours: 2 },
  );
});

test("cooldown details order generic, windows, free, and fall back to generic", () => {
  const now = Date.parse("2026-09-01T00:00:00Z");
  const at = (ms: number) => new Date(now + ms).toISOString();
  const limits = [
    { key: "window_5h" as const, label: "L5" },
    { key: "window_week" as const, label: "LW" },
  ];
  const both = draftAccount({
    cooldown_generic_until: at(60_000),
    cooldown_5h_until: at(120_000),
    cooldown_free_until: at(180_000),
  });
  assert.deepEqual(cooldownDetails(both, now, limits), [
    { kind: "generic" },
    { kind: "window", label: "L5" },
    { kind: "free" },
  ]);
  const windowOnly = draftAccount({ cooldown_week_until: at(120_000) });
  assert.deepEqual(cooldownDetails(windowOnly, now, limits), [
    { kind: "window", label: "LW" },
  ]);
  assert.deepEqual(cooldownDetails(draftAccount(), now, limits), [{ kind: "generic" }]);
});

test("managed step label keys cover every setup step with distinct copy", () => {
  const steps: AccountSetupStep[] = [
    "google_account",
    "opencode_registration",
    "payment",
    "key_verification",
    "ready",
  ];
  assert.deepEqual(Object.keys(MANAGED_STEP_LABEL_KEYS).sort(), [...steps].sort());
  assert.equal(new Set(Object.values(MANAGED_STEP_LABEL_KEYS)).size, steps.length);
});

test("CPA accounts expose only the jump to their external-integration page", () => {
  const cpa = draftAccount({
    id: "00000000-0000-0000-0000-000000000003",
    provider_id: "cpa",
    plan_routable: true,
    verification_status: "verified",
  });
  assert.deepEqual(
    accountMenuOptions(cpa).map(({ key }) => key),
    ["open-cpa"],
  );
});

test("Ollama cards drop OpenCode-only console and profile actions but keep generic lifecycle", () => {
  const now = Date.now();
  const ollama = draftAccount({
    id: "ollama-1",
    name: "s",
    provider_id: "ollama",
    enabled: true,
    plan_routable: true,
    verification_status: "not_required",
  });
  const keys = accountMenuOptions(ollama, now).map((option) => option.key);
  assert.deepEqual(keys, ["open-site", "edit", "delete"]);
  assert.ok(!keys.includes("open-console"));
  assert.ok(!keys.includes("reset-profile"));
  assert.ok(!keys.includes("continue-setup"));

  const cooling = draftAccount({
    id: "ollama-1",
    name: "s",
    provider_id: "ollama",
    enabled: true,
    plan_routable: true,
    verification_status: "not_required",
    cooldown_until: new Date(now + 60_000).toISOString(),
  });
  assert.deepEqual(
    accountMenuOptions(cooling, now).map((option) => option.key),
    ["open-site", "edit", "reset", "delete"],
  );
});

test("every emitted menu option is label-free in the domain and has a label key", () => {
  const now = Date.now();
  const scenarios = [
    draftAccount({
      id: "00000000-0000-0000-0000-000000000003",
      provider_id: "cpa",
      plan_routable: true,
      verification_status: "verified",
    }),
    draftAccount({ id: "custom-1", plan_routable: true }),
    draftAccount({
      id: "ollama-1",
      provider_id: "ollama",
      enabled: true,
      plan_routable: true,
      verification_status: "not_required",
      cooldown_until: new Date(now + 60_000).toISOString(),
    }),
    draftAccount({ id: "go-1", provider_id: "opencode", setup_step: "google_account" }),
    draftAccount({
      id: "go-2",
      provider_id: "opencode",
      enabled: true,
      plan_routable: true,
      verification_status: "verified",
      cooldown_until: new Date(now + 60_000).toISOString(),
    }),
  ];
  const emittedKeys = new Set(
    scenarios.flatMap((account) => accountMenuOptions(account, now).map((option) => option.key)),
  );
  for (const account of scenarios) {
    for (const option of accountMenuOptions(account, now)) {
      assert.equal(option.label, undefined);
    }
  }
  for (const key of emittedKeys) {
    assert.ok(
      Object.prototype.hasOwnProperty.call(ACCOUNT_MENU_LABEL_KEYS, String(key)),
      `missing label key for menu option ${String(key)}`,
    );
  }
  const labelValues = Object.values(ACCOUNT_MENU_LABEL_KEYS);
  assert.equal(new Set(labelValues).size, labelValues.length);
});

test("routable Custom accounts follow live enablement rather than legacy verification state", () => {
  const custom = (overrides: Partial<Account> = {}) => draftAccount({
    id: "custom-1",
    name: "Custom",
    purchase_date: "",
    expires_on: "",
    plan_routable: true,
    ...overrides,
  });

  const pending = custom();
  assert.deepEqual(accountStatus(pending), { kind: "disabled" });
  assert.equal(accountStatusTagType(pending), "error");
  assert.deepEqual(accountStatus(custom({ verification_status: "failed" })), { kind: "disabled" });
  assert.deepEqual(accountStatus(custom({ verification_status: "verified" })), { kind: "disabled" });
  assert.deepEqual(accountMenuOptions(custom(), Date.now()).map(({ key }) => key), ["edit", "delete"]);
});

test("GOAT account states are live without a verification phase", () => {
  const goat = (overrides: Partial<Account> = {}) => draftAccount({
    id: "goat-1",
    name: "GOAT",
    provider_id: "command-code",
    plan_routable: true,
    verification_status: "not_required",
    ...overrides,
  });

  assert.deepEqual(accountStatus(goat()), { kind: "disabled" });
  assert.equal(accountStatusTagType(goat()), "error");
  // Ready + enabled is the routing-on configuration state, not an availability claim.
  assert.deepEqual(accountStatus(goat({ enabled: true })), { kind: "enabled" });
  assert.equal(accountStatusTagType(goat({ enabled: true })), "success");
  assert.deepEqual(accountStatus(goat({ plan_routable: false })), { kind: "draft", state: "unsupported" });
});

test("upstream auth failure is a distinct unavailable state, not cooldown", () => {
  const broken = draftAccount({
    plan_routable: true,
    verification_status: "verified",
    enabled: true,
    auth_error: "401",
  });
  assert.deepEqual(accountStatus(broken), { kind: "unavailable" });
  assert.equal(accountStatusTagType(broken), "error");
  assert.deepEqual(accountStatus({ ...broken, enabled: false }), { kind: "disabled-unavailable" });
});

test("usage sync status reports last success or never-synced, never a refresh cooldown", () => {
  const neverSynced = draftAccount({
    plan_routable: true,
    verification_status: "verified",
  });
  assert.deepEqual(usageSyncStatus(neverSynced), { kind: "never" });

  const syncedAt = "2026-08-20T12:00:00Z";
  const cooling = draftAccount({
    plan_routable: true,
    verification_status: "verified",
    usage_sync_last_success_at: syncedAt,
    usage_sync_next_allowed_at: "2026-08-21T00:01:00Z",
  });
  assert.deepEqual(usageSyncStatus(cooling), {
    kind: "synced",
    time: new Date(Date.parse(syncedAt)).toLocaleString(),
  });
});

test("unparseable sync timestamps pass through as raw data", () => {
  const broken = draftAccount({ usage_sync_last_success_at: "not-a-date" });
  assert.deepEqual(usageSyncStatus(broken), { kind: "synced", time: "not-a-date" });
});
