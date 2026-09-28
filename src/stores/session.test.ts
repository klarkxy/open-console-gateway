import assert from "node:assert/strict";
import test from "node:test";
import { requestV3, withExpectation } from "../api/dashboard-v3.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { isLocalMutationCancelled, useControlPlaneStore } from "./controlPlane.ts";
import { useSessionStore } from "./session.ts";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void } {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

test("logout resets the control plane through the real session path: queued writes cancel and a late receipt cannot repopulate tokens", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/write-a")) return gate.promise;
    if (req.url.endsWith("/write-b")) return { revision: 9, processGeneration: 99 };
    if (req.url.endsWith("/auth/logout")) {
      return { authenticated: false, initialized: true, local: true, revision: 8, processGeneration: 99 };
    }
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
  const cancelled = second.then(() => false, (error: unknown) => isLocalMutationCancelled(error));
  await flush();
  const session = useSessionStore();
  await session.logout();
  assert.equal(session.phase, "login");
  // The write dispatched before logout still settles, but its pre-logout
  // receipt must not repopulate the dropped session's tokens.
  gate.resolve(new Response(JSON.stringify({ revision: 9, processGeneration: 99 }), {
    headers: { "Content-Type": "application/json" },
  }));
  await first;
  assert.equal(await cancelled, true);
  assert.equal(control.hasTokens(), false);
  // The queued write never dispatched; only the in-flight write and the
  // logout itself reached the transport.
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["write-a", "logout"]);
});

test("a 401 during the first-token refresh cancels the queued write through the real session path", async () => {
  setupControlPlane(7, 99);
  const control = useControlPlaneStore();
  const session = useSessionStore();
  session.dropSession();
  const gate = deferred<object>();
  const requests = installFetchMock(() => gate.promise);
  const pending = control.runLocalMutation("a", (expectation) => requestV3("/write-a", {
    method: "POST",
    body: withExpectation({}, expectation),
  }));
  const cancelled = pending.then(() => false, (error: unknown) => isLocalMutationCancelled(error));
  await flush();
  // The initial GET /contract is still in flight when the session dies.
  session.handleAuthRequired();
  gate.resolve({ revision: 8, processGeneration: 99 });
  assert.equal(await cancelled, true);
  // The pre-reset contract answer did not repopulate the new session's
  // tokens, and the queued write never dispatched.
  assert.equal(control.hasTokens(), false);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["contract"]);
});
