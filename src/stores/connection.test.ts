import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useConnectionStore, type CommittedKeyWrite } from "./connection.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useSessionStore } from "./session.ts";

/**
 * Connection write completion is the mutation ack. Follow-up plaintext GET is
 * revalidation only: it must not decide write success, start after reset, or
 * restore a revoked secret.
 */

interface DeferredCall {
  url: string;
  method: string;
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

async function waitForCalls(calls: DeferredCall[], count: number): Promise<void> {
  for (let i = 0; i < 200 && calls.length < count; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(calls.length, count, `expected ${count} fetch calls, saw ${calls.length}`);
}

async function waitForMethod(calls: DeferredCall[], method: string, fromIndex = 0): Promise<DeferredCall> {
  for (let i = 0; i < 200; i++) {
    const found = calls.slice(fromIndex).find((call) => call.method === method);
    if (found) return found;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail(`expected a ${method} request from ${fromIndex}, saw ${calls.map((call) => call.method).join(",")}`);
}

async function flush(ticks = 24): Promise<void> {
  for (let i = 0; i < ticks; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
}

function trackPromise<T>(promise: Promise<T>): {
  status: () => "pending" | "fulfilled" | "rejected";
  value: () => T | undefined;
} {
  let status: "pending" | "fulfilled" | "rejected" = "pending";
  let value: T | undefined;
  void promise.then(
    (next) => {
      status = "fulfilled";
      value = next;
    },
    () => {
      status = "rejected";
    },
  );
  return { status: () => status, value: () => value };
}

function freshPinia(): void {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
}

function subKey(id: string, name: string, value: string, enabled = true): object {
  return { id, name, enabled, value };
}

function connectionBody(
  primaryKey: string,
  revision: number,
  subKeys: object[] = [],
): object {
  return {
    gatewayPort: 9042,
    clientRootUrl: "",
    primaryKey,
    subKeys,
    revision,
    processGeneration: 99,
  };
}

async function seedConnection(
  store: ReturnType<typeof useConnectionStore>,
  calls: DeferredCall[],
  primaryKey = "primary-live",
  subKeys: object[] = [subKey("sub-1", "Laptop", "sub-live")],
): Promise<void> {
  const pending = store.load();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(connectionBody(primaryKey, 7, subKeys));
  await pending;
}

function writes(calls: DeferredCall[]): string[] {
  return calls.filter((call) => call.method !== "GET").map((call) => call.method);
}

function connectionGets(calls: DeferredCall[]): DeferredCall[] {
  return calls.filter((call) => call.method === "GET" && call.url.includes("/connection"));
}

function asCommitted<T>(value: unknown): CommittedKeyWrite<T> {
  assert.equal(typeof value, "object");
  assert.ok(value);
  const write = value as CommittedKeyWrite<T>;
  assert.equal(write.committed, true);
  assert.equal(typeof write.revalidation.then, "function");
  return write;
}

function secretUsable(
  info: { primary_key: string; sub_keys: Array<{ value: string }> } | null,
  secret: string,
): boolean {
  if (!info) return false;
  return info.primary_key === secret || info.sub_keys.some((entry) => entry.value === secret);
}

async function ackThenFailFollowupGet(
  calls: DeferredCall[],
  started: number,
): Promise<void> {
  await flush();
  const followups = calls.slice(started).filter((call) => call.method === "GET");
  for (const call of followups) {
    call.reject(new Error("read unavailable"));
  }
  await flush();
}

test("create, update, delete, and sub-Key rotation do not GET or commit after session reset", async () => {
  const cases: Array<{
    name: string;
    method: string;
    run: (store: ReturnType<typeof useConnectionStore>) => Promise<unknown>;
  }> = [
    { name: "createKey", method: "POST", run: (store) => store.createKey("Phone") },
    { name: "updateKey", method: "PATCH", run: (store) => store.updateKey("sub-1", { enabled: false }) },
    { name: "deleteKey", method: "DELETE", run: (store) => store.deleteKey("sub-1") },
    { name: "regenerateKey", method: "POST", run: (store) => store.regenerateKey("sub-1") },
  ];

  const failures: string[] = [];
  for (const item of cases) {
    freshPinia();
    const calls = installDeferredFetch();
    const store = useConnectionStore();
    await seedConnection(store, calls);
    const started = calls.length;
    const pending = item.run(store);
    const tracked = trackPromise(pending);
    const write = await waitForMethod(calls, item.method, started);
    useSessionStore().dropSession();
    write.resolve({ revision: 8, processGeneration: 99 });
    await flush();
    try {
      assert.equal(
        connectionGets(calls.slice(started)).length,
        0,
        `${item.name}: reset must not start a plaintext GET`,
      );
      assert.deepEqual(
        writes(calls.slice(started)),
        [item.method],
        `${item.name}: only the in-flight write remains`,
      );
      assert.equal(store.info, null, `${item.name}: reset store stays empty`);
      assert.notEqual(tracked.status(), "pending", `${item.name}: caller is released`);
    } catch (error) {
      failures.push(error instanceof Error ? error.message : String(error));
    }
  }
  assert.deepEqual(failures, [], failures.join("\n"));
});

test("create ack completes independently of revalidation and does not issue a second write", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  await seedConnection(store, calls, "primary-live", []);
  const started = calls.length;
  const pending = store.createKey("Laptop");
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "POST", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();

  assert.equal(tracked.status(), "fulfilled", "create ack is independent of revalidation");
  const receipt = asCommitted(tracked.value());
  assert.equal(receipt.value, undefined);
  assert.equal(writes(calls.slice(started)).length, 1, "ack must not retry the create write");
  await ackThenFailFollowupGet(calls, started);
  assert.equal(await receipt.revalidation, "unavailable");
  assert.equal(writes(calls.slice(started)).length, 1);
  assert.equal(tracked.status(), "fulfilled");
  assert.ok(store.info, "an active session keeps its connection resource");
});

test("toggle ack stays committed when the follow-up GET fails", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  await seedConnection(store, calls);
  const started = calls.length;
  const pending = store.updateKey("sub-1", { enabled: false });
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "PATCH", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled", "update ack is independent of revalidation");
  const receipt = asCommitted(tracked.value());
  await ackThenFailFollowupGet(calls, started);
  assert.equal(await receipt.revalidation, "unavailable");

  assert.equal(tracked.status(), "fulfilled");
  assert.equal(store.info?.sub_keys.find((entry) => entry.id === "sub-1")?.enabled, false);
  assert.equal(secretUsable(store.info, "sub-live"), true, "toggle does not rotate the secret");
});

test("delete ack removes the Key locally when the follow-up GET fails", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  await seedConnection(store, calls);
  const started = calls.length;
  const pending = store.deleteKey("sub-1");
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "DELETE", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled", "delete ack is independent of revalidation");
  const receipt = asCommitted(tracked.value());
  await ackThenFailFollowupGet(calls, started);
  assert.equal(await receipt.revalidation, "unavailable");

  assert.equal(tracked.status(), "fulfilled");
  assert.equal(store.info?.sub_keys.some((entry) => entry.id === "sub-1"), false);
  assert.equal(secretUsable(store.info, "sub-live"), false);
});

test("sub-Key rotation ack revokes the old secret when the follow-up GET fails", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  await seedConnection(store, calls);
  const started = calls.length;
  const pending = store.regenerateKey("sub-1");
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "POST", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled", "rotation ack is independent of revalidation");
  const receipt = asCommitted(tracked.value());
  await ackThenFailFollowupGet(calls, started);
  assert.equal(await receipt.revalidation, "unavailable");

  assert.equal(tracked.status(), "fulfilled");
  assert.equal(secretUsable(store.info, "sub-live"), false, "revoked sub-Key secret is unusable");
  assert.ok(store.info, "rotation failure to read plaintext does not drop the session resource");
  assert.equal(writes(calls.slice(started)).length, 1, "recovery must not repeat the rotate write");
});

test("primary rotation ack revokes the old secret when the follow-up GET fails", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  await seedConnection(store, calls, "primary-live");
  const started = calls.length;
  const pending = store.regeneratePrimaryKey();
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "POST", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled", "primary rotation ack is independent of revalidation");
  const receipt = asCommitted<string>(tracked.value());
  await ackThenFailFollowupGet(calls, started);
  assert.equal(await receipt.revalidation, "unavailable");

  assert.equal(tracked.status(), "fulfilled");
  assert.equal(secretUsable(store.info, "primary-live"), false, "revoked primary secret is unusable");
  assert.ok(store.info);
  assert.equal(writes(calls.slice(started)).length, 1);
});

test("successful revalidation after ack publishes the new plaintext without a second write", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  await seedConnection(store, calls, "primary-live", [subKey("sub-1", "Laptop", "sub-live")]);
  const started = calls.length;
  const pending = store.regenerateKey("sub-1");
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "POST", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled");
  const receipt = asCommitted(tracked.value());
  assert.equal(secretUsable(store.info, "sub-live"), false);

  const followup = connectionGets(calls.slice(started)).at(-1);
  assert.ok(followup, "revalidation may read the rotated list");
  followup.resolve(connectionBody("primary-live", 8, [subKey("sub-1", "Laptop", "sub-rotated")]));
  assert.equal(await receipt.revalidation, "loaded");

  assert.equal(tracked.status(), "fulfilled");
  assert.equal(secretUsable(store.info, "sub-live"), false);
  assert.equal(secretUsable(store.info, "sub-rotated"), true);
  assert.equal(writes(calls.slice(started)).length, 1);
});

test("uncached update, delete, and sub-Key rotation fulfill at ack when the follow-up GET rejects", async () => {
  const cases: Array<{
    name: string;
    method: string;
    run: (store: ReturnType<typeof useConnectionStore>) => Promise<unknown>;
  }> = [
    { name: "updateKey", method: "PATCH", run: (store) => store.updateKey("sub-1", { enabled: false }) },
    { name: "deleteKey", method: "DELETE", run: (store) => store.deleteKey("sub-1") },
    { name: "regenerateKey", method: "POST", run: (store) => store.regenerateKey("sub-1") },
  ];

  const failures: string[] = [];
  for (const item of cases) {
    freshPinia();
    const calls = installDeferredFetch();
    const store = useConnectionStore();
    const pending = item.run(store);
    const tracked = trackPromise(pending);
    const write = await waitForMethod(calls, item.method);
    write.resolve({ revision: 8, processGeneration: 99 });
    await flush();
    try {
      assert.equal(tracked.status(), "fulfilled", `${item.name}: uncached ack is independent of GET`);
      const receipt = asCommitted(tracked.value());
      assert.equal(writes(calls).length, 1, `${item.name}: no replay write`);
      const followup = connectionGets(calls).at(-1);
      assert.ok(followup, `${item.name}: one read-back GET is allowed`);
      followup.reject(new Error("read unavailable"));
      assert.equal(await receipt.revalidation, "unavailable");
      assert.equal(writes(calls).length, 1, `${item.name}: GET failure must not replay the write`);
      assert.equal(connectionGets(calls).length, 1, `${item.name}: no extra GET after reject`);
    } catch (error) {
      failures.push(error instanceof Error ? error.message : String(error));
    }
  }
  assert.deepEqual(failures, [], failures.join("\n"));
});

test("uncached primary rotation fulfills at ack and does not double-GET or replay when read-back rejects", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  const pending = store.regeneratePrimaryKey();
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "POST");
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled", "uncached primary rotation completes at the POST ack");
  const receipt = asCommitted<string>(tracked.value());
  assert.equal(receipt.value, undefined);
  assert.equal(writes(calls).length, 1);
  assert.ok(connectionGets(calls).length <= 1, "uncached primary rotation must not issue the legacy double GET before ack");
  const followup = connectionGets(calls).at(-1);
  assert.ok(followup);
  followup.reject(new Error("read unavailable"));
  assert.equal(await receipt.revalidation, "unavailable");
  assert.equal(writes(calls).length, 1);
  assert.equal(connectionGets(calls).length, 1, "rejected read-back must not start a second GET");
  assert.equal(secretUsable(store.info, "primary-live"), false);
});

test("uncached create fulfills at ack; an ambiguous new-id list is unavailable and never replays the write", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();
  const pending = store.createKey("Laptop");
  const tracked = trackPromise(pending);
  await waitForCalls(calls, 1);
  assert.equal(calls[0]!.method, "GET");
  const superseded = store.load();
  await waitForCalls(calls, 2);
  calls[0]!.resolve(connectionBody("primary-live", 7, []));
  const write = await waitForMethod(calls, "POST", 2);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled", "create ack is independent of the identifying GET");
  const receipt = asCommitted(tracked.value());
  assert.equal(receipt.value, undefined);
  assert.equal(writes(calls).length, 1);

  const followup = connectionGets(calls.slice(2)).at(-1);
  assert.ok(followup, "identifying the created Key is a read-back");
  followup.resolve(connectionBody("primary-live", 8, [
    subKey("sub-a", "A", "secret-a"),
    subKey("sub-b", "B", "secret-b"),
  ]));
  assert.equal(await receipt.revalidation, "unavailable");
  assert.equal(receipt.value, undefined, "ambiguous new ids are not guessed");
  assert.equal(writes(calls).length, 1, "must not POST create again");

  calls[1]!.resolve(connectionBody("primary-live", 7, []));
  await superseded;
});
