import assert from "node:assert/strict";
import test from "node:test";
import { officialApi } from "./official-api.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";

test("official financial GETs stay local and balance POST uses captured CAS without a Key", async () => {
  setupControlPlane(5, 12, "price");
  const seen: string[] = [];
  installFetchMock(({ url, method, body }) => {
    seen.push(`${method} ${url}`);
    if (method === "POST") {
      assert.deepEqual(body, { expectedRevision: 3, processGeneration: 12 });
      assert.match(url, /\/dashboard\/api\/v4\/accounts\/account%2Fone\/official-api\/balance$/);
    } else assert.match(url, /\/dashboard\/api\/v4\/accounts\/account%2Fone\/official-api$/);
    return { revision: 5, processGeneration: 12, balances: [] };
  });
  await officialApi.status("account/one");
  assert.equal(seen.length, 1);
  await officialApi.refreshBalance("account/one", { expectedRevision: 3, processGeneration: 12 });
  assert.equal(seen.length, 2);
  for (const line of seen) {
    assert.doesNotMatch(line, /\/official-api\/pricing|\/pricing\/multipliers|\/providers\/[^/]+\/pricing/);
  }
});
