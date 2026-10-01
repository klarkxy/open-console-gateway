import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { createPinia, setActivePinia } from "pinia";
import {
  defineComponent,
  h,
  ssrContextKey,
  type App,
  type Component,
} from "vue";
import type { Account } from "../api/dashboard.ts";
import type { ProviderCatalogEntry, ProviderUsageResponse } from "../api/providers.ts";
import type { ObservedUsageWindow } from "../domain/accounts-usage.ts";
import { billingBinding } from "../domain/billing.ts";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

type BillingModule = typeof import("../stores/billing.ts");

const billingStoreUrl = pathToFileURL(path.resolve("src/stores/billing.ts")).href;
const NOW = Date.parse("2026-10-01T00:00:00.000Z");
const RESET_AT = "2026-10-01T00:30:00.000Z";
const ACK_AT = "2026-10-01T00:00:01.000Z";
const ACCOUNT_ID = "ollama-1";

let buildDir = "";
let CredentialBody: Component;
let openBillingStore: BillingModule["useBillingStore"] | null = null;

function billingStore(): ReturnType<BillingModule["useBillingStore"]> {
  if (!openBillingStore) throw new Error("billing store was not loaded");
  return openBillingStore();
}

function harnessPlugin() {
  const prefix = "\0credential-body-harness:";
  const modules: Record<string, string> = {
    naive: `
      import { defineComponent, h } from "vue";
      export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("button", attrs, slots.default?.());
      } });
      export const NProgress = defineComponent({
        inheritAttrs: false,
        props: { percentage: { default: null } },
        setup(props) {
          return () => h("div", { "data-progress": "1", "data-percentage": props.percentage });
        },
      });
    `,
    destinations: `
      export const useDestinationsStore = () => ({ destinationForAccount: () => null });
    `,
    figure: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup: () => () => h("div", { class: "stub-account-figure" }) });
    `,
    credit: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup: () => () => h("div", { class: "stub-credit-balance" }) });
    `,
    panel: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup: () => () => h("div", { class: "stub-billing-panel" }) });
    `,
  };
  const forBody: Record<string, string> = {
    "../stores/destinations.ts": "destinations",
    "./AccountFigure.vue": "figure",
    "./AccountCreditBalance.vue": "credit",
    "./BillingPanel.vue": "panel",
  };
  return {
    name: "credential-body-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "vue" || source === "pinia") return { id: source, external: true };
      if (source === "naive-ui") return `${prefix}naive`;
      const importerPath = importer?.replaceAll("\\", "/") ?? "";
      if (importerPath.includes("/src/components/CredentialBody.vue")) {
        if (source === "../stores/billing.ts") return { id: billingStoreUrl, external: true };
        const module = forBody[source];
        return module ? `${prefix}${module}` : null;
      }
      return null;
    },
    load(id: string) {
      if (id.includes("type=style") || id.endsWith(".css")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] ?? null : null;
    },
  };
}

const renderer = createVueHostRenderer();

function ollamaCatalog(): ProviderCatalogEntry {
  return {
    provider_id: "ollama",
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: "ollama",
    display_family: "ollama",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    creation_unavailable_reason: null,
    verification_policy: "not_required",
    verification_runtime_availability: "not_applicable",
    routable: true,
    managed_registration: false,
    usage_availability: "local_state",
    manual_usage_calibration: true,
    quota_unit: "percent",
    model_source: "builtin",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
  } as ProviderCatalogEntry;
}

function ollamaAccount(): Account {
  return {
    id: ACCOUNT_ID,
    name: "ollama",
    provider_id: "ollama",
    updated_at: "v1",
    enabled: true,
    setup_step: "ready",
    plan_routable: true,
  } as unknown as Account;
}

function acknowledgedUsage(percent: number): ObservedUsageWindow {
  return {
    account_id: ACCOUNT_ID,
    window_5h: percent,
    window_week: null,
    window_month: null,
    resets_in_5h: RESET_AT,
    resets_in_week: null,
    resets_in_month: null,
  };
}

interface Mounted {
  app: App;
  root: HostNode;
}

function classTokens(value: unknown): string[] {
  return typeof value === "string" ? value.split(/\s+/).filter(Boolean) : [];
}

function byClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter((node) => classTokens(node.props.class).includes(className));
}

function progressValues(root: HostNode): unknown[] {
  return walkHostNodes(root)
    .filter((node) => node.props["data-progress"] === "1")
    .map((node) => node.props["data-percentage"]);
}

function quotaRows(root: HostNode): HostNode[] {
  return byClass(root, "provider-quota-row")
    .filter((node) => !classTokens(node.props.class).includes("provider-quota-row--empty"));
}

function resetTimes(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => (
    node.type === "time" && classTokens(node.props.class).includes("provider-quota-row__reset")
  ));
}

async function mountBody(usageLoadError: string | null = null): Promise<Mounted> {
  const root: HostNode = { children: [], props: {}, type: "root" };
  const wrapper = defineComponent({
    setup: () => () => h(CredentialBody, {
      account: ollamaAccount(),
      catalog: [ollamaCatalog()],
      providerUsage: null satisfies ProviderUsageResponse | null,
      now: NOW,
      usageLoading: false,
      usageLoadError,
    }),
  });
  const app = renderer.createApp(wrapper);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  return { app, root };
}

function acknowledge(percent: number): void {
  setActivePinia(createPinia());
  billingStore().applyCalibratedUsage(
    ACCOUNT_ID,
    billingBinding("v1", null),
    "window_5h",
    acknowledgedUsage(percent),
    ACK_AT,
  );
}

before(async () => {
  installTestWindow();
  const billing = (await import(billingStoreUrl)) as BillingModule;
  openBillingStore = billing.useBillingStore;
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "credential-body-component-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/components/CredentialBody.vue"),
        fileName: () => "credential-body.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue", "pinia"] },
    },
  });
  CredentialBody = (await import(pathToFileURL(path.join(buildDir, "credential-body.mjs")).href)).default;
}, { timeout: 180_000 });

after(async () => {
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

test("the quota path renders a known percent and reset when billing status is absent", { timeout: 5_000 }, async () => {
  acknowledge(42.5);
  const mounted = await mountBody();
  try {
    assert.equal(byClass(mounted.root, "stub-billing-panel").length, 0);
    assert.equal(byClass(mounted.root, "official-plan-usage").length, 1);
    assert.equal(byClass(mounted.root, "stub-credit-balance").length, 0);
    assert.deepEqual(progressValues(mounted.root), [42.5]);
    assert.equal(quotaRows(mounted.root).length, 1);
    const resets = resetTimes(mounted.root);
    assert.equal(resets.length, 1);
    assert.equal(text(resets[0]!).includes("30"), true);
    assert.equal(text(mounted.root).includes("0%"), false);
    const slot = billingStore().slotFor(ACCOUNT_ID).value;
    assert.equal(slot?.status, null);
    assert.equal(slot?.loaded, false);
    assert.equal(slot?.manualReceipt?.windows.length, 1);
    assert.equal(slot?.manualReceipt?.windows[0]?.used, 42.5);
  } finally {
    mounted.app.unmount();
  }
});

test("a quota read error keeps the known receipt and does not invent a zero sibling", { timeout: 5_000 }, async () => {
  acknowledge(42.5);
  const mounted = await mountBody("load_failed");
  try {
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "alert"), true);
    assert.equal(byClass(mounted.root, "official-plan-usage").length, 1);
    assert.deepEqual(progressValues(mounted.root), [42.5]);
    assert.equal(quotaRows(mounted.root).length, 1);
    assert.equal(resetTimes(mounted.root).length, 1);
    assert.equal(text(resetTimes(mounted.root)[0]!).includes("30"), true);
    assert.equal(text(mounted.root).includes("0%"), false);
    assert.equal(billingStore().slotFor(ACCOUNT_ID).value?.status, null);
  } finally {
    mounted.app.unmount();
  }
});

test("an absent receipt and absent provider usage do not render a zero percent", { timeout: 5_000 }, async () => {
  setActivePinia(createPinia());
  const mounted = await mountBody();
  try {
    assert.equal(byClass(mounted.root, "stub-billing-panel").length, 0);
    assert.equal(byClass(mounted.root, "official-plan-usage").length, 1);
    assert.deepEqual(progressValues(mounted.root), []);
    assert.equal(quotaRows(mounted.root).length, 0);
    assert.equal(resetTimes(mounted.root).length, 0);
    assert.equal(text(mounted.root).includes("0%"), false);
    assert.equal(billingStore().slotFor(ACCOUNT_ID).value?.status ?? null, null);
  } finally {
    mounted.app.unmount();
  }
});
