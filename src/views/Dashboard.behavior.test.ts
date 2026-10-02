import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { createPinia, getActivePinia, setActivePinia } from "pinia";
import { ssrContextKey, type App, type Component } from "vue";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { useConnectionStore } from "../stores/connection.ts";
import { useSessionStore } from "../stores/session.ts";
import { maskConnectionKey } from "./dashboard-connection.ts";
import {
  createTestWindow,
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
  type TestWindow,
} from "../test-helpers/vue-host-runtime.ts";

// The host renders the real Dashboard SFC, connection store, and /connection
// transport. Accounts, provider catalog, destinations, and the token chart are
// inert stand-ins so the key panel can settle without those payloads. Summary
// and token GETs return empty envelopes for the same reason. Assertions stay
// on the connection key panel.

const READ_FAILURE = "connection-read-failed";
const OLD_PRIMARY = "ocg-old-primary-1111";
const NEW_PRIMARY = "ocg-new-primary-2222";
const LAPTOP_SECRET = "ocg-sub-laptop-aaaa";

type HeldCall = {
  method: string;
  path: string;
  pending: boolean;
  resolve: (status: number, payload: unknown) => void;
};

type PageMessage = { type: string };

let buildDir = "";
let Dashboard: Component;
const renderer = createVueHostRenderer();
const heldCalls: HeldCall[] = [];
const copiedValues: string[] = [];
let restoreFetch: (() => void) | null = null;
let restoreClipboard: (() => void) | null = null;

function pageMessages(): PageMessage[] {
  const slot = globalThis as { __dashboardReadMessages?: PageMessage[] };
  slot.__dashboardReadMessages ??= [];
  return slot.__dashboardReadMessages;
}

function messageCount(type: string): number {
  return pageMessages().filter((message) => message.type === type).length;
}

function harnessPlugin() {
  const prefix = "\0dashboard-live-harness:";
  const naive = `
    import { defineComponent, h } from "vue";
    function invoke(handler) {
      if (typeof handler === "function") return handler();
      if (Array.isArray(handler)) return handler.map((entry) => invoke(entry));
      return undefined;
    }
    const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
    } });
    export const NAlert = defineComponent({
      inheritAttrs: false,
      props: { type: String, title: String },
      setup(props, { slots }) {
        return () => h("div", { role: "alert", "data-alert-type": props.type, title: props.title }, slots.default?.());
      },
    });
    export const NButton = defineComponent({
      inheritAttrs: false,
      props: { size: String, type: String, secondary: Boolean, quaternary: Boolean, circle: Boolean, disabled: Boolean, loading: Boolean },
      setup(props, { attrs, slots }) {
        return () => h("button", { ...attrs, ...props }, slots.default?.());
      },
    });
    export const NEmpty = pass;
    export const NIcon = pass;
    export const NSpin = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
      return () => h("div", slots.default?.());
    } });
    export const NTag = pass;
    export const NPopconfirm = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { class: "popconfirm" }, [
        ...(slots.trigger?.() ?? []),
        h("button", { class: "popconfirm-positive", onClick: () => invoke(attrs.onPositiveClick) }),
      ]);
    } });
    export const useMessage = () => {
      const record = (type) => () => { (globalThis.__dashboardReadMessages ??= []).push({ type }); };
      return { error: record("error"), success: record("success"), warning: record("warning"), info: record("info") };
    };
  `;
  const icons = `
    import { defineComponent } from "vue";
    const icon = defineComponent({ setup() { return () => null; } });
    export const ApiOutlined = icon;
    export const CheckOutlined = icon;
    export const CopyOutlined = icon;
    export const DownOutlined = icon;
    export const KeyOutlined = icon;
    export const ReloadOutlined = icon;
    export const UnorderedListOutlined = icon;
  `;
  const slots = `
    import { defineComponent, h } from "vue";
    export default defineComponent({
      inheritAttrs: false,
      setup(_, { slots }) {
        return () => h("div", [...(slots.trigger?.() ?? []), ...(slots.default?.() ?? [])]);
      },
    });
  `;
  const empty = `
    import { defineComponent, h } from "vue";
    export default defineComponent({ setup() { return () => h("div"); } });
  `;
  const accounts = `
    export function useAccountsStore() {
      return { accounts: [], loaded: true, loadPresented: async () => [] };
    }
  `;
  const providers = `
    export function useProvidersStore() {
      return { catalog: null, loadCatalog: async () => null };
    }
  `;
  const destinations = `
    export function useDestinationsStore() {
      return { load: async () => {}, destinationForAccount: () => null };
    }
  `;
  const modules: Record<string, string> = { naive, icons, slots, empty, accounts, providers, destinations };
  return {
    name: "dashboard-live-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      if (source === "@vicons/antd") return `${prefix}icons`;
      const fromDashboard = importer?.replaceAll("\\", "/").includes("/src/views/Dashboard.vue") ?? false;
      if (!fromDashboard) return null;
      if (source.includes("StackedBarChart")) return `${prefix}empty`;
      if (source.includes("OcgPopover") || source.includes("OcgTooltip")) return `${prefix}slots`;
      if (source.includes("/stores/accounts")) return `${prefix}accounts`;
      if (source.includes("/stores/providers")) return `${prefix}providers`;
      if (source.includes("/stores/destinations")) return `${prefix}destinations`;
      return null;
    },
    load(id: string) {
      if (id.endsWith(".css") || id.includes("vue&type=style")) return "";
      if (!id.startsWith(prefix)) return null;
      return modules[id.slice(prefix.length)] ?? null;
    },
  };
}

function requestTarget(input: RequestInfo | URL): URL {
  if (typeof input === "string") return new URL(input, "http://127.0.0.1");
  if (input instanceof URL) return input;
  return new URL(input.url, "http://127.0.0.1");
}

function jsonResponse(status: number, payload: unknown): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function connectionWire(primary: string, revision = 7) {
  return {
    clientRootUrl: "http://127.0.0.1:9042",
    gatewayPort: 9042,
    primaryKey: primary,
    processGeneration: 3,
    revision,
    subKeys: [{ id: "laptop", name: "Laptop", enabled: true, value: LAPTOP_SECRET }],
  };
}

function ancillary(pathname: string): unknown | null {
  if (pathname.endsWith("/dashboard/summary")) {
    return {
      availableAccounts: 0,
      gatewayRunning: true,
      monthCost: null,
      pricingRevision: "p",
      processGeneration: 3,
      revision: 1,
      todayCost: null,
      totalAccounts: 0,
      weekCost: null,
    };
  }
  if (pathname.includes("/dashboard/daily-tokens-by-model")) {
    return { items: [], pricingRevision: "p", processGeneration: 3, revision: 1 };
  }
  return null;
}

function installTransport(): void {
  heldCalls.splice(0, heldCalls.length);
  pageMessages().splice(0, pageMessages().length);
  copiedValues.splice(0, copiedValues.length);
  const previousFetch = globalThis.fetch;
  const fetchMock: typeof fetch = async (input, init) => {
    const url = requestTarget(input);
    const method = (init?.method ?? "GET").toUpperCase();
    const quiet = ancillary(url.pathname);
    if (quiet) return jsonResponse(200, quiet);
    if (!url.pathname.endsWith("/connection") && !url.pathname.includes("/keys")) {
      return jsonResponse(404, { code: "notFound", message: `${method} ${url.pathname}` });
    }
    return await new Promise((resolve) => {
      const call: HeldCall = {
        method,
        path: url.pathname,
        pending: true,
        resolve: (status, payload) => {
          call.pending = false;
          resolve(jsonResponse(status, payload));
        },
      };
      heldCalls.push(call);
    });
  };
  globalThis.fetch = fetchMock;
  restoreFetch = () => {
    globalThis.fetch = previousFetch;
    restoreFetch = null;
  };
  const nav = globalThis.navigator;
  const previousClipboard = Object.getOwnPropertyDescriptor(nav, "clipboard");
  Object.defineProperty(nav, "clipboard", {
    configurable: true,
    value: {
      writeText: async (value: string) => {
        copiedValues.push(value);
      },
    },
  });
  restoreClipboard = () => {
    if (previousClipboard) Object.defineProperty(nav, "clipboard", previousClipboard);
    else Reflect.deleteProperty(nav, "clipboard");
    restoreClipboard = null;
  };
}

function restoreTransport(): void {
  restoreFetch?.();
  restoreClipboard?.();
}

function connectionReads(): HeldCall[] {
  return heldCalls.filter((call) => call.method === "GET" && call.path.endsWith("/connection"));
}

function keyWrites(): HeldCall[] {
  return heldCalls.filter((call) => call.method !== "GET" && call.path.includes("/keys"));
}

async function waitFor(label: string, ready: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    if (ready()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  const pending = heldCalls.filter((call) => call.pending).map((call) => `${call.method} ${call.path}`);
  throw new Error(`${label}; pending: ${pending.join(", ") || "none"}`);
}

async function pendingCall(label: string, match: (call: HeldCall) => boolean): Promise<HeldCall> {
  let found: HeldCall | undefined;
  await waitFor(label, () => {
    found = [...heldCalls].reverse().find((call) => call.pending && match(call));
    return Boolean(found);
  });
  if (!found) throw new Error(label);
  return found;
}

function classTokens(value: unknown): string[] {
  if (typeof value === "string") return value.split(/\s+/).filter(Boolean);
  if (Array.isArray(value)) return value.flatMap(classTokens);
  return [];
}

function hasClass(node: HostNode, className: string): boolean {
  return classTokens(node.props.class).includes(className);
}

function hero(root: HostNode): HostNode {
  const panel = walkHostNodes(root).find((node) => hasClass(node, "connection-hero"));
  if (!panel) throw new Error("connection panel should render");
  return panel;
}

function buttonsIn(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.type === "button");
}

function controlByLabel(root: HostNode, label: string): HostNode {
  const found = buttonsIn(root).find((node) => node.props["aria-label"] === label || node.props.ariaLabel === label);
  if (!found) throw new Error(`control ${label} should render`);
  return found;
}

function insideHero(node: HostNode): boolean {
  let current: HostNode | undefined = node;
  while (current) {
    if (hasClass(current, "connection-hero")) return true;
    current = current.parent;
  }
  return false;
}

function heroWarning(root: HostNode): HostNode | undefined {
  return walkHostNodes(root).find((node) => (
    node.props.role === "alert" && node.props["data-alert-type"] === "warning" && insideHero(node)
  ));
}

function retryButton(alert: HostNode): HostNode {
  const button = walkHostNodes(alert).find((node) => node.type === "button");
  if (!button) throw new Error("read warning should expose a retry button");
  return button;
}

function maskedKey(root: HostNode): string {
  const code = walkHostNodes(hero(root)).find((node) => node.type === "code" && classTokens(node.props.class).length === 0 && !text(node).includes("http"));
  if (!code) throw new Error("masked key should render");
  return text(code);
}

function secretMasked(visible: string, raw: string): boolean {
  if (!raw) {
    return visible.length > 0
      && visible !== maskConnectionKey("")
      && !visible.includes(OLD_PRIMARY)
      && !visible.includes(NEW_PRIMARY);
  }
  return visible === maskConnectionKey(raw) && visible !== raw;
}

function isDisabled(node: HostNode): boolean {
  return node.props.disabled === true;
}

function isLoading(node: HostNode): boolean {
  return node.props.loading === true;
}

function startClick(node: HostNode): Promise<void> {
  const click = node.props.onClick;
  if (typeof click !== "function") throw new Error("control should be clickable");
  return Promise.resolve(click());
}

function confirmRotate(root: HostNode): Promise<void> {
  const positive = walkHostNodes(hero(root)).find((node) => hasClass(node, "popconfirm-positive"));
  if (!positive) throw new Error("key rotate confirmation should render");
  return startClick(positive);
}

function installDocument(view: TestWindow): void {
  const listeners = new Map<string, Set<() => void>>();
  const doc = {
    visibilityState: "hidden",
    hidden: true,
    addEventListener(type: string, listener: () => void) {
      const set = listeners.get(type) ?? new Set();
      set.add(listener);
      listeners.set(type, set);
    },
    removeEventListener(type: string, listener: () => void) {
      listeners.get(type)?.delete(listener);
    },
  };
  Object.defineProperty(globalThis, "document", { configurable: true, writable: true, value: doc });
  Object.defineProperty(view, "document", { configurable: true, value: doc });
}

function authenticate(): void {
  useSessionStore().applyStatus({
    authenticated: true,
    initialized: true,
    local: true,
    processGeneration: 3,
    revision: 7,
  });
  useControlPlaneStore().sync({ processGeneration: 3, revision: 7 });
}

async function mountDashboard(): Promise<{ app: App; root: HostNode }> {
  const view = createTestWindow();
  Object.assign(view.location, { origin: "http://127.0.0.1" });
  installTestWindow(view);
  installDocument(view);
  installTransport();
  const pinia = createPinia();
  setActivePinia(pinia);
  authenticate();
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Dashboard);
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  const load = await pendingCall("initial connection GET", (call) => call.method === "GET" && call.path.endsWith("/connection"));
  load.resolve(200, connectionWire(OLD_PRIMARY));
  await waitFor("key panel ready", () => {
    try {
      return useConnectionStore().info?.primary_key === OLD_PRIMARY
        && !isDisabled(controlByLabel(hero(root), "刷新 Key"));
    } catch {
      return false;
    }
  });
  return { app, root };
}

async function rotateUntilRead(root: HostNode): Promise<HeldCall> {
  const reads = connectionReads().length;
  const done = confirmRotate(root);
  const write = await pendingCall("key rotate", (call) => call.method !== "GET" && call.path.includes("/keys"));
  write.resolve(200, { processGeneration: 3, revision: 8 });
  const read = await pendingCall("deferred connection GET", (call) => call.method === "GET" && call.path.endsWith("/connection"));
  await done;
  await waitFor("rotate released", () => connectionReads().length > reads && !isLoading(controlByLabel(hero(root), "刷新 Key")));
  return read;
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts", "frontend-logic-repair", "keys-read-retry");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "dashboard-live-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/views/Dashboard.vue"),
        fileName: () => "dashboard-live.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue", "pinia"] },
    },
    esbuild: { target: "esnext" },
  });
  Dashboard = (await import(pathToFileURL(path.join(buildDir, "dashboard-live.mjs")).href)).default;
});

after(async () => {
  restoreTransport();
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

test("a confirmed dashboard rotate releases at ack and its read retry is one connection GET", async () => {
  const mounted = await mountDashboard();
  try {
    const read = await rotateUntilRead(mounted.root);
    assert.deepEqual({
      writes: keyWrites().length,
      pendingReads: connectionReads().filter((call) => call.pending).length,
      loading: isLoading(controlByLabel(hero(mounted.root), "刷新 Key")),
      copyDisabled: isDisabled(controlByLabel(hero(mounted.root), "复制 Key")),
      secretMasked: secretMasked(maskedKey(mounted.root), ""),
      warning: Boolean(heroWarning(mounted.root)),
      success: messageCount("success"),
      primary: useConnectionStore().info?.primary_key ?? null,
    }, {
      writes: 1,
      pendingReads: 1,
      loading: false,
      copyDisabled: true,
      secretMasked: true,
      warning: false,
      success: 1,
      primary: "",
    });
    read.resolve(503, { code: "unavailable", message: READ_FAILURE });
    await waitFor("hero read warning", () => useConnectionStore().refreshError === READ_FAILURE && Boolean(heroWarning(mounted.root)));
    const alert = heroWarning(mounted.root);
    if (!alert) throw new Error("failed read should render a warning inside the key panel");
    assert.equal(text(alert).includes(READ_FAILURE), true);
    assert.equal(secretMasked(maskedKey(mounted.root), ""), true);
    assert.equal(text(mounted.root).includes(NEW_PRIMARY), false);
    const writesAtRetry = keyWrites().length;
    const readsAtRetry = connectionReads().length;
    const retrying = startClick(retryButton(alert));
    const retry = await pendingCall("read retry", (call) => call.method === "GET" && call.path.endsWith("/connection"));
    assert.deepEqual({
      method: retry.method,
      writes: keyWrites().length - writesAtRetry,
      reads: connectionReads().length - readsAtRetry,
    }, { method: "GET", writes: 0, reads: 1 });
    retry.resolve(200, connectionWire(NEW_PRIMARY, 9));
    await retrying;
    await waitFor("warning cleared", () => useConnectionStore().refreshError === "" && !heroWarning(mounted.root));
    assert.deepEqual({
      secretMasked: secretMasked(maskedKey(mounted.root), NEW_PRIMARY),
      copyDisabled: isDisabled(controlByLabel(hero(mounted.root), "复制 Key")),
      writes: keyWrites().length,
      success: messageCount("success"),
      showsNew: text(hero(mounted.root)).includes(NEW_PRIMARY),
      showsOld: text(hero(mounted.root)).includes(OLD_PRIMARY),
    }, {
      secretMasked: true,
      copyDisabled: false,
      writes: 1,
      success: 1,
      showsNew: false,
      showsOld: false,
    });
  } finally {
    mounted.app.unmount();
    restoreTransport();
  }
});

test("a failed dashboard read retry keeps the unknown secret and the acknowledged rotate", async () => {
  const mounted = await mountDashboard();
  try {
    const read = await rotateUntilRead(mounted.root);
    read.resolve(503, { code: "unavailable", message: READ_FAILURE });
    await waitFor("hero read warning", () => Boolean(heroWarning(mounted.root)));
    const alert = heroWarning(mounted.root);
    if (!alert) throw new Error("failed read should render a warning inside the key panel");
    const successAtRetry = messageCount("success");
    const writesAtRetry = keyWrites().length;
    const retrying = startClick(retryButton(alert));
    const retry = await pendingCall("failed read retry", (call) => call.method === "GET" && call.path.endsWith("/connection"));
    retry.resolve(503, { code: "unavailable", message: READ_FAILURE });
    await retrying;
    await settle();
    assert.deepEqual({
      secretMasked: secretMasked(maskedKey(mounted.root), ""),
      warning: useConnectionStore().refreshError === READ_FAILURE && Boolean(heroWarning(mounted.root)),
      writes: keyWrites().length - writesAtRetry,
      success: messageCount("success"),
      primary: useConnectionStore().info?.primary_key ?? null,
      laptop: text(hero(mounted.root)).includes("Laptop"),
    }, {
      secretMasked: true,
      warning: true,
      writes: 0,
      success: successAtRetry,
      primary: "",
      laptop: true,
    });
  } finally {
    mounted.app.unmount();
    restoreTransport();
  }
});

test("a dashboard connection read resolving after logout does not restore the old secret or emit a message", async () => {
  const mounted = await mountDashboard();
  try {
    const read = await rotateUntilRead(mounted.root);
    const messages = pageMessages().length;
    useSessionStore().dropSession();
    await settle();
    read.resolve(200, connectionWire(OLD_PRIMARY, 4));
    await settle();
    assert.deepEqual({
      info: useConnectionStore().info,
      messages: pageMessages().length - messages,
      visible: text(mounted.root).includes(OLD_PRIMARY),
      writes: keyWrites().length,
    }, {
      info: null,
      messages: 0,
      visible: false,
      writes: 1,
    });
  } finally {
    mounted.app.unmount();
    restoreTransport();
  }
});

test("a dashboard connection read resolving after unmount keeps the canonical secret without a page callback", async () => {
  const first = await mountDashboard();
  const pinia = getActivePinia();
  if (!pinia) throw new Error("active pinia should exist");
  const read = await rotateUntilRead(first.root);
  const messages = pageMessages().length;
  const copies = copiedValues.length;
  const writes = keyWrites().length;
  first.app.unmount();
  setActivePinia(pinia);
  read.resolve(200, connectionWire(NEW_PRIMARY, 9));
  await settle();
  assert.deepEqual({
    primary: useConnectionStore().info?.primary_key ?? null,
    revision: useConnectionStore().info?.revision ?? null,
    refreshError: useConnectionStore().refreshError,
    messages: pageMessages().length - messages,
    copied: copiedValues.slice(copies),
    writes: keyWrites().length - writes,
  }, {
    primary: NEW_PRIMARY,
    revision: 9,
    refreshError: "",
    messages: 0,
    copied: [],
    writes: 0,
  });
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Dashboard);
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  try {
    const load = pendingCall("remount connection GET", (call) => call.method === "GET" && call.path.endsWith("/connection"));
    await waitFor("cached canonical", () => {
      try {
        return secretMasked(maskedKey(root), NEW_PRIMARY);
      } catch {
        return false;
      }
    });
    const remountRead = await load;
    assert.equal(remountRead.method, "GET");
    assert.equal(text(hero(root)).includes(OLD_PRIMARY), false);
    assert.equal(text(hero(root)).includes(NEW_PRIMARY), false);
    remountRead.resolve(200, connectionWire(NEW_PRIMARY, 9));
    await waitFor("remount usable", () => {
      try {
        return !isDisabled(controlByLabel(hero(root), "复制 Key"))
          && !isDisabled(controlByLabel(hero(root), "刷新 Key"))
          && !isLoading(controlByLabel(hero(root), "刷新 Key"));
      } catch {
        return false;
      }
    });
    assert.equal(secretMasked(maskedKey(root), NEW_PRIMARY), true);
    assert.equal(useConnectionStore().info?.primary_key, NEW_PRIMARY);
    assert.equal(pageMessages().length - messages, 0);
    assert.deepEqual(copiedValues.slice(copies), []);
    assert.equal(keyWrites().length, writes);
  } finally {
    app.unmount();
    restoreTransport();
  }
});

test("an older dashboard rotate read cannot restore its secret over a newer rotate", async () => {
  const mounted = await mountDashboard();
  try {
    const first = await rotateUntilRead(mounted.root);
    const writes = keyWrites().length;
    const secondDone = confirmRotate(mounted.root);
    const secondWrite = await pendingCall("second rotate", (call) => call.method !== "GET" && call.path.includes("/keys"));
    assert.equal(keyWrites().length > writes, true);
    secondWrite.resolve(200, { processGeneration: 3, revision: 10 });
    const second = await pendingCall("newer connection GET", (call) => (
      call !== first && call.method === "GET" && call.path.endsWith("/connection")
    ));
    const messages = pageMessages().length;
    first.resolve(200, connectionWire(OLD_PRIMARY, 4));
    await settle();
    assert.deepEqual({
      primary: useConnectionStore().info?.primary_key ?? null,
      visible: text(hero(mounted.root)).includes(OLD_PRIMARY),
      messages: pageMessages().length - messages,
      writes: keyWrites().length,
    }, {
      primary: "",
      visible: false,
      messages: 0,
      writes: 2,
    });
    second.resolve(200, connectionWire(NEW_PRIMARY, 11));
    await secondDone;
    await waitFor("newer secret", () => useConnectionStore().info?.primary_key === NEW_PRIMARY);
    assert.equal(secretMasked(maskedKey(mounted.root), NEW_PRIMARY), true);
    assert.equal(isDisabled(controlByLabel(hero(mounted.root), "复制 Key")), false);
    assert.equal(text(hero(mounted.root)).includes(OLD_PRIMARY), false);
  } finally {
    mounted.app.unmount();
    restoreTransport();
  }
});
