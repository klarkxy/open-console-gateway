import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import type { PlatformLink, PlatformSnapshot } from "../api/platform-accounts.ts";
import {
  canImportPlatformKeys,
  composeNewApiUserCredential,
  discoveredModelCapabilities,
  PLATFORM_CREDENTIAL_TAG_KEYS,
  platformCredentialTags,
  platformKeyGroupLabel,
  uniquePublicModelCount,
  platformKeyModelRows,
  platformKeyQuotaName,
  platformModelOverlay,
  formatPlatformTime,
  formatQuotaAmount,
  primaryQuota,
  platformWalletMeter,
  walletMonthQuota,
  importCandidateCapabilities,
  linkForAccount,
  linkedAccountIdSet,
  newApiCredentialIssue,
  platformSnapshotErrorKey,
  platformGroupLabel,
  platformHostedEndpoint,
  platformInferenceEndpoint,
  platformModelCandidates,
  platformManualGroup,
  quotasByKind,
} from "./platform-accounts.ts";

test("New API key import is only offered with a user credential", () => {
  assert.equal(canImportPlatformKeys({ kind: "new_api", hasUserCredential: true }), true);
  assert.equal(canImportPlatformKeys({ kind: "new_api", hasUserCredential: false }), false);
  assert.equal(canImportPlatformKeys({ kind: "sub2api", hasUserCredential: true }), false);
});

test("snapshot error codes map to copy keys and unknown codes stay generic", () => {
  assert.equal(platformSnapshotErrorKey("auth.missing"), "未保存管理凭证");
  assert.equal(platformSnapshotErrorKey("unauthorized"), "管理凭证无效");
  assert.equal(platformSnapshotErrorKey("not-a-real-code"), "刷新未完成");
});

test("New API credential is user id and token together, or omitted", () => {
  assert.equal(newApiCredentialIssue("", ""), null);
  assert.equal(newApiCredentialIssue("  ", "  "), null);
  assert.equal(newApiCredentialIssue("18", ""), "user_id_without_token");
  assert.equal(newApiCredentialIssue("", "pat"), "token_without_user_id");
  assert.equal(newApiCredentialIssue("ab", "pat"), "user_id_not_digits");
  assert.equal(newApiCredentialIssue("18", "pat"), null);
  assert.equal(composeNewApiUserCredential("", ""), undefined);
  assert.equal(composeNewApiUserCredential("18", ""), undefined);
  assert.equal(composeNewApiUserCredential("18", " pat "), "18:pat");
});

test("platform hosted endpoint is the site root", () => {
  assert.equal(platformHostedEndpoint("https://newapi.example.com"), "https://newapi.example.com");
  assert.equal(platformHostedEndpoint("https://newapi.example.com/v1"), "https://newapi.example.com");
  assert.equal(platformHostedEndpoint("https://newapi.example.com/"), "https://newapi.example.com");
  assert.equal(platformHostedEndpoint("not a url"), null);
  assert.equal(platformHostedEndpoint("https://user:pass@example.com"), null);
});

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
  assert.equal(
    platformInferenceEndpoint("https://newapi.example.com/chat/v1", "chat_completions"),
    "https://newapi.example.com/chat/v1/chat/completions",
  );
  // Non-URLs, non-http(s) schemes, and credentialed URLs are never derived.
  assert.equal(platformInferenceEndpoint("not a url", "chat_completions"), null);
  assert.equal(platformInferenceEndpoint("ftp://example.com", "chat_completions"), null);
  assert.equal(platformInferenceEndpoint("https://user:pass@example.com", "chat_completions"), null);
  assert.equal(platformInferenceEndpoint("  ", "chat_completions"), null);
});

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

test("same public models on two Keys overlay without merging rates", () => {
  const overlay = platformModelOverlay([
    {
      id: "stable",
      model_capabilities: [
        { public_model: "gpt-5.5", upstream_model: "gpt-5.5", protocol: "chat_completions", source: "discovery", verified_at: null },
        { public_model: "gpt-6-astra", upstream_model: "gpt-6-astra", protocol: "chat_completions", source: "discovery", verified_at: null },
      ],
    } as Account,
    {
      id: "pro",
      model_capabilities: [
        { public_model: "GPT-5.5", upstream_model: "GPT-5.5", protocol: "chat_completions", source: "discovery", verified_at: null },
        { public_model: "gpt-5.3-codex-spark", upstream_model: "gpt-5.3-codex-spark", protocol: "chat_completions", source: "discovery", verified_at: null },
      ],
    } as Account,
  ]);
  assert.equal(overlay.uniqueIds.length, 3);
  assert.deepEqual(overlay.sharedIds, ["gpt-5.5"]);
  assert.deepEqual(overlay.keys.find((row) => row.accountId === "stable"), {
    accountId: "stable",
    total: 2,
    shared: 1,
    exclusive: 1,
  });
  assert.deepEqual(overlay.keys.find((row) => row.accountId === "pro"), {
    accountId: "pro",
    total: 2,
    shared: 1,
    exclusive: 1,
  });
});

test("unique public model count collapses protocol rows for the same name", () => {
  assert.equal(uniquePublicModelCount(null), 0);
  assert.equal(uniquePublicModelCount({
    model_capabilities: [
      { public_model: "gpt-5.5", upstream_model: "gpt-5.5", protocol: "chat_completions", source: "discovery", verified_at: null },
      { public_model: "gpt-5.5", upstream_model: "gpt-5.5", protocol: "messages", source: "discovery", verified_at: null },
      { public_model: "gpt-5.5", upstream_model: "gpt-5.5", protocol: "responses", source: "discovery", verified_at: null },
      { public_model: "GPT-5.5", upstream_model: "GPT-5.5", protocol: "chat_completions", source: "discovery", verified_at: null },
      { public_model: "codex", upstream_model: "codex", protocol: "chat_completions", source: "discovery", verified_at: null },
    ],
  } as Account), 2);
});

test("platform Key model rows collapse protocols and keep the first upstream id", () => {
  assert.deepEqual(platformKeyModelRows(null), []);
  assert.deepEqual(platformKeyModelRows({
    model_capabilities: [
      { public_model: "gpt-5.5", upstream_model: "openai/gpt-5.5", protocol: "chat_completions", source: "discovery", verified_at: null },
      { public_model: "gpt-5.5", upstream_model: "ignored", protocol: "messages", source: "discovery", verified_at: null },
      { public_model: "codex", upstream_model: "codex", protocol: "responses", source: "discovery", verified_at: null },
      { public_model: "  ", upstream_model: "blank", protocol: "chat_completions", source: "discovery", verified_at: null },
    ],
  } as Account), [
    { public_model: "gpt-5.5", upstream_model: "openai/gpt-5.5", protocols: ["chat_completions", "messages"] },
    { public_model: "codex", upstream_model: "codex", protocols: ["responses"] },
  ]);
});

test("platform credential tags label group and token, never a bare snapshot id", () => {
  assert.deepEqual(platformCredentialTags({
    group: "codex-pro",
    tokenName: "cli-pro",
    accountName: "codex-pro-0.25",
    modelCount: 9,
  }), [
    { kind: "group", name: "codex-pro" },
    { kind: "token", name: "cli-pro" },
    { kind: "models", count: 9 },
  ]);
  assert.deepEqual(platformCredentialTags({
    group: "  ",
    tokenName: "gpt-0.05",
    accountName: "gpt-0.05",
    modelCount: 7,
  }), [{ kind: "models", count: 7 }]);
  assert.deepEqual(platformCredentialTags({
    group: "",
    tokenName: "",
    accountName: "orphan",
    modelCount: 0,
  }), [{ kind: "models", count: 0 }]);
  assert.deepEqual(Object.keys(PLATFORM_CREDENTIAL_TAG_KEYS).sort(), ["group", "models", "token"]);
});

test("key identity uses observed token name and group", () => {
  assert.equal(platformKeyQuotaName({
    observedAt: 1,
    stale: false,
    errors: [],
    quotas: [{
      kind: "key_limit",
      scopeId: "cli-pro",
      unit: "quota",
      used: 1,
      remaining: 2,
      limit: 3,
      unlimited: false,
      period: null,
      resetsAt: null,
      expiresAt: null,
      source: "new_api.token_usage",
    }],
    models: [],
    prices: [],
    groups: [{ id: "Codex-Pro", platform: null, subscriptionType: null, autoGroups: [], verified: true }],
    billingPreference: null,
    walletOverflow: null,
  }), "cli-pro");
  assert.equal(
    platformKeyGroupLabel(
      { group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: false } },
      { observedAt: 1, stale: false, errors: [], quotas: [], models: [], prices: [], groups: [{ id: "Codex稳定", platform: null, subscriptionType: null, autoGroups: [], verified: true }], billingPreference: null, walletOverflow: null },
    ),
    "Codex稳定",
  );
});

test("discovered models become exact public=upstream mappings", () => {
  assert.deepEqual(discoveredModelCapabilities(["claude-sonnet", "gpt-4o"]), [
    { public_model: "claude-sonnet", upstream_model: "claude-sonnet", protocol: "chat_completions", source: "discovery" },
    { public_model: "gpt-4o", upstream_model: "gpt-4o", protocol: "chat_completions", source: "discovery" },
  ]);
});

test("links resolve by account and collect linked ids", () => {
  const links = [link("a1", "p1"), link("a2", "p1"), link("a3", "p2")];
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
  assert.equal(formatQuotaAmount(12.3456, "USD", "en-US"), "$12.3456");
  assert.equal(formatQuotaAmount(3, "usd", "en-US"), "$3.00");
  assert.equal(formatQuotaAmount(100, "", "en-US"), "100");
  assert.equal(formatQuotaAmount(1.5, "quota", "en-US"), "1.5 quota");
});

test("primary remaining prefers the overall Key quota over a time window", () => {
  const quotas = [
    { kind: "key_limit" as const, remaining: 16, used: 4, limit: 20, unit: "usd", scopeId: "key:5h", unlimited: false, period: "5h", resetsAt: 1, expiresAt: null, source: "key" },
    { kind: "key_limit" as const, remaining: 27.5, used: 12.5, limit: 40, unit: "usd", scopeId: "key", unlimited: false, period: null, resetsAt: null, expiresAt: null, source: "key" },
    { kind: "wallet" as const, remaining: 15.5, used: null, limit: null, unit: "usd", scopeId: "wallet", unlimited: false, period: null, resetsAt: null, expiresAt: null, source: "profile" },
  ];
  assert.equal(primaryQuota(quotas, "key_limit")?.remaining, 27.5);
  assert.equal(primaryQuota(quotas, "wallet")?.remaining, 15.5);
  assert.equal(primaryQuota([], "wallet"), null);
});

test("wallet month used stays off the remaining figure", () => {
  const quotas = [
    { kind: "wallet" as const, remaining: null, used: 4, limit: null, unit: "usd", scopeId: "wallet:month", unlimited: false, period: "month", resetsAt: null, expiresAt: null, source: "new_api.log_self_stat" },
    { kind: "wallet" as const, remaining: 27.79, used: 82.21, limit: null, unit: "usd", scopeId: "wallet", unlimited: false, period: null, resetsAt: null, expiresAt: null, source: "new_api.user_self" },
  ];
  assert.equal(primaryQuota(quotas, "wallet")?.remaining, 27.79);
  assert.equal(walletMonthQuota(quotas)?.used, 4);
  const meter = platformWalletMeter({
    observedAt: 1_753_000_000,
    stale: false,
    errors: [],
    quotas,
    models: [],
    prices: [],
    groups: [],
    billingPreference: null,
    walletOverflow: null,
  });
  assert.equal(meter?.remaining, 27.79);
  assert.equal(meter?.historyUsed, 82.21);
  assert.equal(meter?.monthUsed, 4);
  assert.equal(meter?.remainingUnlimited, false);
  assert.equal(platformWalletMeter(null), null);
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

test("candidates dedupe model ids and flag existing mappings", () => {
  const snap = snapshot({
    models: [
      { id: "gpt-4o", platform: "OpenAI", groupId: null, source: "storefront" },
      { id: "GPT-4o", platform: "OpenAI", groupId: null, source: "storefront" },
      { id: "claude-sonnet-4", platform: "Anthropic", groupId: "vip", source: "storefront" },
      { id: "deepseek-v3", platform: null, groupId: null, source: "storefront" },
    ],
    prices: [],
  });
  const candidates = platformModelCandidates(snap, [
    { public_model: "DeepSeek-V3", upstream_model: "deepseek-v3" },
  ]);
  assert.deepEqual(candidates.map((candidate) => candidate.id), ["gpt-4o", "claude-sonnet-4", "deepseek-v3"]);
  assert.equal(candidates[1]?.groupId, "vip");
  assert.equal(candidates[2]?.alreadyMapped, true);
  assert.equal(candidates[0]?.alreadyMapped, false);
  assert.equal("price" in (candidates[0] ?? {}), false);
  assert.deepEqual(platformModelCandidates(null, []), []);
});

test("model import keeps ids and groups when the stored snapshot has no prices", () => {
  const candidates = platformModelCandidates(snapshot({
    models: [
      { id: "gpt-4o", platform: "OpenAI", groupId: "vip", source: "storefront" },
      { id: "deepseek-v3", platform: null, groupId: null, source: "storefront" },
    ],
    prices: [],
  }), []);
  assert.deepEqual(candidates.map((candidate) => candidate.id), ["gpt-4o", "deepseek-v3"]);
  assert.equal(candidates[0]?.groupId, "vip");
  assert.equal(candidates[0]?.platform, "OpenAI");
  assert.equal("price" in (candidates[0] ?? {}), false);
  assert.equal(candidates[1]?.groupId, null);
  const imported = importCandidateCapabilities(candidates, "chat_completions");
  assert.deepEqual(imported, [
    { public_model: "gpt-4o", upstream_model: "gpt-4o", protocol: "chat_completions", source: "platform" },
    { public_model: "deepseek-v3", upstream_model: "deepseek-v3", protocol: "chat_completions", source: "platform" },
  ]);
  for (const row of imported) {
    assert.equal("price" in row, false);
    assert.equal("input" in row, false);
  }
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
