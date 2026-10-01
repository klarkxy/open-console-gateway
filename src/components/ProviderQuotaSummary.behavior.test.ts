import assert from "node:assert/strict";
import { rm } from "node:fs/promises";
import { after, before, test } from "node:test";
import { defineComponent, h, ssrContextKey, type Component } from "vue";
import type { ProviderQuotaWindow, ProviderUsageResponse } from "../api/providers.ts";
import {
  installHostElementMethods,
  loadQuotaBundle,
  type QuotaBundle,
} from "../../.artifacts/frontend-logic-repair/manual-quota-tests/panel-host.ts";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

const NOW = Date.parse("2026-10-01T00:00:00.000Z");
const MONEY = /[$€£¥￥]|USD|CNY/;

const renderer = createVueHostRenderer();
let bundle: QuotaBundle;
let buildDir = "";

function windowRow(overrides: Partial<ProviderQuotaWindow> = {}): ProviderQuotaWindow {
  return {
    account_id: "acc-1",
    window_kind: "five_hours",
    used: 0,
    limit_value: 100,
    started_at: null,
    resets_at: null,
    calibration_offset: 0,
    unit: "percent",
    source: "manual",
    observed_at: "2026-10-01T00:00:00.000Z",
    updated_at: "2026-10-01T00:00:00.000Z",
    ...overrides,
  };
}

function usage(windows: ProviderQuotaWindow[]): ProviderUsageResponse {
  return {
    account_id: "acc-1",
    provider_id: "command-code",
    availability: "available",
    quota_windows: windows,
    credit_balances: [],
    sync_state: null,
  };
}

function mount(component: Component, props: Record<string, unknown>) {
  const root: HostNode = { children: [], props: {}, type: "root" };
  const wrapper = defineComponent({
    setup: () => () => h(component, props),
  });
  const app = renderer.createApp(wrapper);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  return { app, root };
}

function progressValues(root: HostNode): unknown[] {
  return walkHostNodes(root)
    .filter((node) => node.props["data-progress"] === "1")
    .map((node) => node.props["data-percentage"]);
}

function unavailable(root: HostNode): string | null {
  const rendered = text(root);
  if (MONEY.test(rendered)) return "money";
  if (rendered.includes("0%")) return "zero-percent";
  if (rendered.includes("NaN")) return "nan";
  if (rendered.includes("∞")) return "infinity";
  const numeric = progressValues(root).filter((value) => typeof value === "number" && Number.isFinite(value));
  if (numeric.length > 0) return `progress:${numeric.join(",")}`;
  if (walkHostNodes(root).length === 0) return "empty";
  return null;
}

before(async () => {
  installTestWindow();
  const loaded = await loadQuotaBundle();
  bundle = loaded.bundle;
  buildDir = loaded.directory;
  installHostElementMethods();
}, { timeout: 180000 });

after(async () => {
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

test("a valid observed 0 of 100 renders 0 and a full window renders 100", async () => {
  const mounted = mount(bundle.ProviderQuotaSummary, {
    now: NOW,
    usage: usage([
      windowRow({ window_kind: "five_hours", used: 0, limit_value: 100 }),
      windowRow({ window_kind: "week", used: 100, limit_value: 100 }),
    ]),
  });
  try {
    await settle();
    assert.deepEqual(progressValues(mounted.root), [0, 100]);
    const rendered = text(mounted.root);
    assert.equal(rendered.includes("0"), true);
    assert.equal(rendered.includes("100"), true);
    assert.equal(MONEY.test(rendered), false);
  } finally {
    mounted.app.unmount();
  }
});

test("null usage renders unavailable without money or 0%", async () => {
  const mounted = mount(bundle.ProviderQuotaSummary, { now: NOW, usage: null });
  try {
    await settle();
    assert.equal(unavailable(mounted.root), null);
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "status"), true);
  } finally {
    mounted.app.unmount();
  }
});

test("a null limit renders unavailable without money or 0%", async () => {
  const mounted = mount(bundle.ProviderQuotaSummary, {
    now: NOW,
    usage: usage([windowRow({ used: 12, limit_value: null, unit: "usd" })]),
  });
  try {
    await settle();
    assert.equal(unavailable(mounted.root), null);
  } finally {
    mounted.app.unmount();
  }
});

test("a zero, negative, or non-finite limit renders unavailable", async () => {
  for (const limit of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
    const mounted = mount(bundle.ProviderQuotaSummary, {
      now: NOW,
      usage: usage([windowRow({ used: 12, limit_value: limit, unit: "percent" })]),
    });
    try {
      await settle();
      assert.equal(unavailable(mounted.root), null, String(limit));
    } finally {
      mounted.app.unmount();
    }
  }
});

test("a missing limit renders unavailable without money or 0%", async () => {
  const row = windowRow({ used: 4, unit: "usd" });
  delete (row as { limit_value?: number | null }).limit_value;
  const mounted = mount(bundle.ProviderQuotaSummary, {
    now: NOW,
    usage: usage([row]),
  });
  try {
    await settle();
    assert.equal(unavailable(mounted.root), null);
  } finally {
    mounted.app.unmount();
  }
});
