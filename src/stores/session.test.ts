import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import { requestV3, withExpectation } from "../api/dashboard-v3.ts";
import { installFetchMock, installWindowDashboard, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
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

interface DeferredCall {
  url: string;
  method: string;
  body: Record<string, unknown> | null;
  resolve: (body: object) => void;
  reject: (error: unknown) => void;
}

let openCalls: DeferredCall[] = [];

function installDeferredFetch(): DeferredCall[] {
  installWindowDashboard();
  const calls: DeferredCall[] = [];
  openCalls = calls;
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (input: string, init: RequestInit = {}) => {
      const promise = new Promise<Response>((resolvePromise, rejectPromise) => {
        calls.push({
          url: String(input),
          method: init.method ?? "GET",
          body: init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null,
          resolve: (body) => resolvePromise(new Response(
            JSON.stringify(body),
            { headers: { "Content-Type": "application/json" } },
          )),
          reject: (error) => rejectPromise(error),
        });
      });
      void promise.catch(() => undefined);
      return promise;
    },
  });
  return calls;
}

afterEach(() => {
  for (const call of openCalls) call.reject(new Error("unsettled fetch"));
  openCalls = [];
});

async function waitForUrl(calls: DeferredCall[], suffix: string, fromIndex = 0): Promise<DeferredCall> {
  for (let i = 0; i < 200; i++) {
    const found = calls.slice(fromIndex).find((call) => call.url.endsWith(suffix));
    if (found) return found;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail(`expected a request ending ${suffix} from ${fromIndex}, saw ${calls.map((call) => `${call.method} ${call.url}`).join(",")}`);
}

async function drain(ticks = 24): Promise<void> {
  for (let i = 0; i < ticks; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
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

test("dropSession increments a readonly sessionEpoch", () => {
  setupControlPlane(7, 99);
  const session = useSessionStore();
  const first = session.sessionEpoch;
  assert.equal(typeof first, "number");
  session.dropSession();
  assert.equal(session.sessionEpoch, first + 1);
  session.dropSession();
  assert.equal(session.sessionEpoch, first + 2);
});

test("a pending auth-status response cannot revive a dropped session", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response | object>();
  installFetchMock((req) => {
    if (req.url.endsWith("/auth/status")) return gate.promise;
    throw new Error(`unexpected request ${req.url}`);
  });
  const session = useSessionStore();
  const pending = session.loadStatus();
  const epoch = session.sessionEpoch;
  session.dropSession();
  gate.resolve({ authenticated: true, initialized: true, local: false, revision: 7, processGeneration: 99 });
  await pending.then(() => undefined, () => undefined);
  assert.equal(session.phase, "login");
  assert.equal(session.authenticated, false);
  if (typeof epoch === "number") {
    assert.equal(session.sessionEpoch, epoch + 1);
  }
});

test("a pending login response cannot revive a dropped session", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response | object>();
  installFetchMock((req) => {
    if (req.url.endsWith("/auth/login")) return gate.promise;
    throw new Error(`unexpected request ${req.url}`);
  });
  const session = useSessionStore();
  const pending = session.login("admin", "password123");
  await flush();
  const epoch = session.sessionEpoch;
  session.dropSession();
  gate.resolve({ authenticated: true, initialized: true, local: false, revision: 9, processGeneration: 99 });
  await pending.then(() => undefined, () => undefined);
  assert.equal(session.phase, "login");
  assert.equal(session.authenticated, false);
  assert.equal(session.sessionEpoch, epoch + 1);
});

test("a pending register response cannot revive a dropped session", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<Response | object>();
  installFetchMock((req) => {
    if (req.url.endsWith("/auth/register")) return gate.promise;
    throw new Error(`unexpected request ${req.url}`);
  });
  const session = useSessionStore();
  const pending = session.register("admin", "password123");
  await flush();
  session.dropSession();
  gate.resolve({ authenticated: true, initialized: true, local: false, revision: 9, processGeneration: 99 });
  await pending.then(() => undefined, () => undefined);
  assert.equal(session.phase, "login");
  assert.equal(session.authenticated, false);
});

test("login waiting for an authExpectation GET does not dispatch a login write after drop", async () => {
  setupControlPlane(7, 99);
  const session = useSessionStore();
  session.dropSession();
  const gate = deferred<Response | object>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/auth/status")) return gate.promise;
    if (req.url.endsWith("/auth/login")) {
      return { authenticated: true, initialized: true, local: false, revision: 9, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${req.url}`);
  });
  const pending = session.login("admin", "password123");
  await flush();
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["status"]);
  session.dropSession();
  gate.resolve({ authenticated: true, initialized: true, local: false, revision: 8, processGeneration: 99 });
  await pending.then(() => undefined, () => undefined);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["status"]);
  assert.equal(session.phase, "login");
  assert.equal(session.authenticated, false);
});

test("register waiting for an authExpectation GET does not dispatch a register write after drop", async () => {
  setupControlPlane(7, 99);
  const session = useSessionStore();
  session.dropSession();
  const gate = deferred<Response | object>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/auth/status")) return gate.promise;
    if (req.url.endsWith("/auth/register")) {
      return { authenticated: true, initialized: true, local: false, revision: 9, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${req.url}`);
  });
  const pending = session.register("admin", "password123");
  await flush();
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["status"]);
  session.dropSession();
  gate.resolve({ authenticated: false, initialized: false, local: false, revision: 8, processGeneration: 99 });
  await pending.then(() => undefined, () => undefined);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["status"]);
  assert.equal(session.authenticated, false);
});

test("logout waiting for an authExpectation GET does not dispatch a logout write after drop", async () => {
  setupControlPlane(7, 99);
  const session = useSessionStore();
  session.dropSession();
  const gate = deferred<Response | object>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/auth/status")) return gate.promise;
    if (req.url.endsWith("/auth/logout")) {
      return { authenticated: false, initialized: true, local: false, revision: 9, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${req.url}`);
  });
  const pending = session.logout();
  await flush();
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["status"]);
  session.dropSession();
  gate.resolve({ authenticated: false, initialized: true, local: false, revision: 8, processGeneration: 99 });
  await pending.then(() => undefined, () => undefined);
  assert.deepEqual(requests.map((req) => req.url.split("/").pop()), ["status"]);
  assert.equal(session.phase, "login");
});

test("a logout reply after drop and a new login cannot drop the newer session or change its suppress flag", async () => {
  setupControlPlane(7, 99);
  const logoutGate = deferred<Response | object>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/auth/logout")) return logoutGate.promise;
    if (req.url.endsWith("/auth/login")) {
      return { authenticated: true, initialized: true, local: false, revision: 10, processGeneration: 99 };
    }
    if (req.url.endsWith("/auth/status")) {
      return { authenticated: false, initialized: true, local: false, revision: 8, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${req.url}`);
  });
  const session = useSessionStore();
  const oldLogout = session.logout();
  await flush();
  assert.equal(requests.some((req) => req.url.endsWith("/auth/logout")), true);
  session.dropSession();
  await session.login("admin", "password123");
  assert.equal(session.phase, "ready");
  assert.equal(session.authenticated, true);
  const epoch = session.sessionEpoch;
  const suppress = session.suppressAuthRequired;
  logoutGate.resolve({ authenticated: false, initialized: true, local: false, revision: 9, processGeneration: 99 });
  await oldLogout.then(() => undefined, () => undefined);
  assert.equal(session.phase, "ready");
  assert.equal(session.authenticated, true);
  assert.equal(session.sessionEpoch, epoch);
  assert.equal(session.suppressAuthRequired, suppress);
});

const STALE_AUTH_ACTIONS = [
  { name: "login", path: "/auth/login", start: (session: ReturnType<typeof useSessionStore>) => session.login("admin", "password123") },
  { name: "register", path: "/auth/register", start: (session: ReturnType<typeof useSessionStore>) => session.register("admin", "password123") },
  { name: "logout", path: "/auth/logout", start: (session: ReturnType<typeof useSessionStore>) => session.logout() },
] as const;

for (const row of STALE_AUTH_ACTIONS) {
  test(`${row.name} waiting on authStatus must not POST after drop once a new session has loaded tokens`, async () => {
    // Cookie/authority boundary: a POST after drop already uses the live
    // session. Process identities compare equal as opaque values.
    const oldProcess = 99;
    const newProcess = 12;
    setupControlPlane(7, oldProcess);
    const calls = installDeferredFetch();
    const session = useSessionStore();
    const control = useControlPlaneStore();
    session.dropSession();
    const oldAction = row.start(session);
    void oldAction.then(() => undefined, () => undefined);
    const oldPrep = await waitForUrl(calls, "/auth/status");
    assert.equal(calls.some((call) => call.url.endsWith(row.path) && call.method === "POST"), false);

    session.dropSession();
    const seeded = session.loadStatus();
    const newPrep = await waitForUrl(calls, "/auth/status", 1);
    assert.notEqual(newPrep, oldPrep);
    newPrep.resolve({
      authenticated: true,
      initialized: true,
      local: false,
      revision: 4,
      processGeneration: newProcess,
    });
    await seeded;
    assert.equal(session.phase, "ready");
    assert.equal(session.authenticated, true);
    assert.equal(control.hasTokens(), true);
    assert.equal(control.processGeneration, newProcess);
    assert.notEqual(control.processGeneration, oldProcess);
    const epoch = session.sessionEpoch;
    const suppress = session.suppressAuthRequired;
    const processId = control.processGeneration;

    try {
      oldPrep.resolve({
        authenticated: false,
        initialized: true,
        local: false,
        revision: 7,
        processGeneration: oldProcess,
      });
      await drain();
      const mutations = calls.filter((call) => call.url.endsWith(row.path) && call.method === "POST");
      for (const call of mutations) {
        call.reject(new Error(`stale ${row.name} POST must not dispatch`));
      }
      await oldAction.then(() => undefined, () => undefined);
      assert.equal(
        mutations.length,
        0,
        `stale ${row.name} dispatched POST with ${JSON.stringify(mutations[0]?.body)}`,
      );
      assert.equal(session.phase, "ready");
      assert.equal(session.authenticated, true);
      assert.equal(session.sessionEpoch, epoch);
      assert.equal(session.suppressAuthRequired, suppress);
      assert.equal(control.processGeneration, processId);
    } finally {
      for (const call of calls) call.reject(new Error("unsettled fetch"));
      await oldAction.then(() => undefined, () => undefined);
    }
  });
}
