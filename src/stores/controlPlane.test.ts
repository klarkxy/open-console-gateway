import assert from "node:assert/strict";
import test from "node:test";
import { DashboardConflictError, requestV3, withExpectation } from "../api/dashboard-v3.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import {
  isLocalMutationBusy,
  isLocalMutationCancelled,
  useControlPlaneStore,
} from "./controlPlane.ts";

function deferred<T = void>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void; reject: (error: unknown) => void } {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

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

test("two quick independent local writes serialize; the second reads the first receipt's tokens", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/write-a")) return gate.promise;
    if (req.url.endsWith("/write-b")) return { revision: 9, processGeneration: 99, ok: true };
    throw new Error(`unexpected request ${req.url}`);
  });
  const control = useControlPlaneStore();
  const first = control.runLocalMutation("a", (expectation) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  const second = control.runLocalMutation("b", (expectation) => requestV3("/write-b", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  // The second write is still queued behind the slow first one.
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["write-a"]);
  assert.deepEqual(requests[0]?.body, { expectedRevision: 7, processGeneration: 99 });
  gate.resolve(new Response(JSON.stringify({ revision: 8, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await first;
  await second;
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["write-a", "write-b"]);
  // Dispatched after the first receipt synced its revision: no self-conflict.
  assert.deepEqual(requests[1]?.body, { expectedRevision: 8, processGeneration: 99 });
});

test("a duplicate submission for a busy target rejects instead of coalescing", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  let calls = 0;
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/write-a")) {
      calls += 1;
      // The first call hangs on the gate; later calls answer immediately.
      return calls === 1 ? gate.promise : { revision: 8 + calls, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${req.url}`);
  });
  const control = useControlPlaneStore();
  const mutate = (expectation: { expectedRevision: number; processGeneration: number }) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  });
  const first = control.runLocalMutation("a", mutate);
  // Same target while one is in flight: rejected, never a shared promise.
  await assert.rejects(control.runLocalMutation("a", mutate), isLocalMutationBusy);
  gate.resolve(new Response(JSON.stringify({ revision: 8, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await first;
  assert.equal(requests.length, 1);
  // Once the first write settled, the target is free again.
  await control.runLocalMutation("a", mutate);
  assert.equal(requests.length, 2);
});

test("a queued uncaptured intent cannot cross a known backend generation change", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/write-a")) return gate.promise;
    if (req.url.endsWith("/write-b")) return { revision: 9, processGeneration: 100 };
    throw new Error(`unexpected request ${req.url}`);
  });
  const control = useControlPlaneStore();
  const first = control.runLocalMutation("a", (expectation) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  const second = control.runLocalMutation("b", (expectation) => requestV3("/write-b", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  // The backend restarted while the second write was queued.
  control.sync({ revision: 30, processGeneration: 100 });
  gate.resolve(new Response(JSON.stringify({ revision: 31, processGeneration: 100 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await first;
  await assert.rejects(second, (error: unknown) => error instanceof DashboardConflictError);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["write-a"]);
});

test("a reset during the first-token refresh cannot dispatch the queued write", async () => {
  setupControlPlane(7, 99);
  const control = useControlPlaneStore();
  control.reset();
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const pending = control.runLocalMutation("a", (expectation) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  // The initial GET /contract is in flight; logout lands before it answers.
  control.reset();
  gate.resolve(new Response(JSON.stringify({ revision: 8, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await assert.rejects(pending, isLocalMutationCancelled);
  // The pre-reset contract response did not repopulate the new session's
  // tokens, and no write was dispatched.
  assert.equal(control.hasTokens(), false);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["contract"]);
});

test("a new session does not wait on the prior session's in-flight local write", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/write-old")) return gate.promise;
    if (req.url.endsWith("/write-new")) return { revision: 9, processGeneration: 99 };
    throw new Error(`unexpected request ${req.url}`);
  });
  const control = useControlPlaneStore();
  const old = control.runLocalMutation("old", (expectation) => requestV3("/write-old", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  control.reset();
  control.sync({ revision: 8, processGeneration: 99 });
  let wroteNew = false;
  const current = control.runLocalMutation("new", async (expectation) => {
    wroteNew = true;
    return requestV3("/write-new", { method: "POST", body: withExpectation({}, expectation) });
  });
  await flush();
  // The new write dispatched without waiting for the old request to settle.
  assert.equal(wroteNew, true);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["write-old", "write-new"]);
  gate.resolve(new Response(JSON.stringify({ revision: 8, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await Promise.all([old, current]);
});

test("distinct operations on one account never share a promise: delete behind update rejects", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/update")) return gate.promise;
    if (req.url.endsWith("/delete")) return { revision: 9, processGeneration: 99 };
    throw new Error(`unexpected request ${req.url}`);
  });
  const control = useControlPlaneStore();
  const update = control.runLocalMutation("account:acct-1", (expectation) => requestV3("/update", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  // A delete queued behind the in-flight update must not report the update's
  // success: it rejects as busy and the DELETE is never dispatched.
  const deleteBehind = control.runLocalMutation("account:acct-1", (expectation) => requestV3("/delete", {
    method: "DELETE",
    body: withExpectation({}, expectation),
  }));
  await assert.rejects(deleteBehind, isLocalMutationBusy);
  gate.resolve(new Response(JSON.stringify({ revision: 8, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await update;
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["update"]);
  // After the update settled, the delete dispatches as its own operation.
  await control.runLocalMutation("account:acct-1", (expectation) => requestV3("/delete", {
    method: "DELETE",
    body: withExpectation({}, expectation),
  }));
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["update", "delete"]);
});

test("a captured editor expectation stays exactly captured behind the lane", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/write-a")) return gate.promise;
    if (req.url.endsWith("/write-b")) {
      // The backend only accepts the revision its last receipt published.
      if (req.body?.expectedRevision !== 8) {
        return new Response(JSON.stringify({
          message: "revision conflict",
          code: "revisionConflict",
          currentRevision: 8,
          processGeneration: 99,
        }), { status: 409, headers: { "Content-Type": "application/json" } });
      }
      return { revision: 9, processGeneration: 99, ok: true };
    }
    if (req.url.endsWith("/contract")) return { revision: 8, processGeneration: 99 };
    throw new Error(`unexpected request ${req.url}`);
  });
  const control = useControlPlaneStore();
  const first = control.runLocalMutation("a", (expectation) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  const second = control.runLocalMutation("b", (expectation) => requestV3("/write-b", {
    method: "POST",
    body: withExpectation({}, expectation),
  }), { expectedRevision: 4, processGeneration: 99 });
  gate.resolve(new Response(JSON.stringify({ revision: 8, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await first;
  await assert.rejects(second, (error: unknown) => error instanceof DashboardConflictError);
  // The captured pair was sent verbatim; the newer store tokens were not used.
  assert.deepEqual(requests[1]?.body, { expectedRevision: 4, processGeneration: 99 });
  // The 409 refreshed tokens once, but the rejected write was never replayed.
  assert.deepEqual(
    requests.map((req) => req.url.split("/").pop()),
    ["write-a", "write-b", "contract"],
  );
  assert.equal(control.revision, 8);
});

test("a captured expectation from an older backend generation is rejected without dispatch", async () => {
  setupControlPlane(7, 99);
  const requests = installFetchMock(() => {
    throw new Error("no request may be dispatched");
  });
  const control = useControlPlaneStore();
  await assert.rejects(
    control.runLocalMutation("a", async () => "unreachable", { expectedRevision: 4, processGeneration: 98 }),
    (error: unknown) => error instanceof DashboardConflictError,
  );
  assert.equal(requests.length, 0);
});

test("queued local writes never dispatch after a session reset", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/write-a")) return gate.promise;
    if (req.url.endsWith("/write-b")) return { revision: 9, processGeneration: 99 };
    throw new Error(`unexpected request ${req.url}`);
  });
  const control = useControlPlaneStore();
  const first = control.runLocalMutation("a", (expectation) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  const second = control.runLocalMutation("b", (expectation) => requestV3("/write-b", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  control.reset();
  gate.resolve(new Response(JSON.stringify({ revision: 8, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await first;
  await assert.rejects(second, isLocalMutationCancelled);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["write-a"]);
});

test("an old contract response cannot clear or replace a new session's tokens", async () => {
  setupControlPlane(7, 99);
  const control = useControlPlaneStore();
  const gate = deferred<Response>();
  installFetchMock(() => gate.promise);
  const pending = control.refresh();
  control.reset();
  control.sync({ revision: 2, processGeneration: 100 });
  gate.resolve(Response.json({ revision: 90, processGeneration: 99 }));
  await assert.rejects(pending, isLocalMutationCancelled);
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 100 });
});

test("old success and auth failures cannot alter a newly authenticated session", async () => {
  setupControlPlane(7, 99);
  const control = useControlPlaneStore();
  const success = deferred<Response>();
  const unauthorized = deferred<Response>();
  installFetchMock(({ url }) => url.endsWith("/old-success") ? success.promise : unauthorized.promise);
  const events: string[] = [];
  window.dispatchEvent = (event: Event) => { events.push(event.type); return true; };
  const oldSuccess = requestV3("/old-success");
  const oldUnauthorized = requestV3("/old-unauthorized");
  const rejected = assert.rejects(oldUnauthorized);
  control.reset();
  control.sync({ revision: 2, processGeneration: 100 });
  success.resolve(Response.json({ revision: 90, processGeneration: 99 }));
  unauthorized.resolve(new Response("", { status: 401 }));
  await Promise.all([oldSuccess, rejected]);
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 100 });
  assert.deepEqual(events, []);
});

test("a late old-generation receipt cannot revive intents queued before a restart", async () => {
  setupControlPlane(7, 99);
  const control = useControlPlaneStore();
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const first = control.runLocalMutation("a", () => requestV3("/first", { method: "POST" }));
  const queued = control.runLocalMutation("b", async () => "must not dispatch");
  const rejected = assert.rejects(queued, DashboardConflictError);
  await flush();
  control.sync({ revision: 1, processGeneration: 100 });
  gate.resolve(Response.json({ revision: 8, processGeneration: 99 }));
  await Promise.all([first, rejected]);
  assert.equal(requests.length, 1);
});
