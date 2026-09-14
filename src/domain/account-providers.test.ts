import assert from "node:assert/strict";
import { test } from "node:test";

import {
  OLLAMA_PROVIDER_ID,
  ZEN_FREE_ACCOUNT_ID,
  ZEN_FREE_OFFERING,
  ZEN_FREE_PROVIDER_ID,
  isOllamaCloudAccount,
  isZenFreeAccount,
} from "./account-providers.ts";
import { findPlanDefinition } from "./plans.ts";

test("Zen Free offering is the egress-IP sealed route", () => {
  assert.equal(ZEN_FREE_OFFERING.quota_scope, "egress-ip");
  assert.equal(isZenFreeAccount({ id: ZEN_FREE_ACCOUNT_ID, provider_id: "opencode" }), true);
  assert.equal(isZenFreeAccount({ id: "other", provider_id: ZEN_FREE_PROVIDER_ID }), true);
  assert.equal(isZenFreeAccount({ id: "other", provider_id: "opencode" }), false);
});

test("ollama cloud account predicate matches the sealed family exactly", () => {
  assert.ok(isOllamaCloudAccount({ provider_id: OLLAMA_PROVIDER_ID }));
  assert.ok(!isOllamaCloudAccount({ provider_id: "opencode" }));
  assert.ok(!isOllamaCloudAccount({ provider_id: "kimi" }));
});

test("ollama cloud plan definition follows the sealed registry identities", () => {
  const plan = findPlanDefinition(OLLAMA_PROVIDER_ID);
  assert.ok(plan, "the plan definition must exist");
  assert.equal(plan.provider_id, OLLAMA_PROVIDER_ID);
});
