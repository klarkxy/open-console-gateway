import assert from "node:assert/strict";
import { rm } from "node:fs/promises";
import { after, before, test } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { defineComponent, h, shallowRef, ssrContextKey, type Component } from "vue";
import type { Account } from "../api/dashboard.ts";
import type { BillingStatus } from "../api/billing.ts";
import type { OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import { billingBinding } from "../domain/billing.ts";
import { PAY_GO_METER_EMPTY } from "../domain/pay-go-meter.ts";
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
const RETIRED_PRICE = /\/official-api\/pricing|\/pricing\/multipliers|\/providers\/[^/]+\/pricing/;

const renderer = createVueHostRenderer();
let bundle: RetirementBundle;
let buildDir = "";
let lane = Promise.resolve();

function serial(run: () => Promise<void>): Promise<void> {
  const next = lane.then(run, run);
  lane = next.then(() => undefined, () => undefined);
  return next;
}

function account(): Account {
  return {
    id: "acc-1",
    name: "DeepSeek",
    username: "",
    password: "",
    key: "",
    enabled: true,
    account_type: "key",
    setup_step: "ready",
    provider_id: "deepseek",
    credential_kind: "api_key",
    quota_scope: "key",
    purchase_date: "2026-09-01",
    expires_on: "2027-09-01",
    cooldown_until: null,
    cooldown_generic_until: null,
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: null,
    last_error: null,
    auth_error: null,
    notes: "",
    usage_sync_last_success_at: null,
    usage_sync_next_allowed_at: null,
    created_at: "2026-09-01T00:00:00.000Z",
    updated_at: UPDATED,
    verification_status: "not_required",
    connection_verified_at: null,
    verification_error: null,
    plan_routable: true,
    custom_config: {
      account_id: "acc-1",
      endpoint_url: ENDPOINT,
      upstream_protocol: "chat_completions",
      created_at: "2026-09-01T00:00:00.000Z",
      updated_at: UPDATED,
    },
    model_capabilities: [],
    ollama_billing_tier: null,
  };
}

function cash(total: number, revision: number): OfficialApiStatus {
  return {
    accountId: "acc-1",
    balanceAvailable: true,
    balances: [{
      currency: "CNY",
      granted: 0,
      observedAt: "2026-09-21T08:00:00.000Z",
      toppedUp: 0,
      total,
    }],
    kind: "deepseek",
    lifetimeSpend: [],
    monthSpend: [],
    monthStartedAt: "2026-09-01T00:00:00.000Z",
    processGeneration: 99,
    providerId: "deepseek",
    revision,
    unpricedRequests: 0,
  };
}

function billing(snapshot: OfficialApiStatus): BillingStatus {
  return {
    accountId: "acc-1",
    cash: snapshot,
    configurableCredits: false,
    credits: null,
    manualCalibration: false,
    model: "cash",
    officialRefresh: true,
    presets: [],
    processGeneration: snapshot.processGeneration,
    revision: snapshot.revision,
    source: "official",
    unit: "CNY",
    usage: null,
  };
}

function emptyCash(): OfficialApiStatus {
  return {
    ...cash(0, 5),
    balanceAvailable: false,
    balances: [],
  };
}

function assertNoRetiredPrice(calls: readonly RecordedCall[]): void {
  for (const call of calls) assert.doesNotMatch(`${call.method} ${call.url}`, RETIRED_PRICE);
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
  return {
    app,
    root,
    update(next: Record<string, unknown>) {
      props.value = next;
    },
  };
}

function circleButton(root: HostNode): HostNode {
  const buttons = walkHostNodes(root).filter((node) => node.type === "button");
  const button = buttons.find((node) => node.props["data-circle"] === "1");
  assert.ok(button, JSON.stringify(buttons.map((node) => node.props)));
  return button;
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
}, { timeout: 180000 });

after(async () => {
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

test("account balance renders and refresh posts the status CAS", () => serial(async () => {
  setActivePinia(createPinia());
  const control = bundle.useControlPlaneStore();
  const store = bundle.useBillingStore();
  control.sync({ revision: 9, processGeneration: 100 });
  const calls = installRecordingFetch((call) => {
    if (call.method === "GET" && call.url.endsWith("/accounts/acc-1/billing")) {
      return jsonResponse(billing(cash(12.5, 5)));
    }
    if (call.method === "POST" && call.url.endsWith("/accounts/acc-1/official-api/balance")) {
      return jsonResponse(cash(18, 6));
    }
    throw new Error(`unexpected ${call.method} ${call.url}`);
  });
  const mounted = mount(bundle.BillingPanel, { account: account(), now: NOW });
  try {
    await settle();
    assert.equal(calls.length, 0);
    await store.load("acc-1", BINDING);
    await settle();
    assert.match(text(mounted.root), /12\.50/);
    assert.equal(text(mounted.root).includes("999"), false);
    assert.equal(walkHostNodes(mounted.root).some((node) => {
      const value = String(node.props.class ?? "");
      return value.includes("official-api-table") || value.includes("official-api-head");
    }), false);
    control.sync({ revision: 9, processGeneration: 100 });
    assert.deepEqual(control.expectation(), { expectedRevision: 9, processGeneration: 100 });
    await Promise.resolve((circleButton(mounted.root).props.onClick as () => unknown)());
    await settle();
    assert.match(text(mounted.root), /18\.00/);
    const posts = calls.filter((call) => call.method === "POST");
    assert.equal(posts.length, 1);
    assert.match(posts[0]!.url, /\/accounts\/acc-1\/official-api\/balance$/);
    assert.deepEqual(posts[0]!.body, { expectedRevision: 5, processGeneration: 99 });
    assertNoRetiredPrice(calls);
  } finally {
    mounted.app.unmount();
  }
}));

test("a failed balance refresh keeps the cached amount", () => serial(async () => {
  setActivePinia(createPinia());
  const control = bundle.useControlPlaneStore();
  const store = bundle.useBillingStore();
  control.sync({ revision: 9, processGeneration: 100 });
  const calls = installRecordingFetch((call) => {
    if (call.method === "GET" && call.url.endsWith("/accounts/acc-1/billing")) {
      return jsonResponse(billing(cash(12.5, 5)));
    }
    if (call.method === "POST" && call.url.endsWith("/accounts/acc-1/official-api/balance")) {
      return jsonResponse({ message: "upstream" }, 500);
    }
    throw new Error(`unexpected ${call.method} ${call.url}`);
  });
  const mounted = mount(bundle.BillingPanel, { account: account(), now: NOW });
  try {
    await store.load("acc-1", BINDING);
    await settle();
    assert.match(text(mounted.root), /12\.50/);
    control.sync({ revision: 9, processGeneration: 100 });
    await Promise.resolve((circleButton(mounted.root).props.onClick as () => unknown)());
    await settle();
    assert.match(text(mounted.root), /12\.50/);
    assert.equal(calls.filter((call) => call.method === "POST").length, 1);
    assert.deepEqual(calls.find((call) => call.method === "POST")?.body, {
      expectedRevision: 5,
      processGeneration: 99,
    });
    assertNoRetiredPrice(calls);
  } finally {
    mounted.app.unmount();
  }
}));

test("an unavailable remaining balance renders the empty marker", () => serial(async () => {
  globalThis.fetch = async (input: unknown) => {
    throw new Error(`unexpected fetch ${String(input)}`);
  };
  const mounted = mount(bundle.OfficialApiPanel, {
    providerId: "deepseek",
    accountId: "acc-1",
    accountStatus: emptyCash(),
    now: NOW,
  });
  try {
    await settle();
    assert.ok(text(mounted.root).includes(PAY_GO_METER_EMPTY));
    assert.equal(text(mounted.root).includes("0.00"), false);
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props["data-circle"] === "1"), false);
  } finally {
    mounted.app.unmount();
  }
}));

test("provider navigation does not call a retired price endpoint", () => serial(async () => {
  const calls = installRecordingFetch(() => {
    throw new Error("official panel fetched during provider navigation");
  });
  const mounted = mount(bundle.OfficialApiPanel, { providerId: "deepseek", now: NOW });
  try {
    await settle();
    mounted.update({ providerId: "zhipu", now: NOW });
    await settle();
    mounted.update({ providerId: "zhipu", accountId: "acc-1", now: NOW });
    await settle();
    assert.equal(calls.length, 0);
    assert.equal(text(mounted.root).includes("999"), false);
  } finally {
    mounted.app.unmount();
  }
}));
