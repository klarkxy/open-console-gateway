import assert from "node:assert/strict";
import test from "node:test";
import { attentionTagType } from "./dashboard-attention.ts";

test("attention severity preserves error, cooling warning, and onboarding information", () => {
  assert.equal(attentionTagType("auth-error"), "error");
  assert.equal(attentionTagType("expired"), "error");
  assert.equal(attentionTagType("cooling"), "warning");
  assert.equal(attentionTagType("setup-incomplete"), "info");
});
