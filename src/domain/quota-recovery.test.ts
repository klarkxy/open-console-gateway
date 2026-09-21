import assert from "node:assert/strict";
import test from "node:test";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import {
  CARD_QUOTA_AVAILABILITY_KEYS,
  QUOTA_RECOVERY_REASON_KEYS,
  QUOTA_RECOVERY_STATUS_KEYS,
  QUOTA_RECOVERY_WINDOW_KEYS,
  cardQuotaAvailability,
  credentialHasActiveCooldown,
  credentialHasQuotaRecovery,
  credentialIsRouteAvailable,
  quotaRecoveryPresentation,
  quotaRetryRequestNeeded,
  withAccountEnablement,
  type QuotaRecovery,
  type RouteAvailableCredential,
} from "./quota-recovery.ts";

const NOW = Date.parse("2026-09-20T12:00:00Z");
const FUTURE = "2026-09-20T18:00:00Z";
const PAST = "2026-09-20T06:00:00Z";

function recovery(overrides: Partial<QuotaRecovery> = {}): QuotaRecovery {
  return {
    status: "waiting",
    reason: "quota_exhausted",
    window: "five_hours",
    observed_at: "2026-09-20T11:00:00Z",
    resets_at: "2026-09-20T16:00:00Z",
    next_retry_at: "2026-09-20T12:30:00Z",
    failure_count: 2,
    ...overrides,
  };
}

function dest(overrides: Partial<Destination> = {}): Destination {
  return {
    adapter: "http",
    legacy: { kind: "builtin", id: "lab" },
    auth_scheme: "bearer",
    base_url: null,
    brand_family: null,
    capabilities: {
      billing_tier_required: false,
      discoverable_models: false,
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
    id: "lab",
    max_credentials: null,
    name: "lab",
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

function idleCooldowns(): DestinationCredential["cooldowns"] {
  return {
    five_hour_until: null,
    free_until: null,
    generic_until: null,
    month_until: null,
    week_until: null,
  };
}

function key(overrides: Partial<RouteAvailableCredential> = {}): RouteAvailableCredential {
  return {
    enabled: true,
    auth_state: "unknown",
    quota_recovery: null,
    has_secret: true,
    onboarding_task: null,
    scope: { kind: "all" },
    cooldowns: idleCooldowns(),
    ...overrides,
  };
}

function usable(credential: RouteAvailableCredential, destination = dest()): boolean {
  return credentialIsRouteAvailable(credential, destination, NOW);
}

function card(
  membership: readonly RouteAvailableCredential[],
  destination = dest(),
) {
  return cardQuotaAvailability(membership, destination, NOW);
}

test("quota recovery KEY tables cover every presentation code", () => {
  assert.deepEqual(Object.keys(QUOTA_RECOVERY_STATUS_KEYS).sort(), ["probing", "ready", "waiting"]);
  assert.deepEqual(Object.keys(QUOTA_RECOVERY_REASON_KEYS).sort(), [
    "insufficient_balance",
    "quota_exhausted",
  ]);
  assert.deepEqual(Object.keys(QUOTA_RECOVERY_WINDOW_KEYS).sort(), ["five_hours", "month", "week"]);
  assert.deepEqual(Object.keys(CARD_QUOTA_AVAILABILITY_KEYS).sort(), [
    "no_available_keys",
    "no_keys",
    "quota_exhausted",
  ]);
});

test("no quotaRecovery means no confirmed exhaustion", () => {
  assert.equal(credentialHasQuotaRecovery(key()), false);
  assert.equal(credentialHasQuotaRecovery(key({ quota_recovery: undefined })), false);
  assert.equal(quotaRecoveryPresentation(null, NOW), null);
  assert.equal(quotaRecoveryPresentation(undefined, NOW), null);
  assert.equal(usable(key()), true);
});

test("any waiting/ready/probing recovery stays exhausted until the backend clears it", () => {
  for (const status of ["waiting", "ready", "probing"] as const) {
    const row = key({ quota_recovery: recovery({ status }) });
    assert.equal(credentialHasQuotaRecovery(row), true);
    assert.equal(usable(row), false);
    assert.equal(quotaRetryRequestNeeded(row.quota_recovery), status === "waiting");
  }
});

test("waiting presentation uses backend status and nextRetryAt only for the wait label", () => {
  const current = quotaRecoveryPresentation(recovery({
    status: "waiting",
    next_retry_at: "2026-09-20T12:30:00Z",
  }), NOW);
  assert.deepEqual(current, {
    kind: "waiting",
    reason: "quota_exhausted",
    window: "five_hours",
    wait: { unit: "minutes", minutes: 30 },
  });

  const overdue = quotaRecoveryPresentation(recovery({
    status: "waiting",
    next_retry_at: "2026-09-20T11:00:00Z",
  }), NOW);
  assert.equal(overdue?.kind, "waiting");
  if (overdue?.kind !== "waiting") return;
  assert.equal(overdue.wait, null);
});

test("overdue waiting does not become ready; ready/probing are backend status", () => {
  assert.deepEqual(
    quotaRecoveryPresentation(recovery({
      status: "ready",
      next_retry_at: "2026-09-20T11:00:00Z",
    }), NOW),
    { kind: "ready", reason: "quota_exhausted" },
  );
  assert.deepEqual(
    quotaRecoveryPresentation(recovery({
      status: "probing",
      next_retry_at: "2026-09-20T18:00:00Z",
    }), NOW),
    { kind: "probing", reason: "quota_exhausted" },
  );
});

test("recovery reason survives waiting, ready, and probing", () => {
  for (const status of ["waiting", "ready", "probing"] as const) {
    for (const reason of ["quota_exhausted", "insufficient_balance"] as const) {
      const presented = quotaRecoveryPresentation(recovery({ status, reason }), NOW);
      assert.equal(presented?.kind, status);
      assert.equal(presented?.reason, reason);
    }
  }
});

test("retry POSTs only while waiting; ready and probing need no extra request", () => {
  assert.equal(quotaRetryRequestNeeded(recovery({ status: "waiting" })), true);
  assert.equal(quotaRetryRequestNeeded(recovery({ status: "ready" })), false);
  assert.equal(quotaRetryRequestNeeded(recovery({ status: "probing" })), false);
  assert.equal(quotaRetryRequestNeeded(null), false);
});

test("disabled or invalid Keys are not route-available even without quotaRecovery", () => {
  assert.equal(usable(key({ enabled: false })), false);
  assert.equal(usable(key({ auth_state: "invalid" })), false);
  assert.equal(usable(key({
    enabled: false,
    quota_recovery: recovery(),
  })), false);
});

test("unknown auth is allowed; missing secret is required only when the destination authenticates", () => {
  assert.equal(usable(key({ auth_state: "unknown" })), true);
  assert.equal(usable(key({ has_secret: false })), false);
  assert.equal(usable(key({ has_secret: false }), dest({
    adapter: "zen",
    auth_scheme: "none",
    max_credentials: 1,
  })), true);
  const cpa = dest({
    adapter: "cpa",
    auth_scheme: "bearer",
    max_credentials: 1,
    capabilities: {
      ...dest().capabilities,
      external_integration: true,
      testable: false,
    },
  });
  assert.equal(usable(key({ has_secret: false }), cpa), true);
  assert.equal(card([key({ has_secret: false })], cpa), "available");
  assert.equal(card([key({ has_secret: false, enabled: false })], cpa), "no_available_keys");
});

test("onboarding in progress and an empty Only scope are not route-available", () => {
  assert.equal(usable(key({
    onboarding_task: { kind: "managed_registration", state: "in_progress", step: "payment" },
  })), false);
  assert.equal(usable(key({
    onboarding_task: { kind: "managed_registration", state: "completed", step: "ready" },
  })), true);
  assert.equal(usable(key({ scope: { kind: "only", models: [] } })), false);
  assert.equal(usable(key({ scope: { kind: "only", models: ["opus"] } })), true);
});

test("a disabled destination makes every Key unusable without inventing exhaustion", () => {
  const destination = dest({ enabled: false });
  assert.equal(usable(key(), destination), false);
  assert.equal(card([key()], destination), "no_available_keys");
});

test("empty card is no_keys, not exhaustion", () => {
  assert.equal(card([]), "no_keys");
});

test("one confirmed exhausted Key does not exhaust a sibling on the same card", () => {
  assert.equal(card([
    key({ quota_recovery: recovery() }),
    key(),
  ]), "available");
});

test("all Keys confirmed exhausted is quota_exhausted, including disabled exhausted rows", () => {
  assert.equal(card([
    key({ quota_recovery: recovery({ status: "waiting" }) }),
    key({ enabled: false, quota_recovery: recovery({ status: "ready" }) }),
  ]), "quota_exhausted");
});

test("mixed disabled, invalid, and exhausted without a usable Key is no_available_keys", () => {
  assert.equal(card([
    key({ enabled: false }),
    key({ auth_state: "invalid" }),
    key({ quota_recovery: recovery() }),
  ]), "no_available_keys");
});

test("all disabled or invalid without quotaRecovery is no_available_keys, not exhaustion", () => {
  assert.equal(card([
    key({ enabled: false }),
    key({ auth_state: "invalid" }),
  ]), "no_available_keys");
});

test("separate cards of the same destination stay independent", () => {
  const destination = dest();
  const exhaustedCard = [key({ quota_recovery: recovery() })];
  const healthyCard = [key()];
  assert.equal(card(exhaustedCard, destination), "quota_exhausted");
  assert.equal(card(healthyCard, destination), "available");
});

test("card availability uses full saved membership, not the filtered visible rows", () => {
  const exhausted = key({ quota_recovery: recovery() });
  const healthy = key();
  const membership = [exhausted, healthy];
  const visible = [exhausted];
  assert.equal(card(visible), "quota_exhausted");
  assert.equal(card(membership), "available");
});

test("all saved Keys exhausted stays quota_exhausted when a subset is visible", () => {
  const waiting = key({ quota_recovery: recovery({ status: "waiting" }) });
  const ready = key({ quota_recovery: recovery({ status: "ready" }) });
  assert.equal(card([waiting, ready]), "quota_exhausted");
  assert.equal(card([waiting]), "quota_exhausted");
});

test("quota recovery detection ignores ordinary cooldown; route aggregation does not", () => {
  const cooling = key({
    cooldowns: {
      ...idleCooldowns(),
      five_hour_until: FUTURE,
      free_until: FUTURE,
    },
  });
  assert.equal(credentialHasQuotaRecovery(cooling), false);
  assert.equal(quotaRetryRequestNeeded(cooling.quota_recovery), false);
  assert.equal(credentialHasActiveCooldown(cooling, dest(), NOW), true);
  assert.equal(usable(cooling), false);
  assert.equal(card([cooling]), "no_available_keys");
});

test("expired cooldown restores route availability without becoming quota recovery", () => {
  const cooled = key({
    cooldowns: { ...idleCooldowns(), five_hour_until: PAST, generic_until: PAST },
  });
  assert.equal(credentialHasActiveCooldown(cooled, dest(), NOW), false);
  assert.equal(usable(cooled), true);
  assert.equal(card([cooled]), "available");
});

test("Zen only consults the free cooldown channel", () => {
  const zen = dest({ adapter: "zen", auth_scheme: "none", max_credentials: 1 });
  const fiveHourOnly = key({
    has_secret: false,
    cooldowns: { ...idleCooldowns(), five_hour_until: FUTURE },
  });
  const freeCooling = key({
    has_secret: false,
    cooldowns: { ...idleCooldowns(), free_until: FUTURE },
  });
  assert.equal(credentialHasActiveCooldown(fiveHourOnly, zen, NOW), false);
  assert.equal(usable(fiveHourOnly, zen), true);
  assert.equal(credentialHasActiveCooldown(freeCooling, zen, NOW), true);
  assert.equal(usable(freeCooling, zen), false);
  assert.equal(card([freeCooling], zen), "no_available_keys");
});

test("Go ignores a free-only cooldown", () => {
  const go = dest({ adapter: "opencode_go" });
  const freeOnly = key({
    cooldowns: { ...idleCooldowns(), free_until: FUTURE },
  });
  assert.equal(credentialHasActiveCooldown(freeOnly, go, NOW), false);
  assert.equal(usable(freeOnly, go), true);
  assert.equal(card([freeOnly], go), "available");
});

test("Zen generic cooldown makes the Key unavailable", () => {
  const zen = dest({ adapter: "zen", auth_scheme: "none", max_credentials: 1 });
  const genericOnly = key({
    has_secret: false,
    cooldowns: { ...idleCooldowns(), generic_until: FUTURE },
  });
  assert.equal(credentialHasActiveCooldown(genericOnly, zen, NOW), true);
  assert.equal(usable(genericOnly, zen), false);
  assert.equal(card([genericOnly], zen), "no_available_keys");
  assert.equal(credentialHasQuotaRecovery(genericOnly), false);
  assert.equal(quotaRetryRequestNeeded(genericOnly.quota_recovery), false);
});

test("cooling mixed with exhaustion is no_available_keys, not quota_exhausted", () => {
  assert.equal(card([
    key({ cooldowns: { ...idleCooldowns(), generic_until: FUTURE } }),
    key({ quota_recovery: recovery() }),
  ]), "no_available_keys");
});

test("account switch overlay updates enablement without waiting for the destination snapshot", () => {
  const staleOff = key({ enabled: false });
  const presentedOn = withAccountEnablement(staleOff, true);
  assert.equal(staleOff.enabled, false);
  assert.equal(presentedOn.enabled, true);
  assert.equal(usable(staleOff), false);
  assert.equal(usable(presentedOn), true);
  assert.equal(card([staleOff, key()]), "available");
  assert.equal(card([withAccountEnablement(key(), false)]), "no_available_keys");
  assert.equal(withAccountEnablement(key({ enabled: false }), null).enabled, false);
});

test("purchase date is not a routing-availability input", () => {
  const row = {
    ...key(),
    purchase_date: "2020-01-01",
  } satisfies RouteAvailableCredential & Pick<DestinationCredential, "purchase_date">;
  assert.equal(usable(row), true);
  assert.equal(card([row]), "available");
});
