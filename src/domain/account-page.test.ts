import assert from "node:assert/strict";
import { test } from "node:test";
import type { AccountPageCard } from "../api/pages.ts";
import { accountPageActionKey, accountPageDemandIds, accountPageRefreshErrorCodes, accountPageStatus, accountPageTagType, accountRoutingSource, pageAction, accountPageQuotaReceipt } from "./account-page.ts";

test("server actions keep permissions and normalize only operation names", () => {
  const action = { key: "refreshUsage", allowed: false, reason: "throttled" };
  assert.equal(accountPageActionKey(action.key), "refresh-usage");
  assert.equal(pageAction([action], "refresh-usage"), action);
  assert.equal(pageAction([action], "delete"), null);
});
test("status formatting uses server codes and retains unknown status explicitly", () => {
  assert.equal(accountPageStatus("cooling"), "cooling"); assert.equal(accountPageTagType("cooling"), "warning");
  assert.equal(accountPageStatus("future-state"), "unknown");
});
test("viewport demand excludes offscreen rows and folded cards rather than declaring filtered rows visible", () => {
  const cards = [
    { cardId: "open", rows: [{ credential: { id: "visible", legacyAccountId: "a" }, account: { id: "a" }, refresh: { supported: true } },
      { credential: { id: "offscreen", legacyAccountId: "b" }, account: { id: "b" }, refresh: { supported: true } },
      { credential: { id: "unsupported", legacyAccountId: "d" }, account: { id: "d" }, refresh: { supported: false } }] },
    { cardId: "folded", rows: [{ credential: { id: "folded-row", legacyAccountId: "c" }, account: { id: "c" }, refresh: { supported: true } }] },
  ] as AccountPageCard[];
  assert.deepEqual(accountPageDemandIds(cards, new Set(["visible", "folded-row", "unsupported"]), new Set(["folded"])), ["a"]);
});
test("routing presentation prefers newer server page facts and then a newer settings receipt", () => {
  const page = { routingMode: "round-robin" as const, conversationSticky: false, revision: { revision: 2, processGeneration: 1 } };
  assert.equal(accountRoutingSource(page, { revision: 1, process_generation: 1 }, 1), "page");
  assert.equal(accountRoutingSource(page, { revision: 3, process_generation: 1 }, 1), "settings");
  assert.equal(accountRoutingSource(page, { revision: 1, process_generation: 2 }, 2), "settings");
});
test("manual partial refresh keeps component error codes and deduplicates repeated failures", () => {
  assert.deepEqual(accountPageRefreshErrorCodes({ outcome: "partial", errors: [{ code: "models_unavailable" },
    { code: "balance_unavailable" }, { code: "models_unavailable" }] }), ["models_unavailable", "balance_unavailable"]);
});

test("a same-binding manual acknowledgement remains visible beside canonical sibling windows", () => {
  const row = { account: { id: "a", updatedAt: "v1" }, inferenceEndpointUrl: "https://example.test/v1",
    billing: { usage: { accountId: "a", providerId: "opencode", quotaWindows: [
      { accountId: "a", windowKind: "five_hours", used: 70, limitValue: 100 },
      { accountId: "a", windowKind: "week", used: 40, limitValue: 100 },
    ], creditBalances: [], syncState: null } },
  } as unknown as import("../api/pages.ts").AccountPageRow;
  const slot = { boundVersion: "v1\0https://example.test/v1", manualReceipt: { windows: [{
    windowKind: "five_hours", used: 0, limitValue: 100 as const, unit: "percent" as const, source: "manual" as const,
    observedAt: "2026-10-07T00:00:00Z", updatedAt: "2026-10-07T00:00:00Z", resetsAt: null,
  }] } };
  assert.deepEqual(accountPageQuotaReceipt(row, slot)?.quota_windows.map(window => [window.window_kind, window.used]),
    [["five_hours", 0], ["week", 40]]);
  assert.equal(accountPageQuotaReceipt(row, { ...slot, boundVersion: "previous-binding" }), null);
});
