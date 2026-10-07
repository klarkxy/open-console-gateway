import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { getActivePinia, type Pinia } from "pinia";
import { createMemoryHistory, createRouter, type Router } from "vue-router";
import { defineComponent, ssrContextKey, type App, type Component } from "vue";
import { setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { dropAllSnapshots } from "../stores/persistence.ts";
import { useAliasPageStore } from "../stores/aliasPage.ts";
import { useSessionStore } from "../stores/session.ts";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

const BUILTIN = "alias-builtin-sentinel";
const DYNAMIC = "alias-dynamic-sentinel";
const CPA = "alias-cpa-sentinel";

let buildDir = "";
let Aliases: Component;
const renderer = createVueHostRenderer();
const storage = memoryStorage();

type FailureMap = Partial<Record<string, string>>;
type Recorded = { url: string; method: string };

function memoryStorage(): Storage {
  const data = new Map<string, string>();
  return {
    get length() { return data.size; },
    clear: () => data.clear(),
    getItem: (key) => data.get(key) ?? null,
    key: (index) => [...data.keys()][index] ?? null,
    removeItem: (key) => { data.delete(key); },
    setItem: (key, value) => { data.set(key, String(value)); },
  };
}

function walkSources(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) walkSources(full, out);
    else if (name.endsWith(".vue") || name.endsWith(".ts")) out.push(full);
  }
  return out;
}

function importedNames(specifier: string): string[] {
  const names = new Set<string>();
  const pattern = /import\s+(type\s+)?\{([\s\S]*?)\}\s+from\s+["']([^"']+)["']/g;
  for (const file of walkSources(path.resolve("src"))) {
    const source = readFileSync(file, "utf8");
    for (const match of source.matchAll(pattern)) {
      if (match[1] || match[3] !== specifier) continue;
      for (const part of match[2]!.split(",")) {
        const piece = part.trim();
        if (!piece || piece.startsWith("type ")) continue;
        const original = piece.split(/\s+as\s+/)[0]?.trim();
        if (original) names.add(original);
      }
    }
  }
  return [...names];
}

function naiveSource(names: string[]): string {
  const special: Record<string, string> = {
    NPagination: `export const NPagination = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", attrs, [
        ...(slots.prev?.({ page: attrs.page }) ?? []),
        ...(slots.label?.({ type: "page", node: attrs.page, active: true }) ?? []),
        ...(slots.next?.({ page: attrs.page }) ?? []),
      ]);
    } });`,
    NAlert: `export const NAlert = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { role: "alert", type: attrs.type, class: attrs.class }, [attrs.title ?? "", ...Object.values(slots).flatMap((slot) => slot?.() ?? [])]);
    } });`,
    NButton: `export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("button", attrs, slots.default?.());
    } });`,
    NDataTable: `export const NDataTable = defineComponent({ inheritAttrs: false, setup(_, { attrs }) {
      return () => {
        const columns = Array.isArray(attrs.columns) ? attrs.columns : [];
        const data = Array.isArray(attrs.data) ? attrs.data : [];
        return h("table", {}, data.map((row, index) => h("tr", { key: String(index) }, columns.map((column) => {
          const rendered = typeof column.render === "function" ? column.render(row) : row?.[column.key];
          return h("td", {}, rendered ?? "");
        }))));
      };
    } });`,
    NModal: `export const NModal = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => attrs.show === false ? h("div") : h("div", { role: "dialog", class: attrs.class }, [...(slots.default?.() ?? []), ...(slots.footer?.() ?? [])]);
    } });`,
    NPopconfirm: `export const NPopconfirm = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { class: "popconfirm" }, [...(slots.trigger?.() ?? []), h("button", { class: "popconfirm-positive", onClick: () => invoke(attrs.onPositiveClick) })]);
    } });`,
    NSpin: `export const NSpin = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { role: "status", class: attrs.class }, slots.default?.());
    } });`,
    NTabPane: `export const NTabPane = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { "data-tab": attrs.name }, slots.default?.());
    } });`,
    NTooltip: `export const NTooltip = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
      return () => h("span", {}, [...(slots.trigger?.() ?? []), ...(slots.default?.() ?? [])]);
    } });`,
    useDialog: `export function useDialog() { const noop = () => ({ destroy() {} }); return { warning: noop, error: noop, success: noop, info: noop }; }`,
    useMessage: `export function useMessage() {
      const record = (type) => (...args) => { (globalThis.__ocgMessages ??= []).push({ type, args }); };
      return { error: record("error"), success: record("success"), warning: record("warning"), info: record("info") };
    }`,
  };
  const body = names.map((name) => {
    if (special[name]) return special[name];
    if (name.startsWith("use")) return `export function ${name}() { return { value: null }; }`;
    if (name.endsWith("Theme")) return `export const ${name} = {};`;
    return `export const ${name} = pass;`;
  });
  return `
    import { defineComponent, h } from "vue";
    const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
    } });
    function invoke(value) {
      if (typeof value === "function") value();
      else if (Array.isArray(value)) value.forEach(invoke);
    }
    ${body.join("\n")}
  `;
}

function iconSource(names: string[]): string {
  const exports = names.map((name) => `export const ${name} = icon;`).join("\n");
  return `
    import { defineComponent, h } from "vue";
    const icon = defineComponent(() => () => h("i"));
    ${exports}
  `;
}

function harnessPlugin(naive: string, icons: string) {
  return {
    name: "ocg-alias-host",
    enforce: "pre" as const,
    resolveId(source: string) {
      if (source === "naive-ui") return "\0ocg-naive";
      if (source === "@vicons/antd") return "\0ocg-icons";
      return null;
    },
    load(id: string) {
      if (id.includes("type=style")) return "";
      if (id === "\0ocg-naive") return naive;
      if (id === "\0ocg-icons") return icons;
      return null;
    },
  };
}

function aliasBody(failures: FailureMap) {
  const names = [BUILTIN, DYNAMIC, CPA];
  return { revision: { revision: 12, processGeneration: 42 }, readVersion: "aliases12", asOf: new Date().toISOString(),
    validUntil: new Date(Date.now() + 15_000).toISOString(), totalGroups: 3, totalRows: 5000, filteredGroups: 3, filteredRows: 5000,
    offset: 0, limit: 50, hasMore: true,
    errors: Object.entries(failures).filter(([resource]) => resource !== "page").map(([resource, code]) => ({ resource, id: null, code })),
    groups: names.map((publicModel, index) => ({ publicModel, publicationKey: publicModel, published: true,
      totalRows: index ? 1 : 4998, matchingRows: index ? 1 : 4998, hasOverlap: index === 0, continued: index === 0,
      rows: Array.from({ length: index ? 1 : 48 }, (_, row) => ({ key: `${publicModel}:${row}`, providerId: index === 2 ? "cpa" : "opencode",
        destinationId: "dest", publicModel, upstreamModel: `upstream-${index}-${row}`, providerPlan: `plan-${index}`,
        customAccountId: null, customAccount: null, routable: true, routingRanks: [row + 1], platformLabel: null,
        capability: { state: index === 0 ? "unknown" : index === 1 ? "ready" : "unavailable", destinationId: "dest", source: index === 1 ? "modelsdev" : null,
          inputModalities: index === 1 ? ["text"] : [], outputModalities: index === 1 ? ["text"] : [] },
        target: index === 2 ? null : { accountId: null, providerId: "opencode", destinationId: "dest", model: publicModel, capabilities: false },
        capabilityTarget: index === 0 ? { accountId: null, providerId: "opencode", destinationId: "dest", model: publicModel, capabilities: true } : null,
      })) })) };
}
type Dashboard = { requests: Recorded[]; failures: FailureMap };
function installDashboard(dashboard: Dashboard): void {
  Object.defineProperty(globalThis, "fetch", { configurable: true, value: async (input: string, init: RequestInit = {}) => {
    const url = String(input); const method = init.method ?? "GET"; dashboard.requests.push({ url, method });
    if (!url.includes("/pages/aliases")) throw new Error(`unexpected inventory read ${url}`);
    if (dashboard.failures.page) throw new Error(dashboard.failures.page);
    return new Response(JSON.stringify(aliasBody(dashboard.failures)), { headers: { "Content-Type": "application/json" } });
  }});
}
function prepareWindow(): void {
  const view = installTestWindow({ pathname: "/dashboard/aliases", href: "http://127.0.0.1/dashboard/aliases" });
  Object.assign(view, { dispatchEvent: () => true, localStorage: storage });
  globalThis.localStorage = storage;
}

async function untilQuiet(requests: Recorded[], floor: number): Promise<void> {
  let last = -1;
  let stable = 0;
  for (let attempt = 0; attempt < 80; attempt += 1) {
    await new Promise((resolve) => setImmediate(resolve));
    if (requests.length === last && requests.length > floor) {
      stable += 1;
      if (stable >= 4) return;
    } else {
      stable = 0;
      last = requests.length;
    }
  }
  throw new Error(`alias reads did not settle (${requests.length}, floor ${floor})`);
}

function alerts(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props.role === "alert");
}

function alertsWith(root: HostNode, token: string): HostNode[] {
  return alerts(root).filter((node) => text(node).includes(token));
}

function shown(root: HostNode): string {
  return text(root);
}

type Mounted = {
  app: App;
  root: HostNode;
  dashboard: Dashboard;
  pinia: Pinia;
  router: Router;
};

async function mountAliases(pinia: Pinia, requests: Recorded[]): Promise<{ app: App; root: HostNode; router: Router }> {
  const router: Router = createRouter({
    history: createMemoryHistory("/dashboard/"),
    routes: [
      { path: "/aliases", name: "aliases", component: defineComponent({ setup: () => () => null }) },
      { path: "/providers", name: "providers", component: defineComponent({ setup: () => () => null }) },
      { path: "/accounts", name: "accounts", component: defineComponent({ setup: () => () => null }) },
      { path: "/:pathMatch(.*)*", component: defineComponent({ setup: () => () => null }) },
    ],
  });
  await router.push({ name: "aliases" });
  await router.isReady();
  const floor = requests.length;
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Aliases);
  app.use(pinia);
  app.use(router);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await untilQuiet(requests, floor);
  await settle();
  return { app, root, router };
}

async function openAliases(failures: FailureMap): Promise<Mounted> {
  dropAllSnapshots();
  storage.clear();
  prepareWindow();
  (globalThis as { __ocgMessages?: unknown[] }).__ocgMessages = [];
  setupControlPlane(12, 42);
  const pinia = getActivePinia();
  if (!pinia) throw new Error("pinia should be active");
  const dashboard: Dashboard = { requests: [], failures: { ...failures } };
  installDashboard(dashboard);
  useAliasPageStore();
  useSessionStore();
  const mounted = await mountAliases(pinia, dashboard.requests);
  return { ...mounted, dashboard, pinia };
}

async function remountAliases(session: Mounted): Promise<Mounted> {
  prepareWindow();
  const mounted = await mountAliases(session.pinia, session.dashboard.requests);
  return { ...mounted, dashboard: session.dashboard, pinia: session.pinia };
}

before(async () => {
  prepareWindow();
  const scratch = path.join(process.cwd(), ".artifacts", "frontend-logic-repair", "provider-tests");
  await mkdir(scratch, { recursive: true });
  buildDir = await mkdtemp(path.join(scratch, "build-aliases-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(naiveSource(importedNames("naive-ui")), iconSource(importedNames("@vicons/antd"))), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: { entry: path.resolve("src/views/Aliases.vue"), fileName: () => "aliases.mjs", formats: ["es"] },
      outDir: buildDir,
      rollupOptions: {
        external: ["vue", "pinia", "vue-router"],
        output: { inlineDynamicImports: true },
      },
    },
    esbuild: { target: "esnext" },
  });
  Aliases = (await import(pathToFileURL(path.join(buildDir, "aliases.mjs")).href)).default;
});

after(async () => {
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

describe("bounded alias page reads", { concurrency: false }, () => {
  test("builtin, dynamic, and CPA rows render from one page read with bounded DOM", async () => {
    const mounted = await openAliases({});
    try {
      assert.ok([BUILTIN, DYNAMIC, CPA].every(name => shown(mounted.root).includes(name)));
      assert.equal(alerts(mounted.root).length, 0);
      assert.equal(mounted.dashboard.requests.length, 1);
      assert.ok(mounted.dashboard.requests[0]!.url.includes("limit=50"));
      assert.equal(walkHostNodes(mounted.root).filter(node => node.type === "tbody").flatMap(node => node.children).filter(node => node.type === "tr").length, 50);
    } finally { mounted.app.unmount(); }
  });
  test("a failed initial page read exposes one failure and no fabricated rows", async () => {
    const mounted = await openAliases({ page: "PAGE_FAILURE" });
    try { assert.equal(alertsWith(mounted.root, "PAGE_FAILURE").length, 1); assert.equal(shown(mounted.root).includes(BUILTIN), false); }
    finally { mounted.app.unmount(); }
  });
  test("a failed revalidation keeps the last successful rows", async () => {
    const first = await openAliases({}); first.app.unmount(); first.dashboard.failures.page = "PAGE_REFRESH_FAILURE";
    const mounted = await remountAliases(first);
    try { assert.ok(shown(mounted.root).includes(BUILTIN)); assert.equal(alertsWith(mounted.root, "PAGE_REFRESH_FAILURE").length, 1); }
    finally { mounted.app.unmount(); }
  });
  test("partial source issues remain explicit while successful server rows stay visible", async () => {
    const mounted = await openAliases({ metadata: "METADATA_FAILURE", identities: "RANK_FAILURE" });
    try { assert.ok(shown(mounted.root).includes(DYNAMIC)); assert.equal(alertsWith(mounted.root, "METADATA_FAILURE").length, 1); assert.equal(alertsWith(mounted.root, "RANK_FAILURE").length, 1); }
    finally { mounted.app.unmount(); }
  });
  test("capability navigation opens the exact destination and one-shot metadata model", async () => {
    const mounted = await openAliases({});
    try {
      const link = walkHostNodes(mounted.root).find(node => typeof node.props.onClick === "function" && String(node.props["aria-label"] ?? "").includes(BUILTIN));
      assert.ok(link); (link.props.onClick as () => void)();
      for (let attempt = 0; attempt < 20 && mounted.router.currentRoute.value.name !== "providers"; attempt++) await new Promise(resolve => setImmediate(resolve));
      await settle();
      assert.equal(mounted.router.currentRoute.value.name, "providers");
      assert.equal(mounted.router.currentRoute.value.query.destination, "dest");
      assert.equal(mounted.router.currentRoute.value.query.capabilities, BUILTIN);
    } finally { mounted.app.unmount(); }
  });
});
