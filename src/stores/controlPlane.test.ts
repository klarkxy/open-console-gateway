import assert from "node:assert/strict";
import test from "node:test";
import { DashboardConflictError } from "../api/dashboard-v3.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

test("runMutation sends a captured expectation instead of the store's later tokens", async () => {
  setupControlPlane(4, 11, "p1");
  const control = useControlPlaneStore();
  control.sync({ revision: 8, processGeneration: 11, pricingRevision: "p1" });

  let used = { expectedRevision: 0, processGeneration: 0 };
  const result = await control.runMutation(
    async (expectation) => {
      used = expectation;
      return "ok";
    },
    { expectedRevision: 4, processGeneration: 11 },
  );
  assert.equal(result, "ok");
  assert.deepEqual(used, { expectedRevision: 4, processGeneration: 11 });
});

test("runMutation rethrows the original 409 when GET /contract fails", async () => {
  setupControlPlane(4, 11, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/contract") && method === "GET") {
      throw new Error("contract unavailable");
    }
    throw new Error(`unexpected request ${url}`);
  });
  const control = useControlPlaneStore();
  const conflict = new DashboardConflictError("revision conflict", 5, 11);

  await assert.rejects(
    () => control.runMutation(async () => {
      throw conflict;
    }),
    (error: unknown) => error === conflict,
  );
  assert.equal(requests.filter((request) => request.method === "GET").length, 1);
  assert.equal(control.revision, 4);
});
