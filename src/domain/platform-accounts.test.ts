import assert from "node:assert/strict";
import test from "node:test";
import type { PlatformLink, PlatformPrice, PlatformSnapshot } from "../api/platform-accounts.ts";
import {
  formatPlatformRate,
  formatPlatformTime,
  formatQuotaAmount,
  importCandidateCapabilities,
  linkForAccount,
  linkedAccountIdSet,
  linksForPlatform,
  platformGroupLabel,
  platformInferenceEndpoint,
  platformModelCandidates,
  platformManualGroup,
  platformPriceFlags,
  platformPriceForModel,
  platformPriceRows,
  platformUnavailableReasonKey,
  combinedAvailableQuota,
  quotasByKind,
} from "./platform-accounts.ts";

test("platform inference endpoint mirrors the backend derivation per protocol", () => {
  assert.equal(
    platformInferenceEndpoint("https://newapi.example.com", "chat_completions"),
    "https://newapi.example.com/v1/chat/completions",
  );
  assert.equal(
    platformInferenceEndpoint("https://newapi.example.com", "responses"),
    "https://newapi.example.com/v1/responses",
  );
  assert.equal(
    platformInferenceEndpoint("https://newapi.example.com", "messages"),
    "https://newapi.example.com/v1/messages",
  );
  // A saved base carrying /v1 or a trailing slash normalizes to the site root.
  assert.equal(
    platformInferenceEndpoint("https://newapi.example.com/v1", "chat_completions"),
    "https://newapi.example.com/v1/chat/completions",
  );
  assert.equal(
    platformInferenceEndpoint("https://newapi.example.com/", "messages"),
    "https://newapi.example.com/v1/messages",
  );
  // Non-URLs, non-http(s) schemes, and credentialed URLs are never derived.
  assert.equal(platformInferenceEndpoint("not a url", "chat_completions"), null);
  assert.equal(platformInferenceEndpoint("ftp://example.com", "chat_completions"), null);
  assert.equal(platformInferenceEndpoint("https://user:pass@example.com", "chat_completions"), null);
  assert.equal(platformInferenceEndpoint("  ", "chat_completions"), null);
});

function price(overrides: Partial<PlatformPrice> = {}): PlatformPrice {
  return {
    model: "gpt-4o",
    groupId: null,
    currency: "USD",
    input: null,
    output: null,
    cacheRead: null,
    cacheWrite: null,
    source: "storefront",
    officialReference: false,
    unavailableReason: null,
    validUntil: 0,
    ...overrides,
  };
}

function snapshot(overrides: Partial<PlatformSnapshot> = {}): PlatformSnapshot {
  return {
    observedAt: 0,
    stale: false,
    errors: [],
    quotas: [],
    models: [],
    prices: [],
    groups: [],
    billingPreference: null,
    walletOverflow: null,
    ...overrides,
  };
}

function link(accountId: string, platformAccountId: string): PlatformLink {
  return {
    accountId,
    platformAccountId,
    group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: false },
    snapshot: null,
  };
}

test("links group by platform and by account", () => {
  const links = [link("a1", "p1"), link("a2", "p1"), link("a3", "p2")];
  assert.deepEqual(linksForPlatform(links, "p1").map((item) => item.accountId), ["a1", "a2"]);
  assert.deepEqual(linksForPlatform(links, "missing"), []);
  assert.equal(linkForAccount(links, "a3")?.platformAccountId, "p2");
  assert.equal(linkForAccount(links, "nope"), null);
  assert.deepEqual([...linkedAccountIdSet(links)].sort(), ["a1", "a2", "a3"]);
});

test("group label joins id, platform, and subscription type; empty when none", () => {
  assert.equal(
    platformGroupLabel({ id: "vip", platform: "OpenAI", subscriptionType: "按量付费" }),
    "vip · OpenAI · 按量付费",
  );
  assert.equal(platformGroupLabel({ id: "vip", platform: "OpenAI", subscriptionType: null }), "vip · OpenAI");
  assert.equal(platformGroupLabel({ id: "vip", platform: null, subscriptionType: null }), "vip");
  assert.equal(platformGroupLabel({ id: null, platform: null, subscriptionType: null }), "");
});

test("known unavailable reasons map to i18n keys, unknown codes stay raw", () => {
  assert.equal(platformUnavailableReasonKey("user_identity_required"), "需要登录身份才能查看价格");
  assert.equal(platformUnavailableReasonKey("group_model_unavailable"), "该分组不提供此模型");
  assert.equal(platformUnavailableReasonKey("reasoning_multiplier"), "按推理强度倍率计费");
  assert.equal(platformUnavailableReasonKey("some_future_code"), null);
  assert.equal(platformUnavailableReasonKey(null), null);
});

test("manual group entry trims and treats empty as unknown", () => {
  assert.deepEqual(platformManualGroup("  vip-group  ", " OpenAI "), { id: "vip-group", platform: "OpenAI" });
  assert.deepEqual(platformManualGroup("", ""), { id: null, platform: null });
  assert.deepEqual(platformManualGroup("   ", " \t "), { id: null, platform: null });
  assert.deepEqual(platformManualGroup("vip", ""), { id: "vip", platform: null });
  assert.throws(() => platformManualGroup("x".repeat(201), ""), RangeError);
  assert.throws(() => platformManualGroup("", "y".repeat(65)), RangeError);
  // Boundary values at the backend limits are accepted.
  assert.equal(platformManualGroup("x".repeat(200), "y".repeat(64)).id?.length, 200);
});

test("quota amounts carry their unit and never invent totals", () => {
  assert.equal(formatQuotaAmount(12.3456, "USD", "en-US"), "12.35 USD");
  assert.equal(formatQuotaAmount(100, "", "en-US"), "100");
});

test("o03 wallet subscription and key limits stay separate and are never summed", () => {
  const quotas = [
    { kind: "wallet" as const, remaining: 100, used: 10, limit: 110, unit: "USD", scopeId: "w", unlimited: false, period: null, resetsAt: null, expiresAt: null, source: "user" },
    { kind: "subscription" as const, remaining: 200, used: 20, limit: 220, unit: "USD", scopeId: "s", unlimited: false, period: "month", resetsAt: null, expiresAt: null, source: "sub" },
    { kind: "key_limit" as const, remaining: 50, used: 5, limit: 55, unit: "USD", scopeId: "k", unlimited: false, period: "5h", resetsAt: null, expiresAt: null, source: "key" },
  ];
  const byKind = quotasByKind(quotas);
  assert.equal(byKind.wallet.length, 1);
  assert.equal(byKind.subscription.length, 1);
  assert.equal(byKind.key_limit.length, 1);
  assert.equal(byKind.wallet[0]?.remaining, 100);
  assert.equal(byKind.subscription[0]?.remaining, 200);
  assert.equal(byKind.key_limit[0]?.remaining, 50);
  assert.equal(combinedAvailableQuota(quotas), null);
  const inventedTotal = 100 + 200 + 50;
  assert.notEqual(byKind.wallet[0]?.remaining, inventedTotal);
  assert.notEqual(byKind.subscription[0]?.remaining, inventedTotal);
  assert.notEqual(byKind.key_limit[0]?.remaining, inventedTotal);
});

test("o01 plaza candidates are discovery only and never an authorization proof", () => {
  const candidates = platformModelCandidates(snapshot({
    models: [{ id: "plaza-model", platform: "OpenAI", groupId: null, source: "storefront" }],
  }), []);
  assert.equal(candidates.length, 1);
  assert.equal(candidates[0]?.alreadyMapped, false);
  assert.equal(candidates[0]?.source, "storefront");
  assert.ok(!("authorized" in (candidates[0] ?? {})));
  assert.ok(!("permission" in (candidates[0] ?? {})));
  assert.ok(!("granted" in (candidates[0] ?? {})));
});

test("platform time is empty for missing observations", () => {
  assert.equal(formatPlatformTime(0, "en-US"), "");
  assert.ok(formatPlatformTime(1_700_000_000, "en-US").length > 0);
});

test("platform rates are per token with a per-million tooltip", () => {
  const rate = formatPlatformRate(0.0000025, "USD", "en-US");
  assert.ok(rate);
  assert.match(rate.label, /\/token$/);
  assert.match(rate.label, /0\.0*25/);
  assert.match(rate.perMillion ?? "", /2\.5/);
  assert.equal(formatPlatformRate(null, "USD", "en-US"), null);
  assert.equal(formatPlatformRate(0, "USD", "en-US")?.perMillion, null);
  // Non-ISO currency labels fall back to a plain suffix instead of throwing.
  const credits = formatPlatformRate(0.5, "credits", "en-US");
  assert.ok(credits?.label.includes("credits"));
});

test("o04 expired and stale prices are flagged and not treated as current quotes", () => {
  assert.deepEqual(platformPriceFlags(price({ validUntil: 99 }), false, 100), ["expired"]);
  assert.deepEqual(platformPriceFlags(price({ validUntil: 100 }), false, 100), ["expired"]);
  assert.deepEqual(platformPriceFlags(price(), true, 100), ["stale"]);
  const expiredRow = platformPriceRows(snapshot({
    models: [{ id: "gpt-4o", platform: null, groupId: null, source: "storefront" }],
    prices: [price({ validUntil: 99, input: 2 })],
  }), 100)[0];
  assert.ok(expiredRow?.flags.includes("expired"));
});

test("price flags distinguish reference, unavailable, expired, and stale", () => {
  assert.deepEqual(platformPriceFlags(price(), false, 100), []);
  assert.deepEqual(
    platformPriceFlags(price({ officialReference: true }), false, 100),
    ["official_reference"],
  );
  assert.deepEqual(
    platformPriceFlags(price({ unavailableReason: "no_price" }), false, 100),
    ["unavailable"],
  );
  assert.deepEqual(platformPriceFlags(price({ validUntil: 99 }), false, 100), ["expired"]);
  assert.deepEqual(platformPriceFlags(price({ validUntil: 100 }), false, 100), ["expired"]);
  assert.deepEqual(platformPriceFlags(price({ validUntil: 101 }), false, 100), []);
  assert.deepEqual(platformPriceFlags(price(), true, 100), ["stale"]);
});

test("price lookup prefers the exact group then the group-agnostic row", () => {
  const prices = [
    price({ model: "gpt-4o", groupId: null, input: 1 }),
    price({ model: "gpt-4o", groupId: "vip", input: 2 }),
  ];
  assert.equal(platformPriceForModel(prices, "gpt-4o", "vip")?.input, 2);
  assert.equal(platformPriceForModel(prices, "gpt-4o", "other")?.input, 1);
  assert.equal(platformPriceForModel(prices, "missing", null), null);
});

test("price lookup prefers the billed row over an official reference for the same model", () => {
  const prices = [
    price({ model: "gpt-4o", officialReference: true, input: 9 }),
    price({ model: "gpt-4o", input: 2 }),
  ];
  assert.equal(platformPriceForModel(prices, "gpt-4o", null)?.input, 2);
  // Official reference is still returned when it is the only row.
  assert.equal(platformPriceForModel([prices[0]!], "gpt-4o", null)?.input, 9);
});

test("price rows split billed and official prices for the same model into distinct rows", () => {
  const snap = snapshot({
    models: [{ id: "gpt-4o", platform: "OpenAI", groupId: null, source: "storefront" }],
    prices: [
      price({ model: "gpt-4o", input: 2, source: "billed" }),
      price({ model: "gpt-4o", officialReference: true, input: 9, source: "official" }),
    ],
  });
  const rows = platformPriceRows(snap, 100);
  assert.equal(rows.length, 2);
  const keys = rows.map((row) => row.key);
  assert.equal(new Set(keys).size, 2);
  const modelRow = rows.find((row) => row.key.startsWith("model:"));
  const officialRow = rows.find((row) => row.key.startsWith("price:official:"));
  assert.ok(modelRow && officialRow);
  assert.equal(modelRow.price?.input, 2);
  assert.equal(modelRow.distinction, "billed");
  assert.equal(officialRow.price?.input, 9);
  assert.equal(officialRow.distinction, "official");
  assert.ok(officialRow.flags.includes("official_reference"));
});

test("price rows keep single-source and orphan rows without a distinction", () => {
  const onlyOfficial = platformPriceRows(snapshot({
    models: [{ id: "gpt-4o", platform: null, groupId: null, source: "storefront" }],
    prices: [price({ model: "gpt-4o", officialReference: true })],
  }), 100);
  assert.equal(onlyOfficial.length, 1);
  assert.equal(onlyOfficial[0]?.distinction, null);
  assert.ok(onlyOfficial[0]?.flags.includes("official_reference"));

  const orphan = platformPriceRows(snapshot({
    prices: [price({ model: "unlisted-model", input: 1 })],
  }), 100);
  assert.equal(orphan.length, 1);
  assert.equal(orphan[0]?.model, "unlisted-model");
  assert.equal(orphan[0]?.distinction, null);
});

test("candidates dedupe, attach prices, and flag existing mappings", () => {
  const snap = snapshot({
    models: [
      { id: "gpt-4o", platform: "OpenAI", groupId: null, source: "storefront" },
      { id: "GPT-4o", platform: "OpenAI", groupId: null, source: "storefront" },
      { id: "claude-sonnet-4", platform: "Anthropic", groupId: "vip", source: "storefront" },
      { id: "deepseek-v3", platform: null, groupId: null, source: "storefront" },
    ],
    prices: [price({ model: "gpt-4o", input: 0.1 })],
  });
  const candidates = platformModelCandidates(snap, [
    { public_model: "DeepSeek-V3", upstream_model: "deepseek-v3" },
  ]);
  assert.deepEqual(candidates.map((candidate) => candidate.id), ["gpt-4o", "claude-sonnet-4", "deepseek-v3"]);
  assert.equal(candidates[0]?.price?.input, 0.1);
  assert.equal(candidates[1]?.price, null);
  assert.equal(candidates[2]?.alreadyMapped, true);
  assert.equal(candidates[0]?.alreadyMapped, false);
  assert.deepEqual(platformModelCandidates(null, []), []);
});

test("imported candidates become identity mappings with platform provenance", () => {
  const imported = importCandidateCapabilities(
    platformModelCandidates(snapshot({
      models: [{ id: "gpt-4o", platform: null, groupId: null, source: "storefront" }],
    }), []),
    "chat_completions",
  );
  assert.deepEqual(imported, [{
    public_model: "gpt-4o",
    upstream_model: "gpt-4o",
    protocol: "chat_completions",
    source: "platform",
  }]);
});
