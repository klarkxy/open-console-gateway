import assert from "node:assert/strict";
import test from "node:test";
import { billingApi } from "./billing.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";

test("billing snapshots serialize one bounded local-read request without mutation tokens or secrets", async () => {
  setupControlPlane(5, 12, "price");
  const requests = installFetchMock(({ url, method, body }) => {
    assert.equal(method, "POST");
    assert.match(url, /\/dashboard\/api\/v4\/billing\/snapshots$/);
    assert.deepEqual(body, { accountIds: ["account/one", "account/two"] });
    return {
      statuses: [],
      errors: { "account/two": { code: "notFound", message: "missing", currentRevision: 5, processGeneration: 12 } },
      revision: 5,
      processGeneration: 12,
    };
  });
  const result = await billingApi.snapshots(["account/one", "account/two"]);
  assert.equal(requests.length, 1);
  assert.equal(result.errors["account/two"]?.code, "notFound");
});
