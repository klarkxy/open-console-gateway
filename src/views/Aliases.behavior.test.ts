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
import { v3AccountDto, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { dropAllSnapshots } from "../stores/persistence.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useIdentitiesStore } from "../stores/identities.ts";
import { useProvidersStore } from "../stores/providers.ts";
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

function evidence() {
  return {
    protocol: "chat_completions",
    available: true,
    enabled: true,
    source: "static",
    verifiedAt: null,
    observedAt: null,
    lastProbeResult: null,
    lastProbeAt: null,
    lastProbeError: null,
    override: "auto",
  };
}

function catalogEntry(providerId: string, dynamic: boolean) {
  return {
    providerId,
    origin: dynamic ? "custom" : "builtin",
    editable: dynamic,
    deletable: dynamic,
    offering: "api",
    displayName: providerId,
    displayFamily: providerId,
    credentialKind: "api_key",
    quotaScope: "key",
    singleton: false,
    creationAvailability: dynamic ? "available" : "unavailable",
    creationUnavailableReason: null,
    verificationPolicy: "not_required",
    verificationRuntimeAvailability: "not_applicable",
    routable: true,
    managedRegistration: false,
    pricingAvailability: "not_applicable",
    usageAvailability: "unavailable",
    manualUsageCalibration: false,
    quotaUnit: "tokens",
    modelSource: dynamic ? "dynamic_provider" : "static",
    keyPrefix: null,
    authSchemes: ["bearer"],
    upstreamProtocols: ["chat_completions"],
    formFields: [],
    modelAliases: [],
  };
}

function contractsBody() {
  return {
    revision: 12,
    processGeneration: 42,
    pricingRevision: "p1",
    customEndpoints: [],
    providers: [{
      scopeKind: "provider",
      scopeId: "opencode",
      providerId: "opencode",
      staticProtocolSnapshotDate: null,
      accounts: [],
      catalog: { source: "static", sourceUrl: "", refreshedAt: null, models: [BUILTIN], refreshSupported: false },
      models: [{
        alias: BUILTIN,
        modelId: BUILTIN,
        preferredProtocol: "chat_completions",
        protocols: { chat_completions: evidence(), responses: null, messages: null },
        routable: true,
        disabledReasons: [],
      }],
      pricing: { availability: "not_applicable" },
      usage: { availability: "unavailable" },
      card: { fetchZenModels: false, discoverModels: false, protocolProbe: true, catalogRefresh: false },
      catalogRoutable: true,
      productionInference: true,
      disabledReasons: [],
      revision: 12,
    }],
  };
}

function destinationBody() {
  return {
    cards: [{ id: "card-dyn", destinationId: "dest-dyn", credentialIds: [] }],
    credentials: [],
    destinations: [{
      accountControls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
      adapter: "http",
      authScheme: "bearer",
      baseUrl: "https://lab.example/v1",
      brandFamily: null,
      capabilities: {
        billingTierRequired: false,
        discoverableModels: true,
        externalIntegration: false,
        identityHeaders: false,
        managedSignup: false,
        observer: false,
        officialBalanceProbe: [],
        redirectPolicy: "no_follow",
        testable: true,
      },
      catalog: [{
        enabled: true,
        preferred: "chat_completions",
        protocols: ["chat_completions"],
        publicModel: DYNAMIC,
        upstreamModel: "upstream-dynamic-sentinel",
        upstreamOverride: null,
      }],
      enabled: true,
      id: "dest-dyn",
      legacy: { kind: "dynamic", id: "dyn-lab" },
      maxCredentials: 1,
      name: "Dyn Lab",
      observerCredentialId: null,
      plan: null,
      protocols: ["chat_completions"],
      protocolRoutes: [],
    }],
    revision: { revision: 12, processGeneration: 42, pricingRevision: "p1" },
  };
}

function channel(pathname: string): string | null {
  if (pathname === "/provider-contracts") return "contracts";
  if (pathname === "/providers") return "catalog";
  if (pathname === "/account-records") return "accounts";
  if (pathname === "/cpa/models") return "cpa";
  if (pathname === "/accounts") return "identities";
  if (pathname === "/alias-publication") return "publication";
  if (pathname === "/routing/cards") return "destinations";
  return null;
}

function bodyFor(pathname: string): object {
  if (pathname === "/provider-contracts") return contractsBody();
  if (pathname === "/providers") {
    return {
      entries: [catalogEntry("opencode", false), catalogEntry("dyn-lab", true)],
      revision: 12,
      processGeneration: 42,
      pricingRevision: "p1",
    };
  }
  if (pathname === "/providers/dyn-lab") {
    return {
      id: "dyn-lab",
      name: "Dyn Lab",
      origin: "custom",
      offering: "api",
      editable: true,
      deletable: true,
      endpointUrl: "http://127.0.0.1:9",
      upstreamProtocol: "chat_completions",
      authKind: "bearer",
      models: [{ publicModel: DYNAMIC, upstreamModel: "upstream-dynamic-sentinel" }],
      createdAt: "2026-01-01T00:00:00Z",
      updatedAt: "2026-01-01T00:00:00Z",
      revision: 12,
      processGeneration: 42,
    };
  }
  if (pathname === "/account-records") {
    return {
      accounts: [
        v3AccountDto("acct-builtin", { providerId: "opencode" }),
        v3AccountDto("acct-dyn", { providerId: "dyn-lab" }),
        v3AccountDto("acct-cpa", { providerId: "cpa" }),
      ],
      revision: 12,
      processGeneration: 42,
    };
  }
  if (pathname === "/cpa/models") {
    return {
      models: [{ id: CPA, enabled: true, ownedBy: "synthetic" }],
      sourceUrl: null,
      refreshedAt: null,
      revision: { revision: 12, processGeneration: 42, pricingRevision: "p1" },
    };
  }
  if (pathname === "/accounts") {
    return { identities: [], revision: { revision: 12, processGeneration: 42, pricingRevision: "p1" } };
  }
  if (pathname === "/alias-publication") {
    return { revision: { revision: 12, processGeneration: 42 }, unpublished: [] };
  }
  if (pathname === "/routing/cards") return destinationBody();
  throw new Error(`unexpected alias request ${pathname}`);
}

type Dashboard = { requests: Recorded[]; failures: FailureMap };

function installDashboard(dashboard: Dashboard): void {
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: async (input: string, init: RequestInit = {}) => {
      const url = String(input);
      const method = init.method ?? "GET";
      dashboard.requests.push({ url, method });
      const pathname = url.slice(url.indexOf("/dashboard/api/v4") + "/dashboard/api/v4".length);
      const name = channel(pathname);
      if (name && dashboard.failures[name]) throw new Error(dashboard.failures[name]);
      return new Response(JSON.stringify(bodyFor(pathname)), { headers: { "Content-Type": "application/json" } });
    },
  });
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
};

async function mountAliases(pinia: Pinia, requests: Recorded[]): Promise<{ app: App; root: HostNode }> {
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
  return { app, root };
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
  useProvidersStore();
  useDestinationsStore();
  useAccountsStore();
  useIdentitiesStore();
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

describe("alias read channels", { concurrency: false }, () => {
  test("enabled builtin, dynamic, and CPA aliases render together", async () => {
    const mounted = await openAliases({});
    try {
      const visible = shown(mounted.root);
      assert.ok(visible.includes(BUILTIN), visible.slice(0, 500));
      assert.ok(visible.includes(DYNAMIC), visible.slice(0, 500));
      assert.ok(visible.includes(CPA), visible.slice(0, 500));
      assert.equal(alerts(mounted.root).length, 0);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a contracts failure on an empty cache is only the contracts status", async () => {
    const mounted = await openAliases({ contracts: "F08_CONTRACTS" });
    try {
      assert.equal(alertsWith(mounted.root, "F08_CONTRACTS").length, 1);
      assert.equal(shown(mounted.root).includes(BUILTIN), false);
      assert.equal(alerts(mounted.root).length, 1);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a contracts revalidation keeps the last alias rows and reports its own failure", async () => {
    const first = await openAliases({});
    first.app.unmount();
    first.dashboard.failures.contracts = "F08_CONTRACTS_REFRESH";
    const mounted = await remountAliases(first);
    try {
      assert.ok(shown(mounted.root).includes(BUILTIN));
      assert.equal(alertsWith(mounted.root, "F08_CONTRACTS_REFRESH").length, 1);
    } finally {
      mounted.app.unmount();
    }
  });

  test("an accounts failure on an empty cache is only the accounts status", async () => {
    const mounted = await openAliases({ accounts: "F08_ACCOUNTS" });
    try {
      assert.equal(alertsWith(mounted.root, "F08_ACCOUNTS").length, 1);
      assert.equal(shown(mounted.root).includes(BUILTIN), false);
      assert.equal(shown(mounted.root).includes(DYNAMIC), false);
    } finally {
      mounted.app.unmount();
    }
  });

  test("an accounts revalidation keeps the last alias rows and reports its own failure", async () => {
    const first = await openAliases({});
    first.app.unmount();
    first.dashboard.failures.accounts = "F08_ACCOUNTS_REFRESH";
    const mounted = await remountAliases(first);
    try {
      assert.ok(shown(mounted.root).includes(BUILTIN));
      assert.ok(shown(mounted.root).includes(DYNAMIC));
      assert.equal(alertsWith(mounted.root, "F08_ACCOUNTS_REFRESH").length, 1);
    } finally {
      mounted.app.unmount();
    }
  });

  test("an identity rank failure keeps alias rows and reports only that status", async () => {
    const mounted = await openAliases({ identities: "F08_IDENTITIES" });
    try {
      assert.ok(shown(mounted.root).includes(BUILTIN));
      assert.equal(alertsWith(mounted.root, "F08_IDENTITIES").length, 1);
      assert.equal(alertsWith(mounted.root, "F08_DEST").length, 0);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a CPA catalog failure hides only CPA aliases and reports that status", async () => {
    const mounted = await openAliases({ cpa: "F08_CPA" });
    try {
      assert.equal(shown(mounted.root).includes(CPA), false);
      assert.ok(shown(mounted.root).includes(BUILTIN));
      assert.ok(shown(mounted.root).includes(DYNAMIC));
      assert.equal(alertsWith(mounted.root, "F08_CPA").length, 1);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a CPA revalidation keeps the last CPA alias and reports its own failure", async () => {
    const first = await openAliases({});
    first.app.unmount();
    first.dashboard.failures.cpa = "F08_CPA_REFRESH";
    const mounted = await remountAliases(first);
    try {
      assert.ok(shown(mounted.root).includes(CPA));
      assert.equal(alertsWith(mounted.root, "F08_CPA_REFRESH").length, 1);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a publication failure stays on the publication status and still shows dynamic aliases", async () => {
    const mounted = await openAliases({ publication: "F08_PUBLICATION" });
    try {
      const store = useProvidersStore();
      assert.match(store.aliasPublicationLoadError, /F08_PUBLICATION/);
      assert.equal(alertsWith(mounted.root, "F08_PUBLICATION").length, 1);
      assert.equal(alertsWith(mounted.root, "F08_DEST").length, 0);
      assert.ok(shown(mounted.root).includes(DYNAMIC));
    } finally {
      mounted.app.unmount();
    }
  });

  test("a publication revalidation keeps dynamic aliases and does not become a destination failure", async () => {
    const first = await openAliases({});
    first.app.unmount();
    first.dashboard.failures.publication = "F08_PUBLICATION_REFRESH";
    const mounted = await remountAliases(first);
    try {
      assert.match(useProvidersStore().aliasPublicationLoadError, /F08_PUBLICATION_REFRESH/);
      assert.equal(alertsWith(mounted.root, "F08_PUBLICATION_REFRESH").length, 1);
      assert.equal(alertsWith(mounted.root, "F08_DEST").length, 0);
      assert.ok(shown(mounted.root).includes(DYNAMIC));
    } finally {
      mounted.app.unmount();
    }
  });

  test("a destination failure hides dynamic aliases and reports the destination status", async () => {
    const mounted = await openAliases({ destinations: "F08_DEST" });
    try {
      assert.equal(shown(mounted.root).includes(DYNAMIC), false);
      assert.ok(shown(mounted.root).includes(BUILTIN));
      assert.equal(alertsWith(mounted.root, "F08_DEST").length, 1);
      assert.equal(useProvidersStore().aliasPublicationLoadError.includes("F08_DEST"), false);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a destination revalidation keeps cached dynamic aliases and reports the destination status", async () => {
    const first = await openAliases({});
    first.app.unmount();
    first.dashboard.failures.destinations = "F08_DEST_REFRESH";
    const mounted = await remountAliases(first);
    try {
      assert.ok(shown(mounted.root).includes(DYNAMIC));
      assert.equal(alertsWith(mounted.root, "F08_DEST_REFRESH").length, 1);
      assert.equal(useProvidersStore().aliasPublicationLoadError.includes("F08_DEST_REFRESH"), false);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a catalog failure keeps builtin aliases and reports the catalog status", async () => {
    const mounted = await openAliases({ catalog: "F08_CATALOG" });
    try {
      assert.ok(shown(mounted.root).includes(BUILTIN), shown(mounted.root).slice(0, 500));
      assert.equal(alertsWith(mounted.root, "F08_CATALOG").length, 1, alerts(mounted.root).map((node) => text(node)).join(" | "));
      assert.equal(alertsWith(mounted.root, "F08_DEST").length, 0);
    } finally {
      mounted.app.unmount();
    }
  });

  test("a catalog revalidation keeps cached dynamic aliases and reports the catalog status", async () => {
    const first = await openAliases({});
    first.app.unmount();
    first.dashboard.failures.catalog = "F08_CATALOG_REFRESH";
    const mounted = await remountAliases(first);
    try {
      assert.ok(shown(mounted.root).includes(DYNAMIC));
      assert.ok(shown(mounted.root).includes(BUILTIN));
      assert.equal(alertsWith(mounted.root, "F08_CATALOG_REFRESH").length, 1, alerts(mounted.root).map((node) => text(node)).join(" | "));
    } finally {
      mounted.app.unmount();
    }
  });
});
