import assert from "node:assert/strict";
import { rm } from "node:fs/promises";
import { after, before, test } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { defineComponent, h, shallowRef, ssrContextKey, type Component } from "vue";
import type { BillingStatus, CreditConfiguration, CreditMeterView } from "../api/billing.ts";
import { billingBinding } from "../domain/billing.ts";
import {
  installRecordingFetch,
  jsonResponse,
  loadRetirementBundle,
  type RecordedCall,
  type RetirementBundle,
} from "../../.artifacts/frontend-logic-repair/frontend-retirement-tests/panel-host.ts";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

const UPDATED = "2026-09-21T00:00:00.000Z";
const ENDPOINT = "https://api.example.test/v1";
const NOW = Date.parse("2026-09-21T12:00:00.000Z");
const BINDING = billingBinding(UPDATED, ENDPOINT);

const renderer = createVueHostRenderer();
let bundle: RetirementBundle;

// Installed after the Vite bundle build. Native v-model calls these on the
// custom-renderer node; a missing method aborts the dialog patch.
function installHostElementMethods(): void {
  const define = (name: string, value: (...args: unknown[]) => unknown) => {
    if (Object.prototype.hasOwnProperty(name)) return;
    Object.defineProperty(Object.prototype, name, { configurable: true, writable: true, value });
  };
  define("addEventListener", () => undefined);
  define("removeEventListener", () => undefined);
  define("dispatchEvent", () => true);
  define("getRootNode", () => ({}));
}
let buildDir = "";
let lane = Promise.resolve();

function serial(run: () => Promise<void>): Promise<void> {
  const next = lane.then(run, run);
  lane = next.then(() => undefined, () => undefined);
  return next;
}

function configuration(): CreditConfiguration {
  return {
    creditsPerCurrency: 1,
    currency: "USD",
    monthly: {
      amount: 250,
      nextResetAt: "2026-10-01T16:00:00.000Z",
      renewalEndsAt: null,
      timezoneOffsetMinutes: 480,
    },
    name: "manual",
    rates: [{
      model: "gpt-4o",
      inputPerMillion: 3,
      outputPerMillion: 6,
      cacheReadPerMillion: null,
      cacheWritePerMillion: null,
    }],
    sourceUrl: null,
  };
}

function meter(pendingRequests = 0): CreditMeterView {
  return {
    activeGranted: 80,
    buckets: [
      {
        id: "manual",
        kind: "manual",
        label: "manual-balance",
        granted: 100,
        remaining: 40,
        startsAt: "2026-09-01T00:00:00.000Z",
        expiresAt: null,
      },
      {
        id: "old-grant",
        kind: "top_up",
        label: "grant-archive",
        granted: 10,
        remaining: 1,
        startsAt: "2026-01-01T00:00:00.000Z",
        expiresAt: "2026-02-01T00:00:00.000Z",
      },
    ],
    configuration: configuration(),
    credentialId: "acc-1",
    estimatedAt: "2026-09-21T00:00:00.000Z",
    lastCalibrationAt: null,
    meterId: "meter-1",
    nextResetAt: "2026-10-01T16:00:00.000Z",
    overdrawn: 0,
    pendingRequests,
    remaining: 125,
    spentSinceCalibration: 0,
    unpricedRequests: 0,
  };
}

function billing(pendingRequests = 0): BillingStatus {
  return {
    accountId: "acc-1",
    cash: null,
    configurableCredits: true,
    credits: meter(pendingRequests),
    manualCalibration: false,
    model: "credits",
    officialRefresh: false,
    presets: [],
    processGeneration: 99,
    revision: 5,
    source: "local_estimate",
    unit: "credits",
    usage: null,
  };
}

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" ? value as Record<string, unknown> : null;
}

function mount(component: Component, initial: Record<string, unknown>) {
  const props = shallowRef(initial);
  const root: HostNode = { children: [], props: {}, type: "root" };
  const wrapper = defineComponent({
    setup: () => () => h(component, props.value),
  });
  const app = renderer.createApp(wrapper);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  return { app, root };
}

function circles(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.type === "button" && node.props["data-circle"] === "1");
}

function dialogs(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props.role === "dialog");
}

function classText(node: HostNode): string {
  const value = node.props.class;
  return Array.isArray(value) ? value.join(" ") : String(value ?? "");
}

function hasClass(root: HostNode, name: string): boolean {
  return walkHostNodes(root).some((node) => classText(node).split(/\s+/).includes(name));
}

async function invoke(node: HostNode | undefined): Promise<void> {
  assert.ok(node);
  const handler = node.props.onClick;
  assert.equal(typeof handler, "function");
  await Promise.resolve((handler as () => unknown)());
  await settle();
}

function prepare(snapshot: BillingStatus): RecordedCall[] {
  setActivePinia(createPinia());
  const control = bundle.useControlPlaneStore();
  control.sync({ revision: 9, processGeneration: 100 });
  return installRecordingFetch((call) => {
    if (call.method === "GET" && call.url.endsWith("/accounts/acc-1/billing")) return jsonResponse(snapshot);
    if (call.method === "POST" || call.method === "PUT") return jsonResponse({ ...snapshot, revision: 6 });
    throw new Error(`unexpected ${call.method} ${call.url}`);
  });
}

before(async () => {
  const testWindow = installTestWindow();
  Object.assign(testWindow, {
    dispatchEvent() { return true; },
    localStorage: { getItem: () => null, setItem() {}, removeItem() {} },
  });
  const loaded = await loadRetirementBundle();
  bundle = loaded.bundle;
  buildDir = loaded.directory;
  installHostElementMethods();
}, { timeout: 180000 });

after(async () => {
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

test("a manual grant posts the typed amount against the loaded status", () => serial(async () => {
  const snapshot = billing();
  const calls = prepare(snapshot);
  const store = bundle.useBillingStore();
  await store.load("acc-1", BINDING);
  bundle.useControlPlaneStore().sync({ revision: 9, processGeneration: 100 });
  const mounted = mount(bundle.CreditMeterPanel, {
    accountId: "acc-1",
    binding: BINDING,
    status: snapshot,
    now: NOW,
  });
  try {
    await settle();
    const rendered = text(mounted.root);
    assert.ok(rendered.includes("125"));
    assert.ok(rendered.includes("80"));
    assert.ok(rendered.includes("grant-archive"));
    const actions = circles(mounted.root);
    assert.equal(
      actions.length,
      3,
      JSON.stringify(walkHostNodes(mounted.root).filter((node) => node.type === "button").map((node) => node.props)),
    );
    await invoke(actions[0]);
    assert.equal(dialogs(mounted.root).length, 1);
    const input = walkHostNodes(mounted.root).find((node) => node.type === "input" && node.props["data-number"] === "1");
    assert.ok(input);
    (input.props.onInput as (value: number) => void)(25);
    await settle();
    const save = walkHostNodes(mounted.root).find((node) => node.type === "button" && node.props["data-variant"] === "primary");
    await invoke(save);
    const posts = calls.filter((call) => call.method === "POST" && call.url.endsWith("/billing/credits/grants"));
    assert.equal(posts.length, 1);
    assert.equal(posts[0]!.body?.amount, 25);
    assert.equal(posts[0]!.body?.expiresAt, null);
    assert.equal(posts[0]!.body?.expectedRevision, 5);
    assert.equal(posts[0]!.body?.processGeneration, 99);
    assert.equal(JSON.stringify(posts[0]!.body).includes("PerMillion"), false);
    assert.deepEqual(bundle.useControlPlaneStore().expectation(), {
      expectedRevision: 6,
      processGeneration: 99,
    });
  } finally {
    mounted.app.unmount();
  }
}));

test("opened credit settings keep the monthly amount and do not submit token rates", () => serial(async () => {
  const snapshot = billing();
  const calls = prepare(snapshot);
  const store = bundle.useBillingStore();
  await store.load("acc-1", BINDING);
  bundle.useControlPlaneStore().sync({ revision: 9, processGeneration: 100 });
  const mounted = mount(bundle.CreditMeterPanel, {
    accountId: "acc-1",
    binding: BINDING,
    status: snapshot,
    now: NOW,
  });
  try {
    await settle();
    await invoke(circles(mounted.root)[1]);
    assert.equal(dialogs(mounted.root).length, 1);
    assert.equal(hasClass(mounted.root, "rate-row"), false);
    assert.equal(hasClass(mounted.root, "rate-hint"), false);
    const save = walkHostNodes(mounted.root).find((node) => node.type === "button" && node.props["data-variant"] === "primary");
    await invoke(save);
    const puts = calls.filter((call) => call.method === "PUT" && call.url.endsWith("/billing/credits"));
    assert.equal(puts.length, 1);
    const submitted = record(puts[0]!.body?.configuration);
    assert.ok(submitted);
    assert.equal(record(submitted.monthly)?.amount, 250);
    assert.equal(puts[0]!.body?.expectedRevision, 5);
    assert.equal(puts[0]!.body?.processGeneration, 99);
    assert.equal("rates" in submitted, false);
    assert.equal("creditsPerCurrency" in submitted, false);
    assert.equal(JSON.stringify(submitted).includes("PerMillion"), false);
  } finally {
    mounted.app.unmount();
  }
}));

test("calibration posts the active bucket balance", () => serial(async () => {
  const snapshot = billing();
  const calls = prepare(snapshot);
  const store = bundle.useBillingStore();
  await store.load("acc-1", BINDING);
  bundle.useControlPlaneStore().sync({ revision: 9, processGeneration: 100 });
  const mounted = mount(bundle.CreditCalibrationEditor, {
    accountId: "acc-1",
    binding: BINDING,
    status: snapshot,
    now: NOW,
  });
  try {
    await settle();
    const save = walkHostNodes(mounted.root).find((node) => node.type === "button" && node.props["data-variant"] === "primary");
    await invoke(save);
    const posts = calls.filter((call) => call.method === "POST" && call.url.endsWith("/billing/credits/calibrate"));
    assert.equal(posts.length, 1);
    assert.deepEqual(posts[0]!.body?.balances, [{ bucketId: "manual", remaining: 40 }]);
    assert.equal(posts[0]!.body?.expectedRevision, 5);
    assert.equal(posts[0]!.body?.processGeneration, 99);
    assert.equal(JSON.stringify(posts[0]!.body).includes("PerMillion"), false);
  } finally {
    mounted.app.unmount();
  }
}));

test("pending settlement blocks calibration", () => serial(async () => {
  const snapshot = billing(4);
  const calls = prepare(snapshot);
  const store = bundle.useBillingStore();
  await store.load("acc-1", BINDING);
  const mounted = mount(bundle.CreditCalibrationEditor, {
    accountId: "acc-1",
    binding: BINDING,
    status: snapshot,
    now: NOW,
  });
  try {
    await settle();
    const save = walkHostNodes(mounted.root).find((node) => node.type === "button" && node.props["data-variant"] === "primary");
    assert.ok(save);
    assert.equal(save.props.disabled, true);
    await invoke(save);
    assert.equal(calls.filter((call) => call.method === "POST").length, 0);
  } finally {
    mounted.app.unmount();
  }
}));
