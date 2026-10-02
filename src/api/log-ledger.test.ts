import assert from "node:assert/strict";
import test from "node:test";
import { operationLogSearch, requestAttemptsPath, requestLogSearch } from "./log-ledger.ts";

test("request search keeps logical status, page, and the pending request id", () => {
  const search = requestLogSearch({
    status: "streaming",
    keyId: "key-1",
    requestId: "req exact",
    limit: 20,
    offset: 40,
  });
  const params = new URLSearchParams(search);
  assert.equal(params.get("status"), "streaming");
  assert.equal(params.get("keyId"), "key-1");
  assert.equal(params.get("requestId"), "req exact");
  assert.equal(params.get("limit"), "20");
  assert.equal(params.get("offset"), "40");
  assert.equal(params.get("sortBy"), null);
});

test("blank request id is omitted so an unfiltered page sends no unknown field", () => {
  const params = new URLSearchParams(requestLogSearch({ requestId: "  ", limit: 20, offset: 0 }));
  assert.equal(params.get("requestId"), null);
  assert.equal(params.get("offset"), "0");
});

test("attempt reads encode request and legacy keys as one path segment", () => {
  assert.equal(requestAttemptsPath("request:exact/id"), "/logs/requests/request%3Aexact%2Fid/attempts");
  assert.equal(requestAttemptsPath("legacy:15"), "/logs/requests/legacy%3A15/attempts");
  assert.equal(requestAttemptsPath("request:legacy:1").startsWith("/logs/requests/"), true);
  assert.equal(requestAttemptsPath("request:legacy:1").includes("/attempts"), true);
  assert.equal(requestAttemptsPath("request:legacy:1").includes("request:legacy:1"), false);
});

test("operation search sends outcome and source without a severity filter", () => {
  const params = new URLSearchParams(operationLogSearch({
    outcome: "pending",
    source: "desktop",
    action: "account.create",
    limit: 20,
    offset: 0,
  }));
  assert.equal(params.get("outcome"), "pending");
  assert.equal(params.get("source"), "desktop");
  assert.equal(params.get("action"), "account.create");
  assert.equal(params.get("level"), null);
});
