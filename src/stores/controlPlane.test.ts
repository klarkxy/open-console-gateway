import assert from "node:assert/strict";
import test from "node:test";
import {
  DASHBOARD_AUTH_REQUIRED_EVENT,
  DASHBOARD_GONE_EVENT,
  DashboardAuthError,
  DashboardConflictError,
  DashboardGoneError,
  requestV3,
  requestV4,
  withExpectation,
} from "../api/dashboard-v3.ts";
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

/** Real EventTarget listeners for the transport's window events. */
function collectDashboardEvents(): string[] {
  const events: string[] = [];
  const collector = new EventTarget();
  const record = (event: Event) => { events.push(event.type); };
  collector.addEventListener(DASHBOARD_AUTH_REQUIRED_EVENT, record);
  collector.addEventListener(DASHBOARD_GONE_EVENT, record);
  window.dispatchEvent = (event: Event) => collector.dispatchEvent(event);
  return events;
}

function errorEnvelope(status: number, processGeneration: number, currentRevision = 99): Response {
  return new Response(JSON.stringify({
    code: status === 410 ? "gone" : "unauthorized",
    message: "request failed",
    currentRevision,
    processGeneration,
  }), { status, headers: { "Content-Type": "application/json" } });
}

function contractResponse(revision: number, processGeneration: number): Response {
  return Response.json({ pricingRevision: "hist", processGeneration, revision });
}

test("runMutation sends a captured expectation instead of the store's later tokens", async () => {
  setupControlPlane(4, 11, "p1");
  const control = useControlPlaneStore();
  control.sync({ revision: 8, processGeneration: 11 });

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

test("a mutation expectation carries revision and process generation only", () => {
  setupControlPlane(7, 11, "hist-price");
  const expectation = useControlPlaneStore().expectation();
  assert.deepEqual(expectation, { expectedRevision: 7, processGeneration: 11 });
  assert.equal("pricingRevision" in expectation, false);
});

test("a deferred requestV4 from the process that started it cannot replace a process observed since", { timeout: 5_000 }, async () => {
  // Process 8 started the read. Process 3 was observed before that read returned.
  setupControlPlane(1, 8);
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const control = useControlPlaneStore();
  const pending = requestV4<{ revision: number; processGeneration: number }>("/observed");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  gate.resolve(Response.json({ revision: 99, processGeneration: 8 }));
  assert.deepEqual(await pending, { revision: 99, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.deepEqual(requests.map((request) => request.url.split("/").pop()), ["observed"]);
});

test("a deferred requestV4 whose process matches the current one is published even if the request started earlier", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const control = useControlPlaneStore();
  const pending = requestV4<{ revision: number; processGeneration: number }>("/current-match");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  gate.resolve(Response.json({ revision: 5, processGeneration: 3 }));
  assert.deepEqual(await pending, { revision: 5, processGeneration: 3 });
  assert.deepEqual(control.expectation(), { expectedRevision: 5, processGeneration: 3 });
  assert.equal(requests.length, 1);
});

test("a requestV4 that is still the current process may adopt a different process from its response", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const control = useControlPlaneStore();
  const pending = requestV4<{ revision: number; processGeneration: number }>("/new-process");
  await flush();
  gate.resolve(Response.json({ revision: 2, processGeneration: 3 }));
  assert.deepEqual(await pending, { revision: 2, processGeneration: 3 });
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.equal(requests.length, 1);
});

test("the same process accepts a higher revision and ignores a lower one", { timeout: 5_000 }, async () => {
  setupControlPlane(5, 8);
  const control = useControlPlaneStore();
  control.sync({ revision: 3, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 5, processGeneration: 8 });
  control.sync({ revision: 6, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 6, processGeneration: 8 });

  const higher = deferred<Response>();
  const lower = deferred<Response>();
  const requests = installFetchMock(({ url }) => (url.endsWith("/higher") ? higher.promise : lower.promise));
  const pendingHigher = requestV4<{ revision: number; processGeneration: number }>("/higher");
  higher.resolve(Response.json({ revision: 9, processGeneration: 8 }));
  await pendingHigher;
  assert.deepEqual(control.expectation(), { expectedRevision: 9, processGeneration: 8 });
  const pendingLower = requestV4<{ revision: number; processGeneration: number }>("/lower");
  lower.resolve(Response.json({ revision: 4, processGeneration: 8 }));
  assert.deepEqual(await pendingLower, { revision: 4, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 9, processGeneration: 8 });
  assert.equal(requests.length, 2);
});

test("a requestV4 that started before any process cannot resurrect a superseded identity", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  control.reset();
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const pending = requestV4<{ revision: number; processGeneration: number }>("/unknown-origin");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  gate.resolve(Response.json({ revision: 99, processGeneration: 8 }));
  assert.deepEqual(await pending, { revision: 99, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.equal(requests.length, 1);
});

test("a requestV4 that started before any process becomes current when nothing else has", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  control.reset();
  const gate = deferred<Response>();
  installFetchMock(() => gate.promise);
  const pending = requestV4<{ revision: number; processGeneration: number }>("/first-process");
  gate.resolve(Response.json({ revision: 1, processGeneration: 8 }));
  assert.deepEqual(await pending, { revision: 1, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 1, processGeneration: 8 });
});

test("session reset still fences a late 200, 401, and 409 from the replaced sink", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const success = deferred<Response>();
  const unauthorized = deferred<Response>();
  const conflict = deferred<Response>();
  installFetchMock(({ url }) => {
    if (url.endsWith("/old-success")) return success.promise;
    if (url.endsWith("/old-unauthorized")) return unauthorized.promise;
    if (url.endsWith("/old-conflict")) return conflict.promise;
    throw new Error(`unexpected request ${url}`);
  });
  const events: string[] = [];
  window.dispatchEvent = (event: Event) => {
    events.push(event.type);
    return true;
  };
  const oldSuccess = requestV4<{ revision: number; processGeneration: number }>("/old-success");
  const oldUnauthorized = requestV4("/old-unauthorized");
  const oldConflict = requestV4("/old-conflict");
  const unauthorizedRejected = assert.rejects(oldUnauthorized, (error: unknown) => error instanceof DashboardAuthError);
  const conflictRejected = assert.rejects(oldConflict, (error: unknown) => {
    assert.ok(error instanceof DashboardConflictError);
    assert.equal(error.status, 409);
    assert.equal(error.code, "revisionConflict");
    assert.equal(error.currentRevision, 99);
    assert.equal(error.processGeneration, 8);
    return true;
  });
  control.reset();
  control.sync({ revision: 2, processGeneration: 3 });
  success.resolve(Response.json({ revision: 99, processGeneration: 8 }));
  unauthorized.resolve(new Response("", { status: 401 }));
  conflict.resolve(new Response(JSON.stringify({
    code: "revisionConflict",
    message: "stale mutation",
    currentRevision: 99,
    processGeneration: 8,
  }), { status: 409, headers: { "Content-Type": "application/json" } }));
  await Promise.all([oldSuccess, unauthorizedRejected, conflictRejected]);
  assert.deepEqual(await oldSuccess, { revision: 99, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.deepEqual(events, []);
});

test("a stale origin response does not cancel or rebind a queued local write", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const oldGate = deferred<Response>();
  const writeGate = deferred<Response>();
  const requests = installFetchMock(({ url }) => {
    if (url.endsWith("/old-read")) return oldGate.promise;
    if (url.endsWith("/write-a")) return writeGate.promise;
    if (url.endsWith("/write-b")) return { revision: 4, processGeneration: 3 };
    throw new Error(`unexpected request ${url}`);
  });
  const control = useControlPlaneStore();
  const oldRead = requestV4<{ revision: number; processGeneration: number }>("/old-read");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  const first = control.runLocalMutation("a", (expectation) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  const second = control.runLocalMutation("b", (expectation) => requestV3("/write-b", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  assert.deepEqual(requests.map((request) => request.url.split("/").pop()), ["old-read", "write-a"]);
  assert.deepEqual(requests[1]?.body, { expectedRevision: 2, processGeneration: 3 });
  oldGate.resolve(Response.json({ revision: 99, processGeneration: 8 }));
  assert.deepEqual(await oldRead, { revision: 99, processGeneration: 8 });
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  writeGate.resolve(Response.json({ revision: 4, processGeneration: 3 }));
  await first;
  await second;
  assert.deepEqual(requests.map((request) => request.url.split("/").pop()), ["old-read", "write-a", "write-b"]);
  assert.deepEqual(requests[2]?.body, { expectedRevision: 4, processGeneration: 3 });
  assert.deepEqual(control.expectation(), { expectedRevision: 4, processGeneration: 3 });
});

test("a deferred 401 from the origin process does not emit the auth event after another process is current", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const events = collectDashboardEvents();
  const control = useControlPlaneStore();
  const pending = requestV4("/stale-unauthorized");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  const rejected = assert.rejects(pending, (error: unknown) => error instanceof DashboardAuthError);
  gate.resolve(errorEnvelope(401, 8));
  await rejected;
  assert.deepEqual(events, []);
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.deepEqual(requests.map((request) => request.url.split("/").pop()), ["stale-unauthorized"]);
});

test("a deferred 410 from the origin process does not emit the tombstone event after another process is current", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const events = collectDashboardEvents();
  const control = useControlPlaneStore();
  const pending = requestV4("/stale-gone");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  const rejected = assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof DashboardGoneError);
    assert.equal(error.status, 410);
    assert.equal(error.code, "gone");
    assert.equal(error.path, "/stale-gone");
    assert.equal(error.currentRevision, 99);
    assert.equal(error.processGeneration, 8);
    return true;
  });
  gate.resolve(errorEnvelope(410, 8));
  await rejected;
  assert.deepEqual(events, []);
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.deepEqual(requests.map((request) => request.url.split("/").pop()), ["stale-gone"]);
});

test("an unchanged 401 with an empty body still emits the auth event", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const events = collectDashboardEvents();
  const control = useControlPlaneStore();
  const pending = requestV4("/same-unauthorized");
  const rejected = assert.rejects(pending, (error: unknown) => error instanceof DashboardAuthError);
  gate.resolve(new Response("", { status: 401 }));
  await rejected;
  assert.deepEqual(events, [DASHBOARD_AUTH_REQUIRED_EVENT]);
  assert.deepEqual(control.expectation(), { expectedRevision: 1, processGeneration: 8 });
  assert.equal(requests.length, 1);
});

test("a 401 and a 410 that start on the current process emit their events", { timeout: 5_000 }, async () => {
  setupControlPlane(2, 3);
  const unauthorized = deferred<Response>();
  const gone = deferred<Response>();
  const requests = installFetchMock(({ url }) => (
    url.endsWith("/fresh-unauthorized") ? unauthorized.promise : gone.promise
  ));
  const events = collectDashboardEvents();
  const control = useControlPlaneStore();
  const pendingUnauthorized = requestV4("/fresh-unauthorized");
  const pendingGone = requestV4("/fresh-gone");
  const unauthorizedRejected = assert.rejects(
    pendingUnauthorized,
    (error: unknown) => error instanceof DashboardAuthError,
  );
  const goneRejected = assert.rejects(pendingGone, (error: unknown) => {
    assert.ok(error instanceof DashboardGoneError);
    assert.equal(error.status, 410);
    assert.equal(error.processGeneration, 3);
    return true;
  });
  unauthorized.resolve(new Response("", { status: 401 }));
  await unauthorizedRejected;
  assert.deepEqual(events, [DASHBOARD_AUTH_REQUIRED_EVENT]);
  gone.resolve(errorEnvelope(410, 3, 2));
  await goneRejected;
  assert.deepEqual(events, [DASHBOARD_AUTH_REQUIRED_EVENT, DASHBOARD_GONE_EVENT]);
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.equal(requests.length, 2);
});

test("a deferred 401 and 410 whose envelope names the current process still emit their events", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const unauthorized = deferred<Response>();
  const gone = deferred<Response>();
  const requests = installFetchMock(({ url }) => (
    url.endsWith("/match-unauthorized") ? unauthorized.promise : gone.promise
  ));
  const events = collectDashboardEvents();
  const control = useControlPlaneStore();
  const pendingUnauthorized = requestV4("/match-unauthorized");
  const pendingGone = requestV4("/match-gone");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  const unauthorizedRejected = assert.rejects(
    pendingUnauthorized,
    (error: unknown) => error instanceof DashboardAuthError,
  );
  const goneRejected = assert.rejects(pendingGone, (error: unknown) => {
    assert.ok(error instanceof DashboardGoneError);
    assert.equal(error.status, 410);
    assert.equal(error.currentRevision, 5);
    assert.equal(error.processGeneration, 3);
    return true;
  });
  unauthorized.resolve(errorEnvelope(401, 3, 5));
  gone.resolve(errorEnvelope(410, 3, 5));
  await Promise.all([unauthorizedRejected, goneRejected]);
  assert.equal(events.length, 2);
  assert.ok(events.includes(DASHBOARD_AUTH_REQUIRED_EVENT));
  assert.ok(events.includes(DASHBOARD_GONE_EVENT));
  assert.deepEqual(control.expectation(), { expectedRevision: 5, processGeneration: 3 });
  assert.equal(requests.length, 2);
});

test("an unbound 401 and 410 cannot invalidate the process observed while they were in flight", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  control.reset();
  const unauthorized = deferred<Response>();
  const gone = deferred<Response>();
  const requests = installFetchMock(({ url }) => (
    url.endsWith("/unbound-unauthorized") ? unauthorized.promise : gone.promise
  ));
  const events = collectDashboardEvents();
  const pendingUnauthorized = requestV4("/unbound-unauthorized");
  const pendingGone = requestV4("/unbound-gone");
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  const unauthorizedRejected = assert.rejects(
    pendingUnauthorized,
    (error: unknown) => error instanceof DashboardAuthError,
  );
  const goneRejected = assert.rejects(pendingGone, (error: unknown) => {
    assert.ok(error instanceof DashboardGoneError);
    assert.equal(error.status, 410);
    assert.equal(error.processGeneration, 8);
    return true;
  });
  unauthorized.resolve(errorEnvelope(401, 8));
  gone.resolve(errorEnvelope(410, 8));
  await Promise.all([unauthorizedRejected, goneRejected]);
  assert.deepEqual(events, []);
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.equal(requests.length, 2);
});

test("session reset still fences a late 401 and 410", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const unauthorized = deferred<Response>();
  const gone = deferred<Response>();
  const requests = installFetchMock(({ url }) => (
    url.endsWith("/reset-unauthorized") ? unauthorized.promise : gone.promise
  ));
  const events = collectDashboardEvents();
  const pendingUnauthorized = requestV4("/reset-unauthorized");
  const pendingGone = requestV4("/reset-gone");
  const unauthorizedRejected = assert.rejects(
    pendingUnauthorized,
    (error: unknown) => error instanceof DashboardAuthError,
  );
  const goneRejected = assert.rejects(pendingGone, (error: unknown) => {
    assert.ok(error instanceof DashboardGoneError);
    assert.equal(error.status, 410);
    assert.equal(error.processGeneration, 8);
    return true;
  });
  control.reset();
  control.sync({ revision: 2, processGeneration: 3 });
  unauthorized.resolve(new Response("", { status: 401 }));
  gone.resolve(errorEnvelope(410, 8));
  await Promise.all([unauthorizedRejected, goneRejected]);
  assert.deepEqual(events, []);
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.equal(requests.length, 2);
});

test("a refresh started on process 8 keeps process 3 when that old contract returns", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const contractGate = deferred<Response>();
  const firstWrite = deferred<Response>();
  const requests = installFetchMock(({ url }) => {
    if (url.endsWith("/contract")) return contractGate.promise;
    if (url.endsWith("/lane-a")) return firstWrite.promise;
    if (url.endsWith("/lane-b")) return { revision: 4, processGeneration: 3 };
    throw new Error(`unexpected request ${url}`);
  });
  const control = useControlPlaneStore();
  const pendingRefresh = control.refresh();
  await flush();
  assert.deepEqual(requests.map((request) => [request.method, request.url]), [
    ["GET", "/dashboard/api/v4/contract"],
  ]);
  control.sync({ revision: 2, processGeneration: 3 });
  const first = control.runLocalMutation("lane-a", (expectation) => requestV3("/lane-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  const second = control.runLocalMutation("lane-b", (expectation) => requestV3("/lane-b", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  await flush();
  assert.deepEqual(requests.map((request) => request.url.split("/").pop()), ["contract", "lane-a"]);
  assert.deepEqual(requests[1]?.body, { expectedRevision: 2, processGeneration: 3 });
  contractGate.resolve(contractResponse(99, 8));
  assert.deepEqual(await pendingRefresh, { expectedRevision: 2, processGeneration: 3 });
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  firstWrite.resolve(Response.json({ revision: 4, processGeneration: 3 }));
  await first;
  await second;
  assert.deepEqual(requests.map((request) => [request.method, request.url.split("/").pop()]), [
    ["GET", "contract"],
    ["POST", "lane-a"],
    ["POST", "lane-b"],
  ]);
  assert.equal(requests.filter((request) => request.url.endsWith("/contract")).length, 1);
  assert.deepEqual(requests[2]?.body, { expectedRevision: 4, processGeneration: 3 });
  assert.deepEqual(control.expectation(), { expectedRevision: 4, processGeneration: 3 });
});

test("a refresh whose contract names the current process adopts that revision", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const contractGate = deferred<Response>();
  const requests = installFetchMock(() => contractGate.promise);
  const control = useControlPlaneStore();
  const pendingRefresh = control.refresh();
  await flush();
  control.sync({ revision: 2, processGeneration: 3 });
  contractGate.resolve(contractResponse(5, 3));
  assert.deepEqual(await pendingRefresh, { expectedRevision: 5, processGeneration: 3 });
  assert.deepEqual(control.expectation(), { expectedRevision: 5, processGeneration: 3 });
  assert.deepEqual(requests.map((request) => [request.method, request.url]), [
    ["GET", "/dashboard/api/v4/contract"],
  ]);
});

test("a refresh that is still the current process adopts a different process from its contract", { timeout: 5_000 }, async () => {
  setupControlPlane(1, 8);
  const contractGate = deferred<Response>();
  const requests = installFetchMock(() => contractGate.promise);
  const control = useControlPlaneStore();
  const pendingRefresh = control.refresh();
  await flush();
  assert.equal(requests.length, 1);
  contractGate.resolve(contractResponse(2, 3));
  assert.deepEqual(await pendingRefresh, { expectedRevision: 2, processGeneration: 3 });
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.deepEqual(requests.map((request) => [request.method, request.url]), [
    ["GET", "/dashboard/api/v4/contract"],
  ]);
});
