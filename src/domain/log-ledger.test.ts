import assert from "node:assert/strict";
import test from "node:test";
import { enUSMessages } from "../i18n/messages/en-US.ts";
import {
  OPERATION_ACTION_KEYS,
  OPERATION_DOMAIN_KEYS,
  OPERATION_OUTCOME_KEYS,
  REQUEST_STATUS_KEYS,
  describeOperationAction,
  logicalRequestStatus,
  operationOutcomeKey,
  requestLogQueryKey,
  requestStatusKey,
  requestUsageTotals,
  showResourceSkeleton,
} from "./log-ledger.ts";

test("logical request status collapses success variants and leaves unresolved statuses", () => {
  assert.equal(logicalRequestStatus("success"), "success");
  assert.equal(logicalRequestStatus("success_unpriced"), "success");
  assert.equal(logicalRequestStatus("success_no_usage"), "success");
  assert.equal(logicalRequestStatus("streaming"), "streaming");
  assert.equal(logicalRequestStatus("outcome_unknown"), "outcome_unknown");
  assert.equal(logicalRequestStatus("cancelled"), "cancelled");
  assert.equal(logicalRequestStatus("error"), "error");
});

test("request summary counts logical requests, upstream attempts, and input plus output", () => {
  const totals = requestUsageTotals({
    totalRequests: 2,
    totalAttempts: 5,
    promptTokens: 10,
    completionTokens: 4,
    cachedTokens: 3,
  });
  assert.equal(totals.totalRequests, 2);
  assert.equal(totals.totalAttempts, 5);
  assert.equal(totals.cachedTokens, 3);
  assert.equal(totals.totalTokens, 14);
  assert.notEqual(totals.totalTokens, totals.inputTokens + totals.outputTokens + totals.cachedTokens);
});

test("a zero-attempt logical request stays at zero attempts", () => {
  const totals = requestUsageTotals({
    totalRequests: 1,
    totalAttempts: 0,
    promptTokens: 0,
    completionTokens: 0,
    cachedTokens: 0,
  });
  assert.equal(totals.totalRequests, 1);
  assert.equal(totals.totalAttempts, 0);
});

test("skeleton is only for the first load", () => {
  assert.equal(showResourceSkeleton(true, false), true);
  assert.equal(showResourceSkeleton(true, true), false);
  assert.equal(showResourceSkeleton(false, true), false);
});

test("operation and request labels map known codes and fall back without inventing a verb", () => {
  assert.equal(operationOutcomeKey("pending"), OPERATION_OUTCOME_KEYS.pending);
  assert.notEqual(OPERATION_OUTCOME_KEYS.pending, OPERATION_OUTCOME_KEYS.success);
  assert.equal(operationOutcomeKey("running"), null);
  assert.equal(describeOperationAction("account.create").kind, "mapped");
  assert.equal(describeOperationAction("key.regenerate").kind, "mapped");
  const provider = describeOperationAction("provider.futureverb");
  assert.equal(provider.kind, "domain");
  if (provider.kind === "domain") {
    assert.equal(provider.domainKey, OPERATION_DOMAIN_KEYS.provider);
    assert.equal(provider.verb, "futureverb");
  }
  assert.equal(describeOperationAction("cpa.install").kind, "domain");
  assert.equal(describeOperationAction("account.explode").kind, "fallback");
  assert.equal(describeOperationAction("not-an-action").kind, "fallback");
  assert.equal(requestStatusKey("success_unpriced"), REQUEST_STATUS_KEYS.success);
  assert.equal(requestStatusKey("streaming"), REQUEST_STATUS_KEYS.streaming);
  assert.equal(requestStatusKey("mystery"), null);
  assert.equal(OPERATION_ACTION_KEYS["settings.update"] in enUSMessages, true);
});

test("unresolved operation and request codes retain distinct semantic mappings", () => {
  assert.equal(operationOutcomeKey("pending"), OPERATION_OUTCOME_KEYS.pending);
  assert.notEqual(operationOutcomeKey("pending"), operationOutcomeKey("success"));
  assert.equal(requestStatusKey("streaming"), REQUEST_STATUS_KEYS.streaming);
  assert.notEqual(requestStatusKey("streaming"), requestStatusKey("success"));
  assert.notEqual(requestStatusKey("outcome_unknown"), requestStatusKey("error"));
});

test("prototype-like unknown action codes use the ordinary fallback", () => {
  for (const action of ["constructor", "tostring", "valueof", "__proto__"]) {
    const label = describeOperationAction(action);
    assert.equal(label.kind, "fallback");
  }
});

test("request page identity includes offset and request id", () => {
  const first = requestLogQueryKey({ limit: 20, offset: 0, requestId: "abc" });
  const nextPage = requestLogQueryKey({ limit: 20, offset: 20, requestId: "abc" });
  const otherId = requestLogQueryKey({ limit: 20, offset: 0, requestId: "def" });
  assert.notEqual(first, nextPage);
  assert.notEqual(first, otherId);
});
