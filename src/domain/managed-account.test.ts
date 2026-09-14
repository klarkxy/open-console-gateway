import assert from "node:assert/strict";
import test from "node:test";
import {
  DEFAULT_OPENCODE_INVITE_URL,
  MANAGED_SETUP_STEPS,
  browserViewUrl,
  normalizeOpenCodeInviteUrl,
  setupStepIndex,
} from "./managed-account.ts";

test("managed wizard steps keep google_account through ready in order and index", () => {
  assert.deepEqual(MANAGED_SETUP_STEPS, [
    "google_account",
    "opencode_registration",
    "payment",
    "key_verification",
    "ready",
  ]);
  assert.equal(setupStepIndex("google_account"), 0);
  assert.equal(setupStepIndex("ready"), MANAGED_SETUP_STEPS.length - 1);
});

test("OpenCode invite URLs are HTTPS, credential-free, bounded, and host allowlisted", () => {
  // The demo default must itself pass the allowlist unchanged.
  assert.equal(
    normalizeOpenCodeInviteUrl(DEFAULT_OPENCODE_INVITE_URL),
    DEFAULT_OPENCODE_INVITE_URL,
  );
  assert.equal(normalizeOpenCodeInviteUrl("  "), "");
  assert.equal(
    normalizeOpenCodeInviteUrl("https://opencode.ai/invite/demo"),
    "https://opencode.ai/invite/demo",
  );
  assert.equal(
    normalizeOpenCodeInviteUrl("https://console.opencode.ai/register?invite=demo"),
    "https://console.opencode.ai/register?invite=demo",
  );
  assert.throws(() => normalizeOpenCodeInviteUrl("http://opencode.ai/invite"), /HTTPS/);
  assert.throws(() => normalizeOpenCodeInviteUrl("https://user:pass@opencode.ai/invite"), /用户名或密码/);
  assert.throws(() => normalizeOpenCodeInviteUrl("https://opencode.ai.example/invite"), /域名/);
  assert.throws(() => normalizeOpenCodeInviteUrl(`https://opencode.ai/${"x".repeat(2049)}`), /2048/);
});

test("remote browser view URL preserves dashboard location and carries the opaque session token", () => {
  assert.equal(
    browserViewUrl("https://mgr.example/dashboard/?view=accounts", "abc/123"),
    "https://mgr.example/dashboard/?view=browser#session=abc%2F123",
  );
});
