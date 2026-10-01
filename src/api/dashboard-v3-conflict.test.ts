import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import {
  DASHBOARD_AUTH_REQUIRED_EVENT,
  DASHBOARD_GONE_EVENT,
  DashboardAuthError,
  DashboardConflictError,
  DashboardGoneError,
  isRevisionConflict,
  requestV3,
  requestV4,
  setControlRevisionSink,
  type ControlPlaneTokens,
} from "./dashboard-v3.ts";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void } {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

function collectDashboardEvents(): string[] {
  const events: string[] = [];
  const collector = new EventTarget();
  const record = (event: Event) => { events.push(event.type); };
  collector.addEventListener(DASHBOARD_AUTH_REQUIRED_EVENT, record);
  collector.addEventListener(DASHBOARD_GONE_EVENT, record);
  window.dispatchEvent = (event: Event) => collector.dispatchEvent(event);
  return events;
}

function errorEnvelope(status: number, processGeneration: number): Response {
  return new Response(JSON.stringify({
    code: status === 410 ? "gone" : "unauthorized",
    message: "request failed",
    currentRevision: 99,
    processGeneration,
  }), { status, headers: { "Content-Type": "application/json" } });
}

function trackDashboardGlobals(context: TestContext): void {
  const windowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");
  const fetchDescriptor = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  context.after(() => {
    setControlRevisionSink(null);
    if (windowDescriptor) Object.defineProperty(globalThis, "window", windowDescriptor);
    else Reflect.deleteProperty(globalThis, "window");
    if (fetchDescriptor) Object.defineProperty(globalThis, "fetch", fetchDescriptor);
    else Reflect.deleteProperty(globalThis, "fetch");
  });
}

function deferredSignal<T>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void } {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

/**
 * Headers and status are available as soon as fetch resolves. `response.text()`
 * stays pending until `release`, which is the gap after the 410 is known and
 * before its envelope can be parsed.
 */
function controlledErrorResponse(status: number): {
  response: Response;
  consumed: Promise<void>;
  release: (body: object | string) => void;
} {
  const consumed = deferredSignal<void>();
  const payload = deferredSignal<string>();
  const response = new Response(new ReadableStream({
    start(controller) {
      consumed.resolve();
      void payload.promise.then((text) => {
        if (text.length > 0) controller.enqueue(new TextEncoder().encode(text));
        controller.close();
      });
    },
  }), {
    status,
    statusText: status === 410 ? "Gone" : "Error",
    headers: { "Content-Type": "application/json" },
  });
  return {
    response,
    consumed: consumed.promise,
    release: (body) => {
      payload.resolve(typeof body === "string" ? body : JSON.stringify(body));
    },
  };
}

function goneBody(processGeneration: number, currentRevision = 99): object {
  return {
    code: "gone",
    message: "request failed",
    currentRevision,
    processGeneration,
  };
}

function goneCount(events: string[]): number {
  return events.filter((type) => type === DASHBOARD_GONE_EVENT).length;
}

async function settleGone(pending: Promise<unknown>): Promise<DashboardGoneError> {
  try {
    await pending;
  } catch (error) {
    assert.ok(error instanceof DashboardGoneError);
    return error;
  }
  throw new Error("fixture: 410 resolved");
}

test("Dashboard V3 transport maps revisionConflict and publishes fresh CAS tokens", async (context) => {
  const windowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");
  const fetchDescriptor = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  context.after(() => {
    setControlRevisionSink(null);
    if (windowDescriptor) Object.defineProperty(globalThis, "window", windowDescriptor);
    else Reflect.deleteProperty(globalThis, "window");
    if (fetchDescriptor) Object.defineProperty(globalThis, "fetch", fetchDescriptor);
    else Reflect.deleteProperty(globalThis, "fetch");
  });

  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      location: { pathname: "/dashboard/" },
      dispatchEvent: () => true,
    },
  });
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: async () => new Response(JSON.stringify({
      code: "revisionConflict",
      message: "stale mutation",
      currentRevision: 42,
      processGeneration: 7,
    }), {
      status: 409,
      headers: { "Content-Type": "application/json" },
    }),
  });

  let published: ControlPlaneTokens | null = null;
  setControlRevisionSink((tokens) => { published = tokens; });

  await assert.rejects(
    requestV3("/settings"),
    (error: unknown) => {
      assert.ok(error instanceof DashboardConflictError);
      assert.equal(error.code, "revisionConflict");
      assert.equal(error.currentRevision, 42);
      assert.equal(error.processGeneration, 7);
      assert.ok(isRevisionConflict(error));
      return true;
    },
  );
  assert.deepEqual(published, { revision: 42, processGeneration: 7 });
  assert.equal(published !== null && "pricingRevision" in published, false);
});

test("a deferred 409 keeps the conflict metadata and does not republish a superseded process", { timeout: 5_000 }, async (context) => {
  const windowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");
  const fetchDescriptor = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  context.after(() => {
    setControlRevisionSink(null);
    if (windowDescriptor) Object.defineProperty(globalThis, "window", windowDescriptor);
    else Reflect.deleteProperty(globalThis, "window");
    if (fetchDescriptor) Object.defineProperty(globalThis, "fetch", fetchDescriptor);
    else Reflect.deleteProperty(globalThis, "fetch");
  });

  // Process 8 started the write. Process 3 was observed before the 409 returned.
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const gate = deferred<Response>();
  const requests = installFetchMock(() => gate.promise);
  const pending = requestV4("/stale-conflict", { method: "POST" });
  await Promise.resolve();
  control.sync({ revision: 2, processGeneration: 3 });
  const rejected = assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof DashboardConflictError);
    assert.equal(error.status, 409);
    assert.equal(error.code, "revisionConflict");
    assert.equal(error.currentRevision, 99);
    assert.equal(error.processGeneration, 8);
    assert.ok(isRevisionConflict(error));
    return true;
  });
  gate.resolve(new Response(JSON.stringify({
    code: "revisionConflict",
    message: "stale mutation",
    currentRevision: 99,
    processGeneration: 8,
  }), { status: 409, headers: { "Content-Type": "application/json" } }));
  await rejected;
  assert.deepEqual(control.expectation(), { expectedRevision: 2, processGeneration: 3 });
  assert.equal(requests.length, 1);
  assert.equal(requests[0]?.method, "POST");
});

test("a deferred 401 and 410 from the origin process do not emit global events", { timeout: 5_000 }, async (context) => {
  const windowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");
  const fetchDescriptor = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  context.after(() => {
    setControlRevisionSink(null);
    if (windowDescriptor) Object.defineProperty(globalThis, "window", windowDescriptor);
    else Reflect.deleteProperty(globalThis, "window");
    if (fetchDescriptor) Object.defineProperty(globalThis, "fetch", fetchDescriptor);
    else Reflect.deleteProperty(globalThis, "fetch");
  });

  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const unauthorized = deferred<Response>();
  const gone = deferred<Response>();
  const requests = installFetchMock(({ url }) => (
    url.endsWith("/stale-unauthorized") ? unauthorized.promise : gone.promise
  ));
  const events = collectDashboardEvents();
  const pendingUnauthorized = requestV4("/stale-unauthorized");
  const pendingGone = requestV4("/stale-gone");
  await Promise.resolve();
  control.sync({ revision: 2, processGeneration: 3 });
  const unauthorizedRejected = assert.rejects(
    pendingUnauthorized,
    (error: unknown) => error instanceof DashboardAuthError,
  );
  const goneRejected = assert.rejects(pendingGone, (error: unknown) => {
    assert.ok(error instanceof DashboardGoneError);
    assert.equal(error.status, 410);
    assert.equal(error.code, "gone");
    assert.equal(error.path, "/stale-gone");
    assert.equal(error.currentRevision, 99);
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

test("a 410 body naming the origin process does not emit after a newer process is current", { timeout: 5_000 }, async (context) => {
  trackDashboardGlobals(context);
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const gone = controlledErrorResponse(410);
  const requests = installFetchMock(() => gone.response);
  const events = collectDashboardEvents();
  const pending = requestV4("/body-stale-gone");
  await gone.consumed;
  control.sync({ revision: 2, processGeneration: 3 });
  gone.release(goneBody(8));
  const error = await settleGone(pending);
  assert.deepEqual({
    status: error.status,
    code: error.code,
    path: error.path,
    currentRevision: error.currentRevision,
    processGeneration: error.processGeneration,
    gone: goneCount(events),
    expectation: control.expectation(),
    method: requests[0]?.method ?? null,
    requests: requests.length,
  }, {
    status: 410,
    code: "gone",
    path: "/body-stale-gone",
    currentRevision: 99,
    processGeneration: 8,
    gone: 0,
    expectation: { expectedRevision: 2, processGeneration: 3 },
    method: "GET",
    requests: 1,
  });
});

test("an unbound 410 body does not emit or publish after another process becomes current", { timeout: 5_000 }, async (context) => {
  trackDashboardGlobals(context);
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  control.reset();
  const gone = controlledErrorResponse(410);
  const requests = installFetchMock(() => gone.response);
  const events = collectDashboardEvents();
  const pending = requestV4("/body-unbound-gone");
  await gone.consumed;
  control.sync({ revision: 2, processGeneration: 3 });
  gone.release(goneBody(8));
  const error = await settleGone(pending);
  assert.deepEqual({
    status: error.status,
    code: error.code,
    path: error.path,
    currentRevision: error.currentRevision,
    processGeneration: error.processGeneration,
    gone: goneCount(events),
    expectation: control.expectation(),
    requests: requests.length,
  }, {
    status: 410,
    code: "gone",
    path: "/body-unbound-gone",
    currentRevision: 99,
    processGeneration: 8,
    gone: 0,
    expectation: { expectedRevision: 2, processGeneration: 3 },
    requests: 1,
  });
});

test("replacing the revision sink while a 410 body is pending suppresses publication and gone", { timeout: 5_000 }, async (context) => {
  trackDashboardGlobals(context);
  setupControlPlane(1, 8);
  const original = useControlPlaneStore();
  const gone = controlledErrorResponse(410);
  const requests = installFetchMock(() => gone.response);
  const events = collectDashboardEvents();
  const pending = requestV4("/body-replaced-sink");
  await gone.consumed;
  setActivePinia(createPinia());
  const replacement = useControlPlaneStore();
  replacement.sync({ revision: 2, processGeneration: 3 });
  gone.release(goneBody(8));
  const error = await settleGone(pending);
  assert.deepEqual({
    status: error.status,
    path: error.path,
    processGeneration: error.processGeneration,
    gone: goneCount(events),
    original: original.expectation(),
    replacement: replacement.expectation(),
    requests: requests.length,
  }, {
    status: 410,
    path: "/body-replaced-sink",
    processGeneration: 8,
    gone: 0,
    original: { expectedRevision: 1, processGeneration: 8 },
    replacement: { expectedRevision: 2, processGeneration: 3 },
    requests: 1,
  });
});

test("a 410 body that names the process current after the read emits gone", { timeout: 5_000 }, async (context) => {
  trackDashboardGlobals(context);
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const gone = controlledErrorResponse(410);
  const requests = installFetchMock(() => gone.response);
  const events = collectDashboardEvents();
  const pending = requestV4("/body-current-gone");
  await gone.consumed;
  control.sync({ revision: 2, processGeneration: 3 });
  gone.release(goneBody(3, 5));
  const error = await settleGone(pending);
  assert.deepEqual({
    status: error.status,
    code: error.code,
    path: error.path,
    currentRevision: error.currentRevision,
    processGeneration: error.processGeneration,
    gone: goneCount(events),
    expectation: control.expectation(),
    requests: requests.length,
  }, {
    status: 410,
    code: "gone",
    path: "/body-current-gone",
    currentRevision: 5,
    processGeneration: 3,
    gone: 1,
    expectation: { expectedRevision: 5, processGeneration: 3 },
    requests: 1,
  });
});

test("a superseded 410 cannot publish a new process and then emit gone for it", { timeout: 5_000 }, async (context) => {
  trackDashboardGlobals(context);
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const gone = controlledErrorResponse(410);
  const requests = installFetchMock(() => gone.response);
  const events = collectDashboardEvents();
  const pending = requestV4("/body-installed-gone");
  await gone.consumed;
  control.sync({ revision: 2, processGeneration: 3 });
  gone.release(goneBody(9));
  const error = await settleGone(pending);
  assert.deepEqual({
    status: error.status,
    path: error.path,
    currentRevision: error.currentRevision,
    processGeneration: error.processGeneration,
    gone: goneCount(events),
    expectation: control.expectation(),
    requests: requests.length,
  }, {
    status: 410,
    path: "/body-installed-gone",
    currentRevision: 99,
    processGeneration: 9,
    gone: 0,
    expectation: { expectedRevision: 99, processGeneration: 9 },
    requests: 1,
  });
});

test("a 410 whose process is still current emits gone after its body is read", { timeout: 5_000 }, async (context) => {
  trackDashboardGlobals(context);
  setupControlPlane(1, 8);
  const control = useControlPlaneStore();
  const gone = controlledErrorResponse(410);
  const requests = installFetchMock(() => gone.response);
  const events = collectDashboardEvents();
  const pending = requestV4("/body-live-gone");
  await gone.consumed;
  gone.release(goneBody(8, 1));
  const error = await settleGone(pending);
  assert.deepEqual({
    status: error.status,
    code: error.code,
    path: error.path,
    currentRevision: error.currentRevision,
    processGeneration: error.processGeneration,
    gone: goneCount(events),
    expectation: control.expectation(),
    requests: requests.length,
  }, {
    status: 410,
    code: "gone",
    path: "/body-live-gone",
    currentRevision: 1,
    processGeneration: 8,
    gone: 1,
    expectation: { expectedRevision: 1, processGeneration: 8 },
    requests: 1,
  });
});
