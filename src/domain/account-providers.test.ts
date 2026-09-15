import assert from "node:assert/strict";
import { test } from "node:test";

import {
  OLLAMA_PROVIDER_ID,
  ZEN_FREE_ACCOUNT_ID,
  ZEN_FREE_PROVIDER_ID,
  isOllamaCloudAccount,
  isZenFreeAccount,
} from "./account-providers.ts";
import { findPlanDefinition } from "./plans.ts";
import { OPENCODE_GO_PLAN } from "./plans.ts";

test("Zen Free account predicate matches the sealed singleton and provider", () => {
  assert.equal(isZenFreeAccount({ id: ZEN_FREE_ACCOUNT_ID, provider_id: "opencode" }), true);
  assert.equal(isZenFreeAccount({ id: "other", provider_id: ZEN_FREE_PROVIDER_ID }), true);
  assert.equal(isZenFreeAccount({ id: "other", provider_id: "opencode" }), false);
});

test("ollama cloud account predicate matches the sealed family exactly", () => {
  assert.ok(isOllamaCloudAccount({ provider_id: OLLAMA_PROVIDER_ID }));
  assert.ok(!isOllamaCloudAccount({ provider_id: "opencode" }));
  assert.ok(!isOllamaCloudAccount({ provider_id: "kimi" }));
});

test("ollama surface exists only when the catalog supplies it", () => {
  assert.equal(findPlanDefinition(OLLAMA_PROVIDER_ID), undefined);
  const plan = findPlanDefinition(OLLAMA_PROVIDER_ID, [{
    ...OPENCODE_GO_PLAN,
    provider_id: OLLAMA_PROVIDER_ID,
    display_name: "Ollama Cloud",
  }]);
  assert.ok(plan);
  assert.equal(plan.provider_id, OLLAMA_PROVIDER_ID);
});
