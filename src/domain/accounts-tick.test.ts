import assert from "node:assert/strict";
import test from "node:test";
import type { QuotaRecovery } from "../api/destinations.ts";
import {
  ACCOUNTS_TICK_IDLE_MS,
  ACCOUNTS_TICK_MAX_MS,
  ACCOUNTS_TICK_MIN_MS,
  accountsNextDeadline,
  accountsTickDelay,
  type TickAccount,
  type TickCredential,
  type TickQuotaWindow,
} from "./accounts-tick.ts";

const NOW = Date.parse("2026-09-20T12:00:00Z");
const SOON = "2026-09-20T12:00:10Z";
const LATER = "2026-09-20T12:00:30Z";
const PAST = "2026-09-20T11:59:50Z";

function account(overrides: Partial<TickAccount> = {}): TickAccount {
  return {
    cooldown_until: null,
    cooldown_generic_until: null,
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: null,
    ...overrides,
  };
}

function credential(overrides: Partial<TickCredential> = {}): TickCredential {
  return {
    cooldowns: {
      five_hour_until: null,
      free_until: null,
      generic_until: null,
      month_until: null,
      week_until: null,
    },
    quota_recovery: null,
    ...overrides,
  };
}

function recovery(overrides: Partial<QuotaRecovery> = {}): QuotaRecovery {
  return {
    status: "waiting",
    reason: "quota_exhausted",
    window: "five_hours",
    observed_at: "2026-09-20T11:00:00Z",
    resets_at: null,
    next_retry_at: SOON,
    failure_count: 1,
    ...overrides,
  };
}

function deadline(input: {
  accounts?: readonly TickAccount[];
  credentials?: readonly TickCredential[];
  quotaWindows?: readonly TickQuotaWindow[];
  now?: number;
}): number | null {
  return accountsNextDeadline({
    accounts: input.accounts ?? [],
    credentials: input.credentials ?? [],
    quotaWindows: input.quotaWindows ?? [],
    now: input.now ?? NOW,
  });
}

test("no accounts, credentials, or quota windows means no deadline", () => {
  assert.equal(deadline({}), null);
});

test("null and past timestamps are ignored", () => {
  assert.equal(deadline({
    accounts: [account({
      cooldown_until: PAST,
      cooldown_5h_until: "not-a-date",
      cooldown_week_until: null,
    })],
    credentials: [credential({
      cooldowns: {
        five_hour_until: PAST,
        free_until: null,
        generic_until: "also-not-a-date",
        month_until: null,
        week_until: null,
      },
    })],
    quotaWindows: [{ resets_at: PAST }, { resets_at: null }],
  }), null);
});

test("a single future account cooldown is the deadline", () => {
  assert.equal(deadline({
    accounts: [account({ cooldown_week_until: SOON })],
  }), Date.parse(SOON));
});

test("the nearest future timestamp wins across every source", () => {
  assert.equal(deadline({
    accounts: [account({ cooldown_until: LATER, cooldown_free_until: PAST })],
    credentials: [credential({ cooldowns: {
      five_hour_until: null,
      free_until: null,
      generic_until: SOON,
      month_until: null,
      week_until: null,
    } })],
    quotaWindows: [{ resets_at: "2026-09-20T12:00:20Z" }],
  }), Date.parse(SOON));
});

test("an already-past candidate does not mask a later future one", () => {
  assert.equal(deadline({
    accounts: [account({ cooldown_until: PAST })],
    credentials: [credential({ cooldowns: {
      five_hour_until: null,
      free_until: null,
      generic_until: null,
      month_until: null,
      week_until: LATER,
    } })],
  }), Date.parse(LATER));
});

test("a waiting quota recovery contributes next_retry_at", () => {
  assert.equal(deadline({
    credentials: [credential({ quota_recovery: recovery() })],
  }), Date.parse(SOON));
});

test("ready or probing quota recovery has no retry deadline", () => {
  assert.equal(deadline({
    credentials: [
      credential({ quota_recovery: recovery({ status: "ready", next_retry_at: SOON }) }),
      credential({ quota_recovery: recovery({ status: "probing", next_retry_at: SOON }) }),
    ],
  }), null);
});

test("a future quota window reset is a deadline", () => {
  assert.equal(deadline({
    quotaWindows: [{ resets_at: LATER }],
  }), Date.parse(LATER));
});

test("tick delay idles at the coarse fallback without a deadline", () => {
  assert.equal(accountsTickDelay(null, NOW), ACCOUNTS_TICK_IDLE_MS);
});

test("tick delay caps at the fast interval for a far deadline", () => {
  assert.equal(accountsTickDelay(Date.parse("2026-09-27T00:00:00Z"), NOW), ACCOUNTS_TICK_MAX_MS);
});

test("tick delay targets an approaching deadline and never goes negative", () => {
  assert.equal(accountsTickDelay(NOW + 8_000, NOW), 8_000);
  assert.equal(accountsTickDelay(NOW - 1_000, NOW), ACCOUNTS_TICK_MIN_MS);
});

test("tick delay never fires faster than the minimum gap", () => {
  assert.equal(accountsTickDelay(NOW, NOW), ACCOUNTS_TICK_MIN_MS);
  assert.equal(accountsTickDelay(NOW + 250, NOW), ACCOUNTS_TICK_MIN_MS);
});

test("sub-second deadlines quantize up to the next whole second", () => {
  assert.equal(accountsTickDelay(NOW + 8_200, NOW), 9_000);
  assert.equal(accountsTickDelay(NOW + 8_900, NOW), 9_000);
  assert.equal(accountsTickDelay(NOW + 9_000, NOW), 9_000);
});
