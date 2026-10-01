import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { createPinia, getActivePinia, setActivePinia } from "pinia";
import { defineComponent, h, KeepAlive, reactive, ref, ssrContextKey, type App, type Component, type Ref } from "vue";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { useConnectionStore } from "../stores/connection.ts";
import { useSessionStore } from "../stores/session.ts";
import { maskConnectionKey } from "./dashboard-connection.ts";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

type ConnectionSubKey = { id: string; name: string; enabled: boolean; value: string };
type ConnectionInfo = {
  gateway_port: number;
  client_root_url: string;
  primary_key: string;
  sub_keys: ConnectionSubKey[];
  revision: number;
};

type ConnectionStub = {
  info: ConnectionInfo;
  updateImpl: (id: string, update: { name?: string; enabled?: boolean }) => Promise<void>;
  load: () => Promise<ConnectionInfo>;
  updateKey: (id: string, update: { name?: string; enabled?: boolean }) => Promise<void>;
};

let buildDir: string;
let Keys: Component;
const renderer = createVueHostRenderer();

function keysHarnessPlugin() {
  const prefix = "\0keys-component-harness:";
  const modules: Record<string, string> = {
    naive: `
      import { defineComponent, h } from "vue";
      const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
      } });
      export const NButton = defineComponent({
        inheritAttrs: false,
        props: {
          size: String,
          type: String,
          secondary: Boolean,
          quaternary: Boolean,
          circle: Boolean,
          disabled: Boolean,
          loading: Boolean,
        },
        setup(props, { attrs, slots }) {
          return () => h("button", { ...attrs, ...props }, slots.default?.());
        },
      });
      export const NInput = defineComponent({
        inheritAttrs: false,
        props: { value: { type: String, default: "" } },
        emits: ["update:value"],
        setup(props, { attrs, emit }) {
          return () => h("input", {
            ...attrs,
            value: props.value,
            "onUpdate:value": (value) => emit("update:value", value),
          });
        },
      });
      export const NAlert = pass;
      export const NIcon = pass;
      export const NSwitch = defineComponent({ inheritAttrs: false, props: { value: Boolean }, setup(props, { attrs }) {
        return () => h("button", { ...attrs, role: "switch", "aria-checked": props.value });
      } });
      export const NTooltip = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
        return () => h("div", [slots.trigger?.(), slots.default?.()]);
      } });
      export const NPopconfirm = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
        return () => h("div", [slots.trigger?.(), slots.default?.()]);
      } });
      export const useMessage = () => {
        const record = (type) => (...args) => { (globalThis.__keysMessages ??= []).push({ type, args }); };
        return { error: record("error"), success: record("success"), warning: record("warning") };
      };
    `,
    dashboard: `
      export class DashboardRequestError extends Error {
        constructor(message, status = 0, code = "") {
          super(message);
          this.name = "DashboardRequestError";
          this.status = status;
          this.code = code;
        }
      }
      globalThis.__KeysDashboardRequestError = DashboardRequestError;
    `,
    connection: `export const useConnectionStore = () => globalThis.__keysConnection;`,
    session: `export const useSessionStore = () => ({ authenticated: true });`,
    i18n: `export const t = (key, values = {}) => key.replace(/\\{(\\w+)\\}/g, (_, name) => String(values[name] ?? ""));`,
    clipboard: `
      import { ref } from "vue";
      const copiedTarget = ref("");
      export const useClipboard = () => ({ copiedTarget, copy: async () => {}, cleanup: () => {} });
    `,
    icons: `
      import { defineComponent } from "vue";
      const icon = defineComponent({ setup() { return () => null; } });
      export const CheckOutlined = icon;
      export const CopyOutlined = icon;
      export const DeleteOutlined = icon;
      export const EditOutlined = icon;
      export const ReloadOutlined = icon;
    `,
  };
  const sources: Record<string, string> = {
    "naive-ui": "naive",
    "@vicons/antd": "icons",
    "../api/dashboard": "dashboard",
    "../api/dashboard.ts": "dashboard",
    "../stores/connection.ts": "connection",
    "../stores/session.ts": "session",
    "../i18n/index.ts": "i18n",
    "../utils/format.ts": "clipboard",
  };
  return {
    name: "keys-component-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      if (source === "@vicons/antd") return `${prefix}icons`;
      const importerPath = importer?.replaceAll("\\", "/") ?? "";
      if (!importerPath.includes("/src/views/Keys.vue")) return null;
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/views/Keys.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

function requestError(status: number, code = ""): Error {
  const Ctor = (globalThis as { __KeysDashboardRequestError?: new (message: string, status?: number, code?: string) => Error })
    .__KeysDashboardRequestError;
  if (!Ctor) throw new Error("DashboardRequestError stub missing");
  return new Ctor("key write failed", status, code);
}

function createConnection(): ConnectionStub {
  const info: ConnectionInfo = {
    gateway_port: 9042,
    client_root_url: "",
    primary_key: "primary-live",
    sub_keys: [{ id: "sub-1", name: "Laptop", enabled: true, value: "sub-live" }],
    revision: 7,
  };
  const stub: ConnectionStub = reactive({
    info,
    updateImpl: async (_id: string, _update: { name?: string; enabled?: boolean }) => {},
    async load() {
      return this.info;
    },
    async updateKey(id: string, update: { name?: string; enabled?: boolean }) {
      await this.updateImpl(id, update);
      if (update.name !== undefined) {
        const entry = this.info.sub_keys.find((item) => item.id === id);
        if (entry) entry.name = update.name;
      }
    },
  });
  return stub;
}

function byClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter(
    (node) => typeof node.props.class === "string" && String(node.props.class).split(" ").includes(className),
  );
}

function subKeyRow(root: HostNode): HostNode {
  const row = byClass(root, "gateway-key-row").find((node) => !String(node.props.class).includes("gateway-key-row--primary"));
  if (!row) throw new Error("sub-Key row should render");
  return row;
}

function renameInput(root: HostNode): HostNode {
  const found = walkHostNodes(subKeyRow(root)).find(
    (node) => node.type === "input" && !String(node.props.class ?? "").includes("key-create-input"),
  );
  if (!found) throw new Error("rename draft input should render");
  return found;
}

function tinyButtons(root: HostNode): HostNode[] {
  return walkHostNodes(subKeyRow(root)).filter((node) => node.type === "button" && node.props.size === "tiny");
}

async function startRename(root: HostNode): Promise<void> {
  const edit = tinyButtons(root)[0];
  if (!edit) throw new Error("rename control should render");
  await (edit.props.onClick as () => void)();
  await settle();
}

async function typeDraft(root: HostNode, name: string): Promise<void> {
  const input = renameInput(root);
  await (input.props["onUpdate:value"] as (value: string) => void)(name);
  await settle();
}

async function saveRename(root: HostNode): Promise<void> {
  const save = tinyButtons(root).find((node) => node.props.secondary);
  if (!save) throw new Error("rename save control should render");
  await (save.props.onClick as () => Promise<void>)();
  await settle();
}

async function mount(connection: ConnectionStub): Promise<{ app: App; root: HostNode; connection: ConnectionStub }> {
  installTestWindow();
  (globalThis as { __keysMessages?: Array<{ type: string }> }).__keysMessages = [];
  (globalThis as { __keysConnection?: ConnectionStub }).__keysConnection = connection;
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Keys);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  return { app, root, connection };
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts", "frontend-logic-repair", "core-tests");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "keys-component-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [keysHarnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/views/Keys.vue"),
        fileName: () => "keys.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue"] },
    },
    esbuild: { target: "esnext" },
  });
  Keys = (await import(pathToFileURL(path.join(buildDir, "keys.mjs")).href)).default;
});

after(async () => {
  await rm(buildDir, { force: true, recursive: true });
});

test("Key rename keeps the draft after a revision conflict", async () => {
  const connection = createConnection();
  connection.updateImpl = async (_id: string, _update: { name?: string; enabled?: boolean }) => {
    throw requestError(409, "revisionConflict");
  };
  const mounted = await mount(connection);
  try {
    await startRename(mounted.root);
    await typeDraft(mounted.root, "Studio");
    await saveRename(mounted.root);
    const input = walkHostNodes(subKeyRow(mounted.root)).find(
      (node) => node.type === "input" && !String(node.props.class ?? "").includes("key-create-input"),
    );
    assert.ok(input, "conflict must keep the rename draft");
    assert.equal(input.props.value, "Studio");
    assert.equal(tinyButtons(mounted.root).some((node) => node.props.secondary), true);
  } finally {
    mounted.app.unmount();
  }
});

test("Key rename keeps the draft after an ordinary write error", async () => {
  const connection = createConnection();
  connection.updateImpl = async (_id: string, _update: { name?: string; enabled?: boolean }) => {
    throw new Error("write unavailable");
  };
  const mounted = await mount(connection);
  try {
    await startRename(mounted.root);
    await typeDraft(mounted.root, "Studio");
    await saveRename(mounted.root);
    const input = walkHostNodes(subKeyRow(mounted.root)).find(
      (node) => node.type === "input" && !String(node.props.class ?? "").includes("key-create-input"),
    );
    assert.ok(input, "write error must keep the rename draft");
    assert.equal(input.props.value, "Studio");
  } finally {
    mounted.app.unmount();
  }
});

test("Key rename clears the draft after a successful write", async () => {
  const connection = createConnection();
  const mounted = await mount(connection);
  try {
    await startRename(mounted.root);
    await typeDraft(mounted.root, "Studio");
    await saveRename(mounted.root);
    assert.equal(
      walkHostNodes(subKeyRow(mounted.root)).some((node) => node.type === "input" && !String(node.props.class ?? "").includes("key-create-input")),
      false,
    );
    assert.equal(connection.info.sub_keys[0]?.name, "Studio");
  } finally {
    mounted.app.unmount();
  }
});

const READ_FAILURE = "connection-read-failed";
const OLD_PRIMARY = "ocg-old-primary-1111";
const NEW_PRIMARY = "ocg-new-primary-2222";
const LAPTOP_SECRET = "ocg-sub-laptop-aaaa";
const PHONE_SECRET = "ocg-sub-phone-bbbb";

type HeldCall = {
  method: string;
  path: string;
  body: string | null;
  pending: boolean;
  resolve: (status: number, payload: unknown) => void;
};

type PageMessage = { type: string };

let liveBuildDir = "";
let KeysLive: Component;
const liveRenderer = createVueHostRenderer();
const heldCalls: HeldCall[] = [];
const copiedSecrets: string[] = [];
let restoreFetch: (() => void) | null = null;
let restoreClipboard: (() => void) | null = null;

function pageMessages(): PageMessage[] {
  const slot = globalThis as { __keyReadMessages?: PageMessage[] };
  slot.__keyReadMessages ??= [];
  return slot.__keyReadMessages;
}

function messageCount(type: string): number {
  return pageMessages().filter((message) => message.type === type).length;
}

function liveHarnessPlugin() {
  const prefix = "\0keys-live-harness:";
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
    export const NInput = defineComponent({
      inheritAttrs: false,
      props: { value: { type: String, default: "" }, disabled: Boolean, size: String },
      emits: ["update:value"],
      setup(props, { attrs, emit }) {
        return () => h("input", { ...attrs, ...props, value: props.value, "onUpdate:value": (value) => emit("update:value", value) });
      },
    });
    export const NIcon = pass;
    export const NSwitch = defineComponent({ inheritAttrs: false, props: { value: Boolean }, setup(props, { attrs }) {
      return () => h("button", { ...attrs, role: "switch", "aria-checked": String(props.value) });
    } });
    export const NTooltip = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
      return () => h("div", [...(slots.trigger?.() ?? []), ...(slots.default?.() ?? [])]);
    } });
    export const NPopconfirm = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { class: "popconfirm" }, [
        ...(slots.trigger?.() ?? []),
        h("button", { class: "popconfirm-positive", onClick: () => invoke(attrs.onPositiveClick) }),
      ]);
    } });
    export const useMessage = () => {
      const record = (type) => () => { (globalThis.__keyReadMessages ??= []).push({ type }); };
      return { error: record("error"), success: record("success"), warning: record("warning"), info: record("info") };
    };
  `;
  const icons = `
    import { defineComponent } from "vue";
    const icon = defineComponent({ setup() { return () => null; } });
    export const CheckOutlined = icon;
    export const CopyOutlined = icon;
    export const DeleteOutlined = icon;
    export const EditOutlined = icon;
    export const ReloadOutlined = icon;
  `;
  return {
    name: "keys-live-harness",
    enforce: "pre" as const,
    resolveId(source: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      if (source === "@vicons/antd") return `${prefix}icons`;
      return null;
    },
    load(id: string) {
      if (id.includes("/src/views/Keys.vue?vue&type=style")) return "";
      if (id === `${prefix}naive`) return naive;
      if (id === `${prefix}icons`) return icons;
      return null;
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
    subKeys: [
      { id: "laptop", name: "Laptop", enabled: true, value: LAPTOP_SECRET },
      { id: "phone", name: "Phone", enabled: true, value: PHONE_SECRET },
    ],
  };
}

function installLiveTransport(): void {
  heldCalls.splice(0, heldCalls.length);
  pageMessages().splice(0, pageMessages().length);
  copiedSecrets.splice(0, copiedSecrets.length);
  const previousFetch = globalThis.fetch;
  const fetchMock: typeof fetch = async (input, init) => {
    const url = requestTarget(input);
    const method = (init?.method ?? "GET").toUpperCase();
    const body = typeof init?.body === "string" ? init.body : null;
    if (!url.pathname.endsWith("/connection") && !url.pathname.includes("/keys")) {
      return jsonResponse(404, { code: "notFound", message: `${method} ${url.pathname}` });
    }
    return await new Promise((resolve) => {
      const call: HeldCall = {
        method,
        path: url.pathname,
        body,
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
        copiedSecrets.push(value);
      },
    },
  });
  restoreClipboard = () => {
    if (previousClipboard) Object.defineProperty(nav, "clipboard", previousClipboard);
    else Reflect.deleteProperty(nav, "clipboard");
    restoreClipboard = null;
  };
}

function restoreLiveTransport(): void {
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

function primaryRow(root: HostNode): HostNode {
  const row = walkHostNodes(root).find((node) => hasClass(node, "gateway-key-row--primary"));
  if (!row) throw new Error("primary key row should render");
  return row;
}

function rowByName(root: HostNode, name: string): HostNode {
  const row = walkHostNodes(root).find((node) => (
    hasClass(node, "gateway-key-row") && !hasClass(node, "gateway-key-row--primary") && text(node).includes(name)
  ));
  if (!row) throw new Error(`key row ${name} should render`);
  return row;
}

function buttonsIn(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.type === "button");
}

function controlByLabel(root: HostNode, label: string): HostNode {
  const found = buttonsIn(root).find((node) => node.props["aria-label"] === label || node.props.ariaLabel === label);
  if (!found) throw new Error(`control ${label} should render`);
  return found;
}

function warningAlert(root: HostNode): HostNode | undefined {
  return walkHostNodes(root).find((node) => node.props.role === "alert" && node.props["data-alert-type"] === "warning");
}

function retryButton(alert: HostNode): HostNode {
  const button = walkHostNodes(alert).find((node) => node.type === "button");
  if (!button) throw new Error("read warning should expose a retry button");
  return button;
}

function primaryCode(root: HostNode): string {
  const code = walkHostNodes(primaryRow(root)).find((node) => hasClass(node, "gateway-key-value"));
  if (!code) throw new Error("primary key code should render");
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

function confirmPrimaryRotate(root: HostNode): Promise<void> {
  const positive = walkHostNodes(primaryRow(root)).find((node) => hasClass(node, "popconfirm-positive"));
  if (!positive) throw new Error("primary rotate confirmation should render");
  return startClick(positive);
}

async function openOtherKeyDraft(root: HostNode, name: string, draft: string): Promise<HostNode> {
  const row = rowByName(root, name);
  const rename = buttonsIn(row).find((node) => node.props.size === "tiny");
  if (!rename) throw new Error("rename control should render");
  await startClick(rename);
  await settle();
  const input = walkHostNodes(row).find((node) => node.type === "input");
  if (!input) throw new Error("rename draft should render");
  const update = input.props["onUpdate:value"];
  if (typeof update !== "function") throw new Error("rename draft should accept input");
  await update(draft);
  await settle();
  return row;
}

function draftValue(row: HostNode): string | null {
  const value = walkHostNodes(row).find((node) => node.type === "input")?.props.value;
  return typeof value === "string" ? value : null;
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

async function mountLive(): Promise<{ app: App; root: HostNode }> {
  const view = installTestWindow();
  Object.assign(view.location, { origin: "http://127.0.0.1" });
  installLiveTransport();
  const pinia = createPinia();
  setActivePinia(pinia);
  authenticate();
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = liveRenderer.createApp(KeysLive);
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  const load = await pendingCall("initial connection GET", (call) => call.method === "GET" && call.path.endsWith("/connection"));
  load.resolve(200, connectionWire(OLD_PRIMARY));
  await waitFor("keys loaded", () => {
    try {
      return useConnectionStore().info?.primary_key === OLD_PRIMARY
        && !isDisabled(controlByLabel(primaryRow(root), "刷新 Key"));
    } catch {
      return false;
    }
  });
  return { app, root };
}

async function mountKept(): Promise<{ app: App; root: HostNode; showing: Ref<boolean> }> {
  const view = installTestWindow();
  Object.assign(view.location, { origin: "http://127.0.0.1" });
  installLiveTransport();
  const pinia = createPinia();
  setActivePinia(pinia);
  authenticate();
  const showing = ref(true);
  const away = defineComponent({
    name: "KeysAway",
    setup() {
      return () => h("div", { class: "keys-away" });
    },
  });
  const host = defineComponent({
    name: "KeysKeepAliveHost",
    setup() {
      return () => h(KeepAlive, null, {
        default: () => (showing.value ? h(KeysLive) : h(away)),
      });
    },
  });
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = liveRenderer.createApp(host);
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  const load = await pendingCall("initial connection GET", (call) => call.method === "GET" && call.path.endsWith("/connection"));
  load.resolve(200, connectionWire(OLD_PRIMARY));
  await waitFor("keys loaded", () => {
    try {
      return useConnectionStore().info?.primary_key === OLD_PRIMARY
        && !isDisabled(controlByLabel(primaryRow(root), "刷新 Key"));
    } catch {
      return false;
    }
  });
  return { app, root, showing };
}

function showsClass(root: HostNode, className: string): boolean {
  return walkHostNodes(root).some((node) => hasClass(node, className));
}

async function rotateUntilRead(root: HostNode): Promise<{ read: HeldCall; done: Promise<void> }> {
  const reads = connectionReads().length;
  const done = confirmPrimaryRotate(root);
  const write = await pendingCall("primary rotate", (call) => call.method !== "GET" && call.path.includes("/keys"));
  write.resolve(200, { processGeneration: 3, revision: 8 });
  const read = await pendingCall("deferred connection GET", (call) => (
    call.method === "GET" && call.path.endsWith("/connection")
  ));
  await waitFor("rotate released", () => connectionReads().length > reads && !isLoading(controlByLabel(primaryRow(root), "刷新 Key")));
  return { read, done };
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts", "frontend-logic-repair", "keys-read-retry");
  await mkdir(artifactsDir, { recursive: true });
  liveBuildDir = await mkdtemp(path.join(artifactsDir, "keys-live-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [liveHarnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/views/Keys.vue"),
        fileName: () => "keys-live.mjs",
        formats: ["es"],
      },
      outDir: liveBuildDir,
      rollupOptions: { external: ["vue", "pinia"] },
    },
    esbuild: { target: "esnext" },
  });
  KeysLive = (await import(pathToFileURL(path.join(liveBuildDir, "keys-live.mjs")).href)).default;
});

after(async () => {
  restoreLiveTransport();
  if (liveBuildDir) await rm(liveBuildDir, { force: true, recursive: true });
});

test("a confirmed primary rotate releases at ack, warns on the deferred read, and retries with one GET", async () => {
  const mounted = await mountLive();
  let turned: { read: HeldCall; done: Promise<void> } | null = null;
  try {
    const phone = await openOtherKeyDraft(mounted.root, "Phone", "Studio");
    turned = await rotateUntilRead(mounted.root);
    assert.deepEqual({
      writes: keyWrites().length,
      pendingReads: connectionReads().filter((call) => call.pending).length,
      loading: isLoading(controlByLabel(primaryRow(mounted.root), "刷新 Key")),
      copyDisabled: isDisabled(controlByLabel(primaryRow(mounted.root), "复制 Key")),
      secretMasked: secretMasked(primaryCode(mounted.root), ""),
      warning: Boolean(warningAlert(mounted.root)),
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
    turned.read.resolve(503, { code: "unavailable", message: READ_FAILURE });
    await turned.done;
    await waitFor("read warning", () => useConnectionStore().refreshError === READ_FAILURE && Boolean(warningAlert(mounted.root)));
    const alert = warningAlert(mounted.root);
    if (!alert) throw new Error("failed read should render a warning alert");
    assert.equal(text(alert).includes(READ_FAILURE), true);
    assert.equal(secretMasked(primaryCode(mounted.root), ""), true);
    assert.equal(isDisabled(controlByLabel(primaryRow(mounted.root), "复制 Key")), true);
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
    await waitFor("warning cleared", () => useConnectionStore().refreshError === "" && !warningAlert(mounted.root));
    assert.deepEqual({
      secretMasked: secretMasked(primaryCode(mounted.root), NEW_PRIMARY),
      copyDisabled: isDisabled(controlByLabel(primaryRow(mounted.root), "复制 Key")),
      writes: keyWrites().length,
      success: messageCount("success"),
      showsNew: text(mounted.root).includes(NEW_PRIMARY),
      showsOld: text(mounted.root).includes(OLD_PRIMARY),
      draft: draftValue(phone),
      laptop: text(mounted.root).includes("Laptop"),
    }, {
      secretMasked: true,
      copyDisabled: false,
      writes: 1,
      success: 1,
      showsNew: false,
      showsOld: false,
      draft: "Studio",
      laptop: true,
    });
  } finally {
    await turned?.done.then(() => undefined, () => undefined);
    mounted.app.unmount();
    restoreLiveTransport();
  }
});

test("a failed read retry keeps the other key draft and the acknowledged rotate", async () => {
  const mounted = await mountLive();
  let turned: { read: HeldCall; done: Promise<void> } | null = null;
  try {
    const phone = await openOtherKeyDraft(mounted.root, "Phone", "Studio");
    turned = await rotateUntilRead(mounted.root);
    turned.read.resolve(503, { code: "unavailable", message: READ_FAILURE });
    await turned.done;
    await waitFor("read warning", () => Boolean(warningAlert(mounted.root)));
    const alert = warningAlert(mounted.root);
    if (!alert) throw new Error("failed read should render a warning alert");
    const successAtRetry = messageCount("success");
    const writesAtRetry = keyWrites().length;
    const retrying = startClick(retryButton(alert));
    const retry = await pendingCall("failed read retry", (call) => call.method === "GET" && call.path.endsWith("/connection"));
    retry.resolve(503, { code: "unavailable", message: READ_FAILURE });
    await retrying;
    await settle();
    assert.deepEqual({
      draft: draftValue(phone),
      laptop: text(mounted.root).includes("Laptop"),
      secretMasked: secretMasked(primaryCode(mounted.root), ""),
      warning: useConnectionStore().refreshError === READ_FAILURE && Boolean(warningAlert(mounted.root)),
      writes: keyWrites().length - writesAtRetry,
      success: messageCount("success"),
      primary: useConnectionStore().info?.primary_key ?? null,
    }, {
      draft: "Studio",
      laptop: true,
      secretMasked: true,
      warning: true,
      writes: 0,
      success: successAtRetry,
      primary: "",
    });
  } finally {
    await turned?.done.then(() => undefined, () => undefined);
    mounted.app.unmount();
    restoreLiveTransport();
  }
});

test("a connection read resolving after logout does not restore the old secret or emit a message", async () => {
  const mounted = await mountLive();
  const turned = await rotateUntilRead(mounted.root);
  try {
    const messages = pageMessages().length;
    useSessionStore().dropSession();
    await settle();
    turned.read.resolve(200, connectionWire(OLD_PRIMARY, 4));
    await turned.done;
    await settle();
    assert.deepEqual({
      info: useConnectionStore().info,
      messages: pageMessages().length - messages,
      copied: copiedSecrets.includes(OLD_PRIMARY),
      visible: text(mounted.root).includes(OLD_PRIMARY),
      writes: keyWrites().length,
    }, {
      info: null,
      messages: 0,
      copied: false,
      visible: false,
      writes: 1,
    });
  } finally {
    await turned.done.then(() => undefined, () => undefined);
    mounted.app.unmount();
    restoreLiveTransport();
  }
});

test("a connection read resolving after unmount keeps the canonical secret and does not copy or toast", async () => {
  const first = await mountLive();
  const pinia = getActivePinia();
  if (!pinia) throw new Error("active pinia should exist");
  const turned = await rotateUntilRead(first.root);
  const messages = pageMessages().length;
  const copies = copiedSecrets.length;
  const writes = keyWrites().length;
  first.app.unmount();
  setActivePinia(pinia);
  turned.read.resolve(200, connectionWire(NEW_PRIMARY, 9));
  await turned.done.then(() => undefined, () => undefined);
  await settle();
  assert.deepEqual({
    primary: useConnectionStore().info?.primary_key ?? null,
    revision: useConnectionStore().info?.revision ?? null,
    refreshError: useConnectionStore().refreshError,
    messages: pageMessages().length - messages,
    copied: copiedSecrets.slice(copies),
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
  const app = liveRenderer.createApp(KeysLive);
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  try {
    const load = pendingCall("remount connection GET", (call) => call.method === "GET" && call.path.endsWith("/connection"));
    await waitFor("cached canonical", () => {
      try {
        return secretMasked(primaryCode(root), NEW_PRIMARY);
      } catch {
        return false;
      }
    });
    const remountRead = await load;
    assert.equal(remountRead.method, "GET");
    assert.equal(text(root).includes(OLD_PRIMARY), false);
    assert.equal(text(root).includes(NEW_PRIMARY), false);
    remountRead.resolve(200, connectionWire(NEW_PRIMARY, 9));
    await waitFor("remount usable", () => {
      try {
        return !isDisabled(controlByLabel(primaryRow(root), "复制 Key"))
          && !isDisabled(controlByLabel(primaryRow(root), "刷新 Key"))
          && !isLoading(controlByLabel(primaryRow(root), "刷新 Key"));
      } catch {
        return false;
      }
    });
    assert.equal(secretMasked(primaryCode(root), NEW_PRIMARY), true);
    assert.equal(useConnectionStore().info?.primary_key, NEW_PRIMARY);
    assert.equal(pageMessages().length - messages, 0);
    assert.deepEqual(copiedSecrets.slice(copies), []);
    assert.equal(keyWrites().length, writes);
  } finally {
    app.unmount();
    restoreLiveTransport();
  }
});

test("a deferred key read during KeepAlive deactivation updates the cache without auto-copy and stays usable on return", async () => {
  const mounted = await mountKept();
  const turned = await rotateUntilRead(mounted.root);
  const messages = pageMessages().length;
  const copies = copiedSecrets.length;
  const writes = keyWrites().length;
  const reads = connectionReads().length;
  try {
    mounted.showing.value = false;
    await settle(40);
    assert.equal(showsClass(mounted.root, "keys-away"), true);
    assert.equal(showsClass(mounted.root, "keys-page"), false);
    turned.read.resolve(200, connectionWire(NEW_PRIMARY, 9));
    await turned.done.then(() => undefined, () => undefined);
    await settle();
    assert.deepEqual({
      primary: useConnectionStore().info?.primary_key ?? null,
      revision: useConnectionStore().info?.revision ?? null,
      refreshError: useConnectionStore().refreshError,
      messages: pageMessages().length - messages,
      copied: copiedSecrets.slice(copies),
      writes: keyWrites().length - writes,
    }, {
      primary: NEW_PRIMARY,
      revision: 9,
      refreshError: "",
      messages: 0,
      copied: [],
      writes: 0,
    });
    mounted.showing.value = true;
    await settle(40);
    const reactivation = connectionReads().slice(reads).filter((call) => call.pending);
    assert.equal(keyWrites().length, writes);
    for (const call of reactivation) {
      assert.equal(call.method, "GET");
      call.resolve(200, connectionWire(NEW_PRIMARY, 9));
    }
    await waitFor("reactivated key", () => {
      try {
        return showsClass(mounted.root, "keys-page")
          && secretMasked(primaryCode(mounted.root), NEW_PRIMARY)
          && !isDisabled(controlByLabel(primaryRow(mounted.root), "复制 Key"))
          && !isLoading(controlByLabel(primaryRow(mounted.root), "刷新 Key"));
      } catch {
        return false;
      }
    });
    assert.equal(text(mounted.root).includes(OLD_PRIMARY), false);
    assert.equal(text(mounted.root).includes(NEW_PRIMARY), false);
    assert.equal(useConnectionStore().info?.primary_key, NEW_PRIMARY);
    assert.equal(pageMessages().length - messages, 0);
    assert.deepEqual(copiedSecrets.slice(copies), []);
    assert.equal(keyWrites().length, writes);
  } finally {
    await turned.done.then(() => undefined, () => undefined);
    mounted.app.unmount();
    restoreLiveTransport();
  }
});

test("an older rotate read cannot restore its secret over a newer rotate", async () => {
  const mounted = await mountLive();
  const first = await rotateUntilRead(mounted.root);
  let secondDone: Promise<void> | null = null;
  try {
    const writes = keyWrites().length;
    secondDone = confirmPrimaryRotate(mounted.root);
    const secondWrite = await pendingCall("second rotate", (call) => call.pending && call.method !== "GET" && call.path.includes("/keys"));
    assert.equal(keyWrites().length > writes, true);
    secondWrite.resolve(200, { processGeneration: 3, revision: 10 });
    const second = await pendingCall("newer connection GET", (call) => (
      call !== first.read && call.method === "GET" && call.path.endsWith("/connection")
    ));
    const messages = pageMessages().length;
    first.read.resolve(200, connectionWire(OLD_PRIMARY, 4));
    await first.done;
    await settle();
    assert.deepEqual({
      primary: useConnectionStore().info?.primary_key ?? null,
      visible: text(mounted.root).includes(OLD_PRIMARY),
      messages: pageMessages().length - messages,
      copied: copiedSecrets.includes(OLD_PRIMARY),
      writes: keyWrites().length,
    }, {
      primary: "",
      visible: false,
      messages: 0,
      copied: false,
      writes: 2,
    });
    second.resolve(200, connectionWire(NEW_PRIMARY, 11));
    await secondDone;
    await waitFor("newer secret", () => useConnectionStore().info?.primary_key === NEW_PRIMARY);
    assert.equal(secretMasked(primaryCode(mounted.root), NEW_PRIMARY), true);
    assert.equal(isDisabled(controlByLabel(primaryRow(mounted.root), "复制 Key")), false);
    assert.equal(text(mounted.root).includes(OLD_PRIMARY), false);
  } finally {
    await first.done.then(() => undefined, () => undefined);
    await secondDone?.then(() => undefined, () => undefined);
    mounted.app.unmount();
    restoreLiveTransport();
  }
});
