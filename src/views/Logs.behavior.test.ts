import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { KeepAlive, defineComponent, h, ssrContextKey, type App, type Component } from "vue";
import { createPinia } from "pinia";
import { createMemoryHistory, createRouter, RouterView, type Router } from "vue-router";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

type ForwardQuery = { status?: string; account_id?: string; request_id?: string };
type GatewayQuery = { level?: string | null; category?: string | null };

type ObservabilityStub = {
  gatewayLogs: unknown[];
  gatewayLoaded: boolean;
  gatewayLoading: boolean;
  gatewayError: string;
  gatewayLoadedAt: number;
  forwardLogs: unknown[];
  forwardTotals: { total_requests: number; prompt_tokens: number; completion_tokens: number; cached_tokens: number; cost: number };
  forwardLoaded: boolean;
  forwardLoading: boolean;
  forwardError: string;
  forwardLoadedAt: number;
  models: string[];
  clientKeys: unknown[];
  forwardQueries: ForwardQuery[];
  gatewayQueries: GatewayQuery[];
  loadGateway: (query: GatewayQuery) => Promise<null>;
  loadForward: (query: ForwardQuery) => Promise<null>;
  loadModels: () => Promise<null>;
  loadKeys: () => Promise<null>;
};

let buildDir: string;
let Logs: Component;
const renderer = createVueHostRenderer();

function logsHarnessPlugin() {
  const prefix = "\0logs-component-harness:";
  const modules: Record<string, string> = {
    naive: `
      import { defineComponent, h } from "vue";
      const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
      } });
      export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("button", attrs, slots.default?.());
      } });
      export const NInput = defineComponent({
        inheritAttrs: false,
        props: { value: { type: String, default: "" } },
        setup(props, { attrs }) { return () => h("input", { ...attrs, value: props.value }); },
      });
      export const NSelect = defineComponent({
        inheritAttrs: false,
        props: { value: [String, Number] },
        setup(props, { attrs }) { return () => h("select", { ...attrs, value: props.value }); },
      });
      export const NTabs = defineComponent({ inheritAttrs: false, props: { value: String }, setup(props, { attrs, slots }) {
        return () => h("div", { ...attrs, "data-tab": props.value }, slots.default?.());
      } });
      export const NTabPane = pass;
      export const NAlert = pass;
      export const NDataTable = pass;
      export const NDatePicker = pass;
      export const NEmpty = pass;
      export const NIcon = pass;
      export const NPopover = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
        return () => h("div", [slots.trigger?.(), slots.default?.()]);
      } });
      export const NTag = pass;
      export const NTooltip = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
        return () => h("div", [slots.trigger?.(), slots.default?.()]);
      } });
      export const useMessage = () => ({ error() {}, success() {}, warning() {} });
    `,
    dashboard: `export const UNATTRIBUTED_KEY_FILTER = "__unattributed__";`,
    observability: `
      import { defineStore } from "pinia";
      import { ref } from "vue";
      export const useObservabilityStore = defineStore("observability", () => {
        const impl = globalThis.__logsObservability;
        return {
          gatewayLogs: ref(impl.gatewayLogs),
          gatewayLoaded: ref(impl.gatewayLoaded),
          gatewayLoading: ref(impl.gatewayLoading),
          gatewayError: ref(impl.gatewayError),
          gatewayLoadedAt: ref(impl.gatewayLoadedAt),
          forwardLogs: ref(impl.forwardLogs),
          forwardTotals: ref(impl.forwardTotals),
          forwardLoaded: ref(impl.forwardLoaded),
          forwardLoading: ref(impl.forwardLoading),
          forwardError: ref(impl.forwardError),
          forwardLoadedAt: ref(impl.forwardLoadedAt),
          models: ref(impl.models),
          clientKeys: ref(impl.clientKeys),
          loadGateway: (...args) => impl.loadGateway(...args),
          loadForward: (...args) => impl.loadForward(...args),
          loadModels: (...args) => impl.loadModels(...args),
          loadKeys: (...args) => impl.loadKeys(...args),
        };
      });
    `,
    accounts: `export const useAccountsStore = () => globalThis.__logsAccounts;`,
    providers: `export const useProvidersStore = () => ({ catalog: [], loadCatalog: async () => [] });`,
    i18n: `
      import { ref } from "vue";
      export const t = (key, values = {}) => key.replace(/\\{(\\w+)\\}/g, (_, name) => String(values[name] ?? ""));
      export const locale = ref("en-US");
    `,
    clipboard: `
      import { ref } from "vue";
      const copiedTarget = ref("");
      export const formatNumber = (value) => String(value ?? 0);
      export const formatCost = (value) => String(value ?? 0);
      export const useClipboard = () => ({ copiedTarget, copy: async () => {}, cleanup: () => {} });
    `,
    icons: `
      import { defineComponent } from "vue";
      const icon = defineComponent({ setup() { return () => null; } });
      export const ArrowDownOutlined = icon;
      export const ArrowUpOutlined = icon;
      export const CalendarOutlined = icon;
      export const CheckOutlined = icon;
      export const ClearOutlined = icon;
      export const CopyOutlined = icon;
      export const ReloadOutlined = icon;
    `,
    plans: `export const planLabel = () => ""; export function providerSurfaces() { return []; }`,
  };
  const sources: Record<string, string> = {
    "naive-ui": "naive",
    "@vicons/antd": "icons",
    "../api/dashboard": "dashboard",
    "../api/dashboard.ts": "dashboard",
    "../stores/observability.ts": "observability",
    "../stores/accounts.ts": "accounts",
    "../stores/providers.ts": "providers",
    "../i18n/index.ts": "i18n",
    "../utils/format.ts": "clipboard",
  };
  return {
    name: "logs-component-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      if (source === "@vicons/antd") return `${prefix}icons`;
      if (source.includes("domain/plans")) return `${prefix}plans`;
      const importerPath = importer?.replaceAll("\\", "/") ?? "";
      if (!importerPath.includes("/src/views/Logs.vue")) return null;
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/views/Logs.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

const Shell = defineComponent({
  setup() {
    return () => h(RouterView, null, {
      default: ({ Component }: { Component: Component | undefined }) => {
        if (!Component) return null;
        return h(KeepAlive, null, {
          default: () => h(Component),
        });
      },
    });
  },
});

function createObservability(): ObservabilityStub {
  const forwardQueries: ForwardQuery[] = [];
  const gatewayQueries: GatewayQuery[] = [];
  return {
    gatewayLogs: [],
    gatewayLoaded: true,
    gatewayLoading: false,
    gatewayError: "",
    gatewayLoadedAt: Date.now(),
    forwardLogs: [],
    forwardTotals: { total_requests: 0, prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cost: 0 },
    forwardLoaded: true,
    forwardLoading: false,
    forwardError: "",
    forwardLoadedAt: Date.now(),
    models: [],
    clientKeys: [],
    forwardQueries,
    gatewayQueries,
    async loadGateway(query) {
      gatewayQueries.push(query);
      return null;
    },
    async loadForward(query) {
      forwardQueries.push(query);
      return null;
    },
    async loadModels() {
      return null;
    },
    async loadKeys() {
      return null;
    },
  };
}

async function mountAt(
  query: Record<string, string>,
): Promise<{ app: App; root: HostNode; router: Router; observability: ObservabilityStub; accountLoads: { count: number } }> {
  installTestWindow();
  const observability = createObservability();
  const accountLoads = { count: 0 };
  (globalThis as { __logsObservability?: ObservabilityStub }).__logsObservability = observability;
  (globalThis as { __logsAccounts?: { accounts: unknown[]; loadPresented: () => Promise<unknown[]> } }).__logsAccounts = {
    accounts: [],
    async loadPresented() {
      accountLoads.count += 1;
      return [];
    },
  };
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/logs", name: "logs", component: Logs, meta: { view: "logs" } },
      { path: "/dashboard", name: "dashboard", component: { render: () => h("div") } },
    ],
  });
  await router.push({ name: "logs", query });
  await router.isReady();
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Shell);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.use(router);
  app.use(createPinia());
  app.mount(root);
  await settle(20);
  return { app, root, router, observability, accountLoads };
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts", "frontend-logic-repair", "core-tests");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "logs-component-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [logsHarnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/views/Logs.vue"),
        fileName: () => "logs.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue", "vue-router", "pinia"] },
    },
    esbuild: { target: "esnext" },
  });
  Logs = (await import(pathToFileURL(path.join(buildDir, "logs.mjs")).href)).default;
});

after(async () => {
  await rm(buildDir, { force: true, recursive: true });
});

async function awaitHistoryNavigation(router: Router, navigate: () => unknown): Promise<void> {
  let stop: () => void = () => {};
  const completed = new Promise<void>((resolve) => {
    stop = router.afterEach(() => {
      resolve();
    });
  });
  try {
    await navigate();
    await completed;
  } finally {
    stop();
  }
  await settle();
}

test("the same Logs instance reloads when its query changes", async () => {
  const mounted = await mountAt({ status: "success" });
  try {
    assert.equal(mounted.observability.forwardQueries.at(-1)?.status, "success");
    const loadsAfterMount = mounted.accountLoads.count;
    await mounted.router.push({ name: "logs", query: { status: "error" } });
    await settle(20);
    assert.equal(mounted.accountLoads.count, loadsAfterMount, "query change reuses the KeepAlive instance");
    assert.equal(mounted.observability.forwardQueries.at(-1)?.status, "error");
    assert.ok(mounted.observability.forwardQueries.length <= 4, "inbound query must not loop replace/fetch");
  } finally {
    mounted.app.unmount();
  }
});

test("Back and Forward on the same Logs instance sync the fetch filter", async () => {
  const mounted = await mountAt({ status: "success" });
  try {
    const loadsAfterMount = mounted.accountLoads.count;
    await mounted.router.push({ name: "logs", query: { status: "error" } });
    await settle(20);
    assert.equal(mounted.observability.forwardQueries.at(-1)?.status, "error");
    await awaitHistoryNavigation(mounted.router, () => mounted.router.back());
    assert.equal(mounted.accountLoads.count, loadsAfterMount, "Back reuses the KeepAlive Logs instance");
    assert.equal(mounted.observability.forwardQueries.at(-1)?.status, "success");
    await awaitHistoryNavigation(mounted.router, () => mounted.router.forward());
    assert.equal(mounted.accountLoads.count, loadsAfterMount, "Forward reuses the KeepAlive Logs instance");
    assert.equal(mounted.observability.forwardQueries.at(-1)?.status, "error");
  } finally {
    mounted.app.unmount();
  }
});

test("returning to Logs with a different query syncs the fetch filter on the cached instance", async () => {
  const mounted = await mountAt({ status: "success" });
  try {
    const loadsAfterMount = mounted.accountLoads.count;
    await mounted.router.push({ name: "dashboard" });
    await settle(20);
    await mounted.router.push({ name: "logs", query: { status: "client_error" } });
    await settle(20);
    assert.equal(mounted.accountLoads.count, loadsAfterMount, "leave/return reuses the KeepAlive Logs instance");
    assert.equal(mounted.observability.forwardQueries.at(-1)?.status, "client_error");
  } finally {
    mounted.app.unmount();
  }
});
