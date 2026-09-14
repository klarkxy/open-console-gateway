import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import {
  accountExpiryLabel,
  accountMenuOptions,
  accountRoutingDraftDescription,
  accountRoutingDraftLabel,
  accountStatusLabel,
  accountStatusTagType,
  usageSyncCaption,
} from "./account-display.ts";

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

test("missing expiry date is unlabeled rather than zero days overdue", () => {
  assert.equal(accountExpiryLabel(draftAccount({ expires_on: "" })), "未设置");
});

test("unroutable account drafts use verification status rather than provider-specific branches", () => {
  const base = { setup_step: "ready" as const, plan_routable: false };
  assert.equal(
    accountRoutingDraftLabel({ ...base, verification_status: "pending" }),
    "待验证",
  );
  assert.equal(
    accountRoutingDraftDescription({ ...base, verification_status: "pending" }),
    "该方案验证功能暂不可用，创建后保持禁用草稿。",
  );
  assert.equal(
    accountRoutingDraftLabel({ ...base, verification_status: "failed" }),
    "验证失败",
  );
  assert.equal(
    accountRoutingDraftLabel({ ...base, verification_status: "not_required" }),
    "等待支持",
  );
  assert.equal(
    accountRoutingDraftLabel({ ...base, plan_routable: true, verification_status: "verified" }),
    null,
  );
});

test("draft status replaces disabled instead of rendering a second competing state", () => {
  const pending = draftAccount();
  assert.equal(accountStatusLabel(pending), "待验证");
  assert.equal(accountStatusTagType(pending), "warning");

  const unsupported = draftAccount({ verification_status: "not_required" });
  assert.equal(accountStatusLabel(unsupported), "等待支持");
  assert.equal(accountStatusTagType(unsupported), "warning");

  const failed = draftAccount({ verification_status: "failed" });
  assert.equal(accountStatusLabel(failed), "验证失败");
  assert.equal(accountStatusTagType(failed), "error");

  const ordinaryDisabled = draftAccount({ plan_routable: true, verification_status: "verified" });
  assert.equal(accountStatusLabel(ordinaryDisabled), "已禁用");
  assert.equal(accountStatusTagType(ordinaryDisabled), "error");
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
  assert.equal(accountStatusLabel(pending), "已禁用");
  assert.equal(accountStatusTagType(pending), "error");
  assert.equal(accountStatusLabel(custom({ verification_status: "failed" })), "已禁用");
  assert.equal(accountStatusLabel(custom({ verification_status: "verified" })), "已禁用");
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

  assert.equal(accountStatusLabel(goat()), "已禁用");
  assert.equal(accountStatusTagType(goat()), "error");
  // Ready + enabled is the routing-on configuration state, not an availability claim.
  assert.equal(accountStatusLabel(goat({ enabled: true })), "已启用");
  assert.equal(accountStatusTagType(goat({ enabled: true })), "success");
  assert.equal(accountStatusLabel(goat({ plan_routable: false })), "等待支持");
});

test("upstream auth failure is a distinct unavailable state, not cooldown", () => {
  const broken = draftAccount({
    plan_routable: true,
    verification_status: "verified",
    enabled: true,
    auth_error: "401",
  });
  assert.equal(accountStatusLabel(broken), "不可用");
  assert.equal(accountStatusTagType(broken), "error");
  assert.equal(
    accountStatusLabel({ ...broken, enabled: false }),
    "已禁用 · 不可用",
  );
});

test("usage sync captions show last success or never-synced, never a refresh cooldown", () => {
  const neverSynced = draftAccount({
    plan_routable: true,
    verification_status: "verified",
  });
  assert.equal(usageSyncCaption(neverSynced), "尚未官方同步");

  const cooling = draftAccount({
    plan_routable: true,
    verification_status: "verified",
    usage_sync_last_success_at: "2026-08-20T12:00:00Z",
    usage_sync_next_allowed_at: "2026-08-21T00:01:00Z",
  });
  assert.match(usageSyncCaption(cooling), /上次官方同步:/);
  assert.doesNotMatch(usageSyncCaption(cooling), /刷新额度冷却中/);
});
