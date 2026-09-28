import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import {
  defineComponent,
  h,
  shallowRef,
  ssrContextKey,
  type App,
  type Component,
  type ShallowRef,
} from "vue";
import type { Account } from "../api/dashboard.ts";
import type { BillingStatus } from "../api/billing.ts";
import { billingBinding } from "../domain/billing.ts";
import type { BillingSlot } from "../stores/billing.ts";
import {
  createVueHostRenderer,
  settle,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

type StoreCall = { method: "load" | "refreshCash"; accountId: string; binding: string };

interface Harness {
  slot: ShallowRef<BillingSlot | undefined>;
  calls: StoreCall[];
}

let buildDir: string;
let BillingPanel: Component;

function harnessPlugin() {
  const prefix = "\0billing-panel-harness:";
  const modules: Record<string, string> = {
    naive: `
      import { defineComponent, h } from "vue";
      export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("button", attrs, slots.default?.());
      } });
    `,
    i18n: `
      export const t = (key) => String(key);
    `,
    billingStore: `
      import { computed } from "vue";
      export const useBillingStore = () => {
        const harness = globalThis.__billingHarness;
        return {
          slotFor: () => computed(() => harness.slot.value),
          load: (accountId, binding) => harness.load(accountId, binding),
          refreshCash: (accountId, binding) => harness.refreshCash(accountId, binding),
        };
      };
    `,
    creditBalance: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup: () => () => h("div", { class: "stub-credit-balance" }) });
    `,
    creditMeter: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup: () => () => h("div", { class: "stub-credit-meter" }) });
    `,
    officialPanel: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup(_, { attrs }) {
        return () => h("div", { ...attrs, class: "stub-cash-panel" });
      } });
    `,
    quotaSummary: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup: () => () => h("div", { class: "stub-quota-summary" }) });
    `,
    accountCredential: `
      export const endpointMatchesSavedGrant = () => true;
    `,
    accountIdentity: `
      export const selectedInferenceBinding = () => null;
    `,
    providers: `
      export const presentProviderUsage = (usage) => usage;
    `,
  };
  const forPanel: Record<string, string> = {
    "../i18n/index.ts": "i18n",
    "../stores/billing.ts": "billingStore",
    "./AccountCreditBalance.vue": "creditBalance",
    "./CreditMeterPanel.vue": "creditMeter",
    "./OfficialApiPanel.vue": "officialPanel",
    "./ProviderQuotaSummary.vue": "quotaSummary",
  };
  const forUpstreamBalance: Record<string, string> = {
    "./account-credential.ts": "accountCredential",
    "./account-identity.ts": "accountIdentity",
  };
  return {
    name: "billing-panel-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      const importerPath = importer?.replaceAll("\\", "/") ?? "";
      if (importerPath.includes("/src/components/BillingPanel.vue")) {
        const module = forPanel[source];
        return module ? `${prefix}${module}` : null;
      }
      if (importerPath.endsWith("/src/domain/upstream-balance.ts")) {
        const module = forUpstreamBalance[source];
        return module ? `${prefix}${module}` : null;
      }
      if (importerPath.endsWith("/src/domain/billing.ts") && source === "../api/providers.ts") {
        return `${prefix}providers`;
      }
      return null;
    },
    load(id: string) {
      if (id.includes("/src/components/BillingPanel.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

const renderer = createVueHostRenderer();

const NOW = Date.parse("2026-09-27T00:00:00Z");
const ENDPOINT = "https://billing.example.com";
const VERSION_A = "2026-09-20T00:00:00Z";
const VERSION_B = "2026-09-21T00:00:00Z";

function makeAccount(updatedAt: string): Account {
  return {
    id: "acc-1",
    provider_id: "prov-1",
    updated_at: updatedAt,
    custom_config: { endpoint_url: ENDPOINT, upstream_protocol: "openai" },
  } as unknown as Account;
}

function cashStatus(): BillingStatus {
  return {
    revision: 1,
    processGeneration: 1,
    model: "cash",
    cash: { balance: 12_000_000 },
  } as unknown as BillingStatus;
}

function slotForBinding(binding: string, patch: Partial<BillingSlot> = {}): BillingSlot {
  return {
    status: null,
    loaded: false,
    loading: false,
    mutating: false,
    error: null,
    boundVersion: binding,
    generation: 1,
    resyncBeforeMutate: false,
    ...patch,
  };
}

function createHarness(): Harness {
  const harness: Harness = { slot: shallowRef(undefined), calls: [] };
  (globalThis as { __billingHarness?: unknown }).__billingHarness = {
    slot: harness.slot,
    load: async (accountId: string, binding: string) => {
      harness.calls.push({ method: "load", accountId, binding });
    },
    refreshCash: async (accountId: string, binding: string) => {
      harness.calls.push({ method: "refreshCash", accountId, binding });
      return null;
    },
  };
  return harness;
}

interface Mounted {
  app: App;
  root: HostNode;
}

async function mountPanel(account: ShallowRef<Account>): Promise<Mounted> {
  const root: HostNode = { children: [], props: {}, type: "root" };
  const wrapper = defineComponent({
    setup: () => () => h(BillingPanel!, { account: account.value, now: NOW }),
  });
  const app = renderer.createApp(wrapper);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  return { app, root };
}

function nodesWithClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props.class === className);
}

function loadingIndicators(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props.role === "status");
}

function actionButtons(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.type === "button");
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "billing-panel-component-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/components/BillingPanel.vue"),
        fileName: () => "billing-panel.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue"] },
    },
  });
  BillingPanel = (await import(pathToFileURL(path.join(buildDir, "billing-panel.mjs")).href)).default;
});

after(async () => { await rm(buildDir, { force: true, recursive: true }); });

test("mount and remount issue no automatic billing loads", async () => {
  const harness = createHarness();
  const account = shallowRef(makeAccount(VERSION_A));

  const first = await mountPanel(account);
  assert.equal(harness.calls.length, 0);
  assert.equal(loadingIndicators(first.root).length, 1);
  assert.equal(nodesWithClass(first.root, "stub-cash-panel").length, 0);
  first.app.unmount();

  const second = await mountPanel(account);
  assert.equal(harness.calls.length, 0);
  assert.equal(loadingIndicators(second.root).length, 1);
  second.app.unmount();
});

test("a binding change hides the stale snapshot and triggers no automatic load", async () => {
  const harness = createHarness();
  const bindingA = billingBinding(VERSION_A, ENDPOINT);
  harness.slot.value = slotForBinding(bindingA, { status: cashStatus(), loaded: true });
  const account = shallowRef(makeAccount(VERSION_A));
  const mounted = await mountPanel(account);
  assert.equal(nodesWithClass(mounted.root, "stub-cash-panel").length, 1);

  // The row rebinds (account version bump): the old snapshot must disappear
  // without the panel issuing its own read.
  account.value = makeAccount(VERSION_B);
  await settle();
  assert.equal(harness.calls.length, 0);
  assert.equal(nodesWithClass(mounted.root, "stub-cash-panel").length, 0);
  assert.equal(loadingIndicators(mounted.root).length, 1);

  // Once the owner supplies a snapshot for the new binding, content returns.
  const bindingB = billingBinding(VERSION_B, ENDPOINT);
  harness.slot.value = slotForBinding(bindingB, { status: cashStatus(), loaded: true });
  await settle();
  assert.equal(harness.calls.length, 0);
  assert.equal(nodesWithClass(mounted.root, "stub-cash-panel").length, 1);
  mounted.app.unmount();
});

test("a same-binding read failure keeps last-good content and retry loads once with the current binding", async () => {
  const harness = createHarness();
  const binding = billingBinding(VERSION_A, ENDPOINT);
  harness.slot.value = slotForBinding(binding, {
    status: cashStatus(),
    loaded: true,
    error: "load_failed",
  });
  const account = shallowRef(makeAccount(VERSION_A));
  const mounted = await mountPanel(account);

  // Last-good content stays visible alongside the failure affordance.
  assert.equal(nodesWithClass(mounted.root, "stub-cash-panel").length, 1);
  const retry = actionButtons(mounted.root);
  assert.equal(retry.length, 1);

  (retry[0]!.props.onClick as () => void)();
  await settle();
  assert.deepEqual(harness.calls, [{ method: "load", accountId: "acc-1", binding }]);
  // The explicit retry does not clear the still-stored snapshot by itself.
  assert.equal(nodesWithClass(mounted.root, "stub-cash-panel").length, 1);
  mounted.app.unmount();
});

test("an initial failure with no snapshot offers retry, which loads once with the current binding", async () => {
  const harness = createHarness();
  const binding = billingBinding(VERSION_A, ENDPOINT);
  harness.slot.value = slotForBinding(binding, { error: "load_failed" });
  const account = shallowRef(makeAccount(VERSION_A));
  const mounted = await mountPanel(account);

  assert.equal(nodesWithClass(mounted.root, "stub-cash-panel").length, 0);
  assert.equal(loadingIndicators(mounted.root).length, 0);
  const retry = actionButtons(mounted.root);
  assert.equal(retry.length, 1);

  (retry[0]!.props.onClick as () => void)();
  await settle();
  assert.deepEqual(harness.calls, [{ method: "load", accountId: "acc-1", binding }]);
  mounted.app.unmount();
});

test("the cash refresh action refreshes through the store with the current binding", async () => {
  const harness = createHarness();
  const binding = billingBinding(VERSION_A, ENDPOINT);
  harness.slot.value = slotForBinding(binding, { status: cashStatus(), loaded: true });
  const account = shallowRef(makeAccount(VERSION_A));
  const mounted = await mountPanel(account);

  const panel = nodesWithClass(mounted.root, "stub-cash-panel")[0];
  assert.ok(panel, "cash panel should render");
  const refreshKey = Object.keys(panel.props).find((key) => key.startsWith("onRefresh"));
  assert.ok(refreshKey, "cash panel should receive a refresh handler");
  await (panel.props[refreshKey] as () => Promise<void>)();
  await settle();
  assert.deepEqual(harness.calls, [{ method: "refreshCash", accountId: "acc-1", binding }]);
  mounted.app.unmount();
});
