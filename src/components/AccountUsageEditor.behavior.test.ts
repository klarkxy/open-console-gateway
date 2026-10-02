import assert from "node:assert/strict";
import { rm } from "node:fs/promises";
import { after, before, test } from "node:test";
import { defineComponent, h, ssrContextKey, type Component } from "vue";
import type { Account, UsageWindow } from "../api/dashboard.ts";
import type { UsageEditState, UsageKey } from "../domain/accounts-usage.ts";
import type { AccountUsageEdits, UsageLimitView } from "../domain/useAccountUsage.ts";
import {
  installHostElementMethods,
  loadQuotaBundle,
  type QuotaBundle,
} from "../test-helpers/panel-host.ts";
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

function account(): Account {
  return {
    id: "goat",
    name: "goat-marker",
    username: "",
    password: "",
    key: "",
    enabled: true,
    account_type: "key",
    setup_step: "ready",
    provider_id: "command-code",
    credential_kind: "api_key",
    quota_scope: "key",
    purchase_date: "2026-09-01",
    expires_on: "2026-10-01",
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
    updated_at: "2026-10-01T00:00:00.000Z",
    verification_status: "not_required",
    connection_verified_at: null,
    verification_error: null,
    plan_routable: true,
    model_capabilities: [],
    ollama_billing_tier: null,
  };
}

function observed(window5h: number | null): UsageWindow {
  return {
    account_id: "goat",
    window_5h: window5h,
    window_week: null,
    window_month: null,
    resets_in_5h: null,
    resets_in_week: null,
    resets_in_month: null,
  };
}

function edit(draft: number | null): UsageEditState {
  return {
    draft: draft as number,
    saved: draft as number,
    saving: false,
    error: null,
    resets_in_minutes_draft: null,
    resets_at_saved: null,
    resets_dirty: false,
  };
}

function mountEditor(props: {
  usage: UsageWindow;
  limits: UsageLimitView[];
  edits: AccountUsageEdits;
}, events: Array<{ name: string; args: unknown[] }>) {
  const root: HostNode = { children: [], props: {}, type: "root" };
  const wrapper = defineComponent({
    setup: () => () => h(bundle.AccountUsageEditor as Component, {
      account: account(),
      usage: props.usage,
      limits: props.limits,
      edits: props.edits,
      loading: false,
      now: NOW,
      onUpdateDraft: (key: UsageKey, value: number | null) => events.push({ name: "update-draft", args: [key, value] }),
      onUpdateResetsFirst: (key: UsageKey, value: number | null) => events.push({ name: "update-resets-first", args: [key, value] }),
      onUpdateResetsSecond: (key: UsageKey, value: number | null) => events.push({ name: "update-resets-second", args: [key, value] }),
      onSave: (key: UsageKey) => events.push({ name: "save", args: [key] }),
    }),
  });
  const app = renderer.createApp(wrapper);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  return { app, root };
}

function numberInputs(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props["data-number"] === "1");
}

function unavailable(root: HostNode): string | null {
  const rendered = text(root);
  if (MONEY.test(rendered)) return "money";
  if (rendered.includes("0%")) return "zero-percent";
  if (rendered.includes("NaN")) return "nan";
  if (rendered.includes("∞")) return "infinity";
  if (numberInputs(root).length === 0) return "missing-input";
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

test("a valid zero observation accepts 42.5 and a 30 minute reset field", async () => {
  const events: Array<{ name: string; args: unknown[] }> = [];
  const mounted = mountEditor({
    usage: observed(0),
    limits: [{ key: "window_5h", label: "window_5h", limit: 100 }],
    edits: { window_5h: edit(0) } as AccountUsageEdits,
  }, events);
  try {
    await settle();
    const inputs = numberInputs(mounted.root);
    assert.equal(inputs.length, 3);
    assert.equal(inputs[0]?.props["data-value"], "0");
    assert.equal(inputs[1]?.props["data-value"], "5");
    assert.equal(inputs[2]?.props["data-value"], "0");
    (inputs[0]?.props.onInput as (value: number) => void)(42.5);
    await settle();
    assert.deepEqual(events.at(-1), { name: "update-draft", args: ["window_5h", 42.5] });
    (inputs[2]?.props.onInput as (value: number) => void)(30);
    await settle();
    assert.deepEqual(events.at(-1), { name: "update-resets-second", args: ["window_5h", 30] });
    (inputs[1]?.props.onInput as (value: number) => void)(0);
    await settle();
    assert.deepEqual(events.at(-1), { name: "update-resets-first", args: ["window_5h", 0] });
  } finally {
    mounted.app.unmount();
  }
});

test("a null observation renders unavailable without money or 0%", async () => {
  const mounted = mountEditor({
    usage: observed(null),
    limits: [{ key: "window_5h", label: "window_5h", limit: 100 }],
    edits: { window_5h: edit(null) } as AccountUsageEdits,
  }, []);
  try {
    await settle();
    assert.equal(unavailable(mounted.root), null);
    assert.equal(numberInputs(mounted.root)[0]?.props["data-value"], "");
  } finally {
    mounted.app.unmount();
  }
});

test("a null, zero, or missing limit renders unavailable without money or 0%", async () => {
  for (const limit of [null, 0, Number.NaN, undefined]) {
    const mounted = mountEditor({
      usage: observed(null),
      limits: [{ key: "window_5h", label: "window_5h", limit: limit as number }],
      edits: { window_5h: edit(null) } as AccountUsageEdits,
    }, []);
    try {
      await settle();
      assert.equal(unavailable(mounted.root), null, String(limit));
      assert.equal(numberInputs(mounted.root)[0]?.props["data-value"], "");
    } finally {
      mounted.app.unmount();
    }
  }
});
