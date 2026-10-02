import assert from "node:assert/strict";
import path from "node:path";
import { afterEach, before, describe, test } from "node:test";
import { pathToFileURL } from "node:url";
import { createPinia, setActivePinia } from "pinia";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { defineComponent, h, nextTick, provide, inject, ssrContextKey, type App, type Component } from "vue";
import {
  createVueHostRenderer,
  installTestWindow,
  walkHostNodes,
  type HostNode,
  type TestWindow,
} from "../test-helpers/vue-host-runtime.ts";

const PAGE_HREF = "http://127.0.0.1:9042/dashboard/index.html?lane=keep#/settings";
const LOADED_PROXY = "http://last-good.example:8080";
const PROXY_A = "http://draft-a.example:8080";
const PROXY_B = "http://draft-b.example:8080";
const CANONICAL_PROXY = "http://canonical-proxy.example:8080";
const POISON_PROXY = "http://poison.example:9";
const LOADED_ROOT = "http://canonical.example";
const NORMALIZED_ROOT = "http://normalized.example";
const radioKey = Symbol("ocg-settings-radio");

type PageMessage = { type: string };
type SettingsComponent = new () => unknown;
type SessionStore = {
  authenticated: boolean;
  applyStatus: (status: {
    authenticated: boolean;
    initialized: boolean;
    local: boolean;
    processGeneration: number;
    revision: number;
  }) => void;
  dropSession: () => void;
};
type SettingsStore = {
  canonicalConfirmed: boolean;
  refreshError: string;
  settings: {
    gateway_port: number;
    process_generation: number;
    proxy_url: string;
    revision: number;
    stream_idle_timeout_secs: number;
  } | null;
};

type SettingsCall = {
  method: string;
  path: string;
  body: Record<string, unknown> | null;
  pending: boolean;
  status: number | null;
  resolve: (status: number, body: object) => void;
  reject: (error: unknown) => void;
};

const renderer = createVueHostRenderer();
const calls: SettingsCall[] = [];
const unknownPaths: string[] = [];
const renderErrors: string[] = [];
const floating: Promise<unknown>[] = [];

let Settings: SettingsComponent;
let useSettingsStore: () => SettingsStore;
let useSessionStore: () => SessionStore;
let testWindow: TestWindow;
let currentApp: App | null = null;
let immediateSettingsGet: { status: number; body: object } | null = null;

if (typeof globalThis.MouseEvent !== "function") {
  class MouseEvent extends Event {
    constructor(type: string, init?: EventInit) {
      super(type, init);
    }
  }
  Object.defineProperty(globalThis, "MouseEvent", { configurable: true, value: MouseEvent });
}

function memoryStorage(): Storage {
  const backing = new Map<string, string>();
  return {
    get length() { return backing.size; },
    clear: () => backing.clear(),
    getItem: (key) => backing.get(key) ?? null,
    key: (index) => [...backing.keys()][index] ?? null,
    removeItem: (key) => { backing.delete(key); },
    setItem: (key, value) => { backing.set(key, String(value)); },
  };
}

function installDocument(): object {
  const document = {
    visibilityState: "visible",
    hidden: false,
    documentElement: { lang: "", style: {} },
    body: { style: {} },
    addEventListener() {},
    removeEventListener() {},
    head: { appendChild() {}, removeChild() {} },
    getElementById() { return null; },
    querySelector() { return null; },
    querySelectorAll() { return []; },
    createElement() {
      return { setAttribute() {}, click() {}, style: {}, appendChild() {} };
    },
  };
  Object.defineProperty(globalThis, "document", { configurable: true, value: document });
  return document;
}

function pageMessages(): PageMessage[] {
  const host = globalThis as { __ocgPageMessages?: PageMessage[] };
  host.__ocgPageMessages ??= [];
  return host.__ocgPageMessages;
}

function installUiKit(): void {
  const pass = defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("div", attrs, slots.default?.());
    },
  });
  const known = new Map<string, unknown>();
  const remember = (name: string, value: unknown) => {
    known.set(name, value);
    return value;
  };
  const generic = (name: string) => defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("div", { ...attrs, "data-ui": name }, ["default", "trigger", "header", "footer", "icon", "extra", "action", "title"]
        .flatMap((slot) => slots[slot]?.() ?? []));
    },
  });
  const NButton = defineComponent({
    inheritAttrs: false,
    props: ["disabled", "loading", "type"],
    setup(props, { attrs, slots }) {
      return () => h("button", {
        ...attrs,
        type: "button",
        disabled: props.disabled || props.loading ? true : undefined,
        "data-loading": props.loading ? "true" : "false",
        "data-variant": props.type ?? "",
        onClick: () => {
          if (props.disabled || props.loading) return undefined;
          const click = attrs.onClick as (() => unknown) | undefined;
          return typeof click === "function" ? click() : undefined;
        },
      }, slots.default?.());
    },
  });
  const NSwitch = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled", "loading"],
    emits: ["update:value"],
    setup(props, { attrs, emit }) {
      return () => h("button", {
        ...attrs,
        type: "button",
        role: "switch",
        "aria-checked": props.value ? "true" : "false",
        disabled: props.disabled || props.loading ? true : undefined,
        "data-loading": props.loading ? "true" : "false",
        onClick: () => {
          if (props.disabled || props.loading) return;
          emit("update:value", !props.value);
        },
      });
    },
  });
  const field = (numeric: boolean) => defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled", "readonly", "inputProps"],
    emits: ["update:value"],
    setup(props, { attrs, emit }) {
      return () => {
        const declared = props.inputProps as Record<string, unknown> | undefined;
        const fallen = (attrs.inputProps ?? attrs["input-props"]) as Record<string, unknown> | undefined;
        const extra = declared ?? fallen ?? {};
        return h("input", {
          ...extra,
          class: attrs.class,
          value: props.value ?? "",
          disabled: props.disabled ? true : undefined,
          readOnly: props.readonly ? true : undefined,
          onInput: (payload: unknown) => {
            if (props.disabled || props.readonly) return;
            const raw = typeof payload === "string" || typeof payload === "number" || typeof payload === "boolean"
              ? payload
              : (payload as { target?: { value?: unknown } } | null)?.target?.value ?? "";
            const next = numeric && typeof raw === "string" && raw.trim() !== "" && Number.isFinite(Number(raw))
              ? Number(raw)
              : raw;
            emit("update:value", next);
          },
        });
      };
    },
  });
  const NRadioGroup = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled"],
    emits: ["update:value"],
    setup(props, { attrs, slots, emit }) {
      provide(radioKey, {
        value: () => props.value,
        select: (value: unknown) => emit("update:value", value),
      });
      return () => h("div", { ...attrs, role: "radiogroup" }, slots.default?.());
    },
  });
  const NRadio = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled"],
    setup(props, { attrs, slots }) {
      const group = inject<{ value: () => unknown; select: (value: unknown) => void } | null>(radioKey, null);
      return () => h("button", {
        ...attrs,
        type: "button",
        "data-choice": String(props.value),
        "aria-pressed": group?.value() === props.value ? "true" : "false",
        disabled: props.disabled ? true : undefined,
        onClick: () => {
          if (!props.disabled) group?.select(props.value);
        },
      }, slots.default?.());
    },
  });
  const NAlert = defineComponent({
    inheritAttrs: false,
    props: ["type", "title"],
    setup(props, { attrs, slots }) {
      return () => h("div", { ...attrs, type: props.type, role: "alert" }, [props.title, slots.default?.()]);
    },
  });
  const NIcon = defineComponent({
    inheritAttrs: false,
    props: ["component"],
    setup(_props, { attrs, slots }) {
      return () => h("span", attrs, slots.default?.());
    },
  });
  const useMessage = () => {
    const record = (type: string) => () => { pageMessages().push({ type }); };
    return { success: record("success"), warning: record("warning"), error: record("error"), info: record("info") };
  };
  const specials: Record<string, unknown> = {
    NAlert, NButton, NCheckbox: pass, NForm: pass, NFormItem: pass, NIcon, NInput: field(false),
    NInputNumber: field(true), NPopconfirm: pass, NProgress: pass, NRadio, NRadioGroup, NSwitch, useMessage,
  };
  const kit = {
    component(name: string) {
      if (known.has(name)) return known.get(name);
      return remember(name, specials[name] ?? generic(name));
    },
    namespace() {
      return new Proxy({}, {
        get: (_target, key) => kit.component(String(key)),
      });
    },
  };
  Object.assign(globalThis, { __ocgUi: kit });
}

function uiKind(source: string): string | null {
  if (source === "naive-ui" || (source.startsWith("naive-ui/") && !source.includes("/locales/"))) return "naive";
  if (source === "@vicons/antd" || source.startsWith("@vicons/antd/")) return "icon";
  if (source === "motion-v") return "motion";
  return null;
}

function uiBindings(clause: string): string | null {
  const trimmed = clause.trim();
  if (!trimmed) return "";
  const star = trimmed.match(/^\*\s+as\s+([A-Za-z_$][\w$]*)$/);
  if (star) return `const ${star[1]} = globalThis.__ocgUi.namespace();`;
  let rest = trimmed;
  const lines: string[] = [];
  const fallback = rest.match(/^([A-Za-z_$][\w$]*)\s*(?:,\s*)?/);
  if (fallback && !rest.startsWith("{") && !rest.startsWith("type") && !rest.startsWith("*")) {
    lines.push(`const ${fallback[1]} = globalThis.__ocgUi.component("default");`);
    rest = rest.slice(fallback[0].length).trim();
  }
  if (!rest) return lines.join("\n");
  if (!rest.startsWith("{")) return null;
  const body = rest.slice(1, rest.lastIndexOf("}"));
  for (const part of body.split(",")) {
    const piece = part.trim();
    if (!piece) continue;
    const typed = piece.match(/^(type\s+)?([A-Za-z_$][\w$]*)(?:\s+as\s+([A-Za-z_$][\w$]*))?$/);
    if (!typed) return null;
    if (typed[1]) continue;
    lines.push(`const ${typed[3] ?? typed[2]} = globalThis.__ocgUi.component(${JSON.stringify(typed[2])});`);
  }
  return lines.join("\n");
}

function uiStubPlugin() {
  const pattern = /import\s+(type\s+)?([\s\S]*?)\s+from\s+["']([^"']+)["']/g;
  return {
    name: "ocg-settings-ui-stub",
    enforce: "pre" as const,
    transform(code: string, id: string) {
      const normalized = id.replaceAll("\\", "/");
      if (!normalized.includes("/src/") || normalized.includes("/node_modules/")) return null;
      let changed = false;
      const next = code.replace(pattern, (full, typeOnly: string | undefined, clause: string, source: string) => {
        if (!uiKind(source)) return full;
        changed = true;
        if (typeOnly) return "";
        return uiBindings(clause) ?? full;
      });
      return changed ? next : null;
    },
    load(id: string) {
      const normalized = id.replaceAll("\\", "/");
      if (normalized.includes("type=style") || normalized.endsWith(".css") || normalized.includes(".css?")) return "export default {}";
      return null;
    },
  };
}

function classTokens(value: unknown): string[] {
  if (typeof value === "string") return value.split(/\s+/).filter(Boolean);
  if (Array.isArray(value)) return value.flatMap(classTokens);
  if (value && typeof value === "object") {
    return Object.entries(value as Record<string, unknown>).filter(([, on]) => Boolean(on)).map(([key]) => key);
  }
  return [];
}

function hasClass(node: HostNode, className: string): boolean {
  return classTokens(node.props.class).includes(className);
}

function byClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter((node) => hasClass(node, className));
}

function region(root: HostNode, label: string): HostNode | undefined {
  return walkHostNodes(root).find((node) => node.props["aria-labelledby"] === label || node.props.ariaLabelledby === label);
}

function outline(root: HostNode): string {
  return walkHostNodes(root).slice(0, 80).map((node) => {
    const classes = classTokens(node.props.class).slice(0, 3).join(".");
    const mark = node.props["aria-labelledby"] ?? node.props.id ?? node.props.role ?? node.props["data-variant"] ?? "";
    return `${node.type}${classes ? `.${classes}` : ""}${mark ? `#${mark}` : ""}`;
  }).join(" | ");
}

function messageCount(type: string): number {
  return pageMessages().filter((message) => message.type === type).length;
}

function warningAlerts(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props.type === "warning");
}

function hostText(node: HostNode): string {
  const title = typeof node.props.title === "string" ? node.props.title : "";
  const own = typeof node.text === "string" ? node.text : "";
  return [title, own, ...node.children.map(hostText)].join("\n");
}

function warningSignals(root: HostNode): number {
  return messageCount("warning") + warningAlerts(root).length;
}

function canonicalReadNotice(root: HostNode, detail: string): HostNode | undefined {
  if (detail === "") return undefined;
  return warningAlerts(root).find((node) => hostText(node).includes(detail));
}

function retryControl(notice: HostNode): HostNode | undefined {
  return walkHostNodes(notice).find((node) => node.type === "button");
}

function inputIn(root: HostNode, className: string): HostNode {
  const host = byClass(root, className)[0];
  const input = host && walkHostNodes(host).find((node) => node.type === "input");
  if (!input) throw new Error(`fixture: no input in .${className}\n${outline(root)}`);
  return input;
}

function timeoutInput(root: HostNode, index: number): HostNode {
  const field = byClass(root, "timeout-field")[index];
  const input = field && walkHostNodes(field).find((node) => node.type === "input");
  if (!input) throw new Error(`fixture: timeout field ${index} missing\n${outline(root)}`);
  return input;
}

function saveButton(root: HostNode): HostNode {
  const host = region(root, "forwarding-title");
  const button = host && walkHostNodes(host).find((node) => node.type === "button" && node.props["data-variant"] === "primary");
  if (!button) throw new Error(`fixture: save control missing\n${host ? outline(host) : outline(root)}`);
  return button;
}

function recoveryAnchor(root: HostNode): HostNode | undefined {
  const host = byClass(root, "gateway-port-field")[0];
  return host ? walkHostNodes(host).find((node) => node.type === "a" && typeof node.props.href === "string") : undefined;
}

function readControl(node: HostNode): string | number | boolean | null {
  const value = node.props.value;
  return typeof value === "string" || typeof value === "number" || typeof value === "boolean" ? value : null;
}

function writeControl(node: HostNode, value: string | number): void {
  const handler = node.props.onInput as ((payload: unknown) => void) | undefined;
  if (typeof handler !== "function") throw new Error("fixture: field has no input handler");
  if (node.props.disabled === true) throw new Error("fixture: refused to type into a disabled field");
  handler(value);
}

function fire(node: HostNode): void {
  const click = node.props.onClick as (() => unknown) | undefined;
  if (typeof click !== "function") throw new Error("fixture: control has no click handler");
  const result = click();
  if (result && typeof (result as Promise<unknown>).then === "function") {
    floating.push((result as Promise<unknown>).catch((error: unknown) => {
      renderErrors.push(error instanceof Error ? error.message : String(error));
    }));
  }
}

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 8; attempt += 1) {
    await nextTick();
    await Promise.resolve();
  }
  await new Promise((resolve) => setImmediate(resolve));
  await nextTick();
}

async function waitFor(label: string, predicate: () => boolean, root?: HostNode): Promise<void> {
  for (let attempt = 0; attempt < 30; attempt += 1) {
    if (predicate()) return;
    await nextTick();
    await Promise.resolve();
  }
  const detail = [
    `fixture: timed out waiting for ${label}`,
    root ? outline(root) : "",
    renderErrors.join("\n"),
    unknownPaths.length ? `unhandled ${unknownPaths.join(", ")}` : "",
  ].filter(Boolean).join("\n");
  throw new Error(detail);
}

function requestPath(input: unknown): string {
  const raw = typeof input === "string"
    ? input
    : input instanceof URL
      ? input.href
      : input && typeof input === "object" && "url" in input
        ? String((input as { url: unknown }).url)
        : String(input);
  const clean = raw.split("?")[0] ?? raw;
  if (clean.startsWith("http://") || clean.startsWith("https://")) return new URL(clean).pathname;
  return clean;
}

function isSettingsResource(pathname: string): boolean {
  return pathname.endsWith("/dashboard/api/v4/settings");
}

function jsonResponse(status: number, body: object): Response {
  const statusText = status === 200 ? "OK" : status === 409 ? "Conflict" : status === 500 ? "Internal Server Error" : "Error";
  return new Response(JSON.stringify(body), {
    status,
    statusText,
    headers: { "Content-Type": "application/json" },
  });
}

function settingsWire(revision: number, extra: Record<string, unknown> = {}): object {
  return {
    revision,
    processGeneration: 99,
    gatewayPort: 9042,
    gatewayPortFromEnv: false,
    proxyMode: "manual",
    proxyUrl: LOADED_PROXY,
    proxyListDirection: "whitelist",
    proxyListModels: ["canonical-model"],
    proxySupportedModels: [],
    opencodeInviteUrl: "https://example.test/invite",
    clientRootUrl: LOADED_ROOT,
    clientRootUrlFromEnv: false,
    autoStart: false,
    autoStartSupported: true,
    showDockIcon: true,
    dockVisibilitySupported: false,
    connectTimeoutSecs: 30,
    nonStreamTimeoutSecs: 900,
    streamIdleTimeoutSecs: 300,
    routingMode: "strict-priority",
    conversationSticky: true,
    ...extra,
  };
}

function nestedRevision(): object {
  return { revision: 7, processGeneration: 99, pricingRevision: "hist" };
}

function ancillary(pathname: string): object | null {
  if (pathname.endsWith("/settings/update-status")) {
    return { phase: "idle", downloaded: 0, total: null, error: null, currentVersion: "1.0.0", installSupported: false };
  }
  if (pathname.endsWith("/routing/temporary-unavailability/restrictions")) {
    return { restrictions: [], revision: nestedRevision() };
  }
  if (pathname.endsWith("/routing/temporary-unavailability")) {
    return { builtins: [], rules: [], revision: nestedRevision() };
  }
  if (pathname.endsWith("/routing/cards")) {
    return { cards: [], credentials: [], destinations: [], revision: nestedRevision() };
  }
  if (pathname.endsWith("/contract")) return { revision: 7, processGeneration: 99 };
  return null;
}

function installFetch(): void {
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (input: unknown, init: RequestInit = {}) => {
      const pathname = requestPath(input);
      const method = init.method ?? "GET";
      const body = init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null;
      if (method === "GET" && isSettingsResource(pathname) && immediateSettingsGet) {
        const prepared = immediateSettingsGet;
        immediateSettingsGet = null;
        calls.push({
          method,
          path: pathname,
          body,
          pending: false,
          status: prepared.status,
          resolve: () => undefined,
          reject: () => undefined,
        });
        return Promise.resolve(jsonResponse(prepared.status, prepared.body));
      }
      if (isSettingsResource(pathname)) {
        const promise = new Promise<Response>((resolve, reject) => {
          const call: SettingsCall = {
            method,
            path: pathname,
            body,
            pending: true,
            status: null,
            resolve: (status, payload) => {
              call.pending = false;
              call.status = status;
              resolve(jsonResponse(status, payload));
            },
            reject: (error) => {
              call.pending = false;
              reject(error);
            },
          };
          calls.push(call);
        });
        void promise.catch(() => undefined);
        return promise;
      }
      const payload = ancillary(pathname);
      if (!payload) {
        unknownPaths.push(`${method} ${pathname}`);
        return Promise.resolve(jsonResponse(404, { code: "notFound", message: "fixture has no handler" }));
      }
      return Promise.resolve(jsonResponse(200, payload));
    },
  });
}

function settingsSince(index: number, method?: string): SettingsCall[] {
  return calls.slice(index).filter((call) => isSettingsResource(call.path) && (!method || call.method === method));
}

async function expectSettings(method: string): Promise<SettingsCall> {
  let found: SettingsCall | undefined;
  await waitFor(`${method} /settings`, () => {
    found = [...calls].reverse().find((call) => call.pending && call.method === method && isSettingsResource(call.path));
    return Boolean(found);
  });
  if (!found) throw new Error(`fixture: ${method} /settings was not issued`);
  return found;
}

async function settleSettings(callsToClose: SettingsCall[], status: number, body: object): Promise<void> {
  for (const call of callsToClose) {
    if (call.pending) call.resolve(status, body);
  }
  await flushUi();
}

function authenticate(): void {
  useSessionStore().applyStatus({
    authenticated: true,
    initialized: true,
    local: true,
    revision: 7,
    processGeneration: 99,
  });
}

function unmountCurrent(): void {
  const app = currentApp;
  currentApp = null;
  app?.unmount();
}

async function boot(wire: object = settingsWire(7)): Promise<{ root: HostNode }> {
  unmountCurrent();
  pageMessages().splice(0, pageMessages().length);
  renderErrors.splice(0, renderErrors.length);
  const pinia = createPinia();
  setActivePinia(pinia);
  authenticate();
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Settings as Component, { themeName: "default", resolvedTheme: "white" });
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.config.errorHandler = (error) => {
    renderErrors.push(error instanceof Error ? error.stack ?? error.message : String(error));
  };
  currentApp = app;
  app.mount(root);
  const load = await expectSettings("GET");
  load.resolve(200, wire);
  await waitFor("save enabled", () => {
    const button = saveButton(root);
    return button.props.disabled !== true && button.props["data-loading"] === "false";
  }, root);
  await waitFor("policy section", () => walkHostNodes(root).some((node) => (
    node.props["data-section"] === "temporary-unavailability" && node.props["data-loaded"] === "true"
  )), root);
  await flushUi();
  if (renderErrors.length > 0) throw new Error(`fixture: page render failed\n${renderErrors.join("\n")}`);
  const proxy = inputIn(root, "proxy-settings");
  const clientRoot = inputIn(root, "client-root-field");
  if (typeof proxy.props["aria-label"] !== "string" || proxy.props["aria-label"] === "") {
    throw new Error("fixture: proxy field did not receive its accessible name");
  }
  if (clientRoot.props["aria-describedby"] !== "client-root-help") {
    throw new Error("fixture: client root field did not keep its description id");
  }
  if (!region(root, "startup-title") || region(root, "dock-icon-title")) {
    throw new Error(`fixture: initial capability sections did not match the loaded settings\n${outline(root)}`);
  }
  pageMessages().splice(0, pageMessages().length);
  return { root };
}

function canonicalWire(extra: Record<string, unknown> = {}): object {
  return settingsWire(8, {
    processGeneration: 100,
    proxyUrl: CANONICAL_PROXY,
    clientRootUrl: NORMALIZED_ROOT,
    streamIdleTimeoutSecs: 120,
    autoStartSupported: false,
    dockVisibilitySupported: true,
    showDockIcon: true,
    ...extra,
  });
}

describe("settings page canonical confirmation", { concurrency: false }, () => {
  before(async () => {
    const localStorage = memoryStorage();
    const sessionStorage = memoryStorage();
    Object.defineProperty(globalThis, "localStorage", { configurable: true, value: localStorage });
    testWindow = installTestWindow({ href: PAGE_HREF, pathname: "/dashboard/index.html", search: "?lane=keep" });
    const location = testWindow.location as TestWindow["location"] & Record<string, unknown>;
    const live = () => new URL(location.href);
    for (const key of ["origin", "hash", "host", "hostname", "port", "protocol"] as const) {
      Object.defineProperty(location, key, { configurable: true, enumerable: true, get: () => live()[key] });
    }
    Object.assign(testWindow, {
      localStorage,
      sessionStorage,
      document: installDocument(),
      navigator: globalThis.navigator,
      dispatchEvent: () => true,
    });
    installUiKit();
    installFetch();
    const outDir = path.resolve(".artifacts/frontend-logic-repair/settings-ui-tests/client");
    await build({
      configFile: false,
      root: process.cwd(),
      mode: "production",
      logLevel: "error",
      cacheDir: path.resolve(".artifacts/frontend-logic-repair/settings-ui-tests/vite-cache"),
      plugins: [uiStubPlugin(), vue()],
      resolve: { alias: { "@": path.resolve("src") } },
      build: {
        emptyOutDir: true,
        target: "esnext",
        lib: {
          entry: path.resolve("src/test-helpers/settings-entry.ts"),
          fileName: () => "bundle.mjs",
          formats: ["es"],
        },
        minify: false,
        outDir,
        rollupOptions: {
          external: ["vue", "pinia", "vue-router"],
          output: { inlineDynamicImports: true },
        },
      },
    });
    const client = await import(pathToFileURL(path.join(outDir, "bundle.mjs")).href);
    Settings = client.Settings;
    useSettingsStore = client.useSettingsStore;
    useSessionStore = client.useSessionStore;
  }, { timeout: 180000 });

  afterEach(() => {
    unmountCurrent();
    for (const call of calls) {
      if (call.pending) call.reject(new Error("unsettled settings fetch"));
    }
    calls.splice(0, calls.length);
    unknownPaths.splice(0, unknownPaths.length);
    immediateSettingsGet = null;
    pageMessages().splice(0, pageMessages().length);
    renderErrors.splice(0, renderErrors.length);
    testWindow.location.href = PAGE_HREF;
    testWindow.__opened.splice(0, testWindow.__opened.length);
  });

  test("an acknowledged save releases before the deferred read, and a failed read warns until a read-only retry", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_A);
    await flushUi();
    fire(saveButton(mounted.root));
    const put = await expectSettings("PUT");
    assert.equal(timeoutInput(mounted.root, 0).props.disabled, undefined);
    writeControl(timeoutInput(mounted.root, 0), 21);
    put.resolve(200, { revision: 8, processGeneration: 99 });
    await waitFor("save released", () => saveButton(mounted.root).props["data-loading"] === "false", mounted.root);
    const pendingRead = settingsSince(mark, "GET");
    assert.deepEqual({
      loading: saveButton(mounted.root).props["data-loading"],
      puts: settingsSince(mark, "PUT").length,
      pendingGets: pendingRead.filter((call) => call.pending).length,
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      connect: readControl(timeoutInput(mounted.root, 0)),
    }, {
      loading: "false",
      puts: 1,
      pendingGets: 1,
      proxy: PROXY_A,
      connect: 21,
    });
    pendingRead[0]?.reject(new Error("canonical read failed"));
    await flushUi();
    assert.deepEqual({
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      connect: readControl(timeoutInput(mounted.root, 0)),
      warning: warningSignals(mounted.root) > 0,
      error: messageCount("error"),
      confirmed: useSettingsStore().canonicalConfirmed,
      refreshError: useSettingsStore().refreshError !== "",
    }, {
      puts: 1,
      gets: 1,
      proxy: PROXY_A,
      connect: 21,
      warning: true,
      error: 0,
      confirmed: false,
      refreshError: true,
    });
    const readFailure = useSettingsStore().refreshError;
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_B);
    await flushUi();
    const notice = canonicalReadNotice(mounted.root, readFailure);
    const retry = notice ? retryControl(notice) : undefined;
    assert.equal(settingsSince(mark, "PUT").length, 1);
    assert.equal(settingsSince(mark, "GET").length, 1);
    assert.equal(readControl(inputIn(mounted.root, "proxy-settings")), PROXY_B);
    assert.ok(retry, "the canonical read warning exposes a read-only retry");
    fire(retry);
    const reread = await expectSettings("GET");
    assert.equal(reread.method, "GET");
    assert.equal(reread.body, null);
    assert.equal(settingsSince(mark, "PUT").length, 1);
    assert.equal(settingsSince(mark, "GET").length, 2);
    reread.resolve(200, canonicalWire());
    for (let attempt = 0; attempt < 30; attempt += 1) {
      const adopted = readControl(timeoutInput(mounted.root, 2)) === 120
        && warningAlerts(mounted.root).length === 0
        && useSettingsStore().refreshError === "";
      if (adopted) break;
      await nextTick();
      await Promise.resolve();
    }
    await flushUi();
    await flushUi();
    const store = useSettingsStore();
    assert.deepEqual({
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      connect: readControl(timeoutInput(mounted.root, 0)),
      stream: readControl(timeoutInput(mounted.root, 2)),
      clientRoot: readControl(inputIn(mounted.root, "client-root-field")),
      startup: Boolean(region(mounted.root, "startup-title")),
      dock: Boolean(region(mounted.root, "dock-icon-title")),
      warnings: warningAlerts(mounted.root).length,
      error: messageCount("error"),
      refreshError: store.refreshError,
      confirmed: store.canonicalConfirmed,
      storeProxy: store.settings?.proxy_url ?? null,
      storeStream: store.settings?.stream_idle_timeout_secs ?? null,
      revision: store.settings?.revision ?? null,
      process: store.settings?.process_generation ?? null,
    }, {
      puts: 1,
      gets: 2,
      proxy: PROXY_B,
      connect: 21,
      stream: 120,
      clientRoot: NORMALIZED_ROOT,
      startup: false,
      dock: true,
      warnings: 0,
      error: 0,
      refreshError: "",
      confirmed: true,
      storeProxy: CANONICAL_PROXY,
      storeStream: 120,
      revision: 8,
      process: 100,
    });
  });

  test("a canonical read keeps in-flight edits and the next save uses that revision and process", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_A);
    await flushUi();
    fire(saveButton(mounted.root));
    const firstPut = await expectSettings("PUT");
    assert.equal(firstPut.body?.connectTimeoutSecs, 30);
    assert.equal(firstPut.body?.proxyUrl, PROXY_A);
    assert.equal("showDockIcon" in (firstPut.body ?? {}), false);
    writeControl(timeoutInput(mounted.root, 0), 21);
    firstPut.resolve(200, { revision: 8, processGeneration: 99 });
    await waitFor("save released before read", () => saveButton(mounted.root).props["data-loading"] === "false", mounted.root);
    assert.notEqual(inputIn(mounted.root, "proxy-settings").props.disabled, true);
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_B);
    const firstRead = settingsSince(mark, "GET");
    assert.equal(firstRead.length, 1);
    firstRead[0]?.resolve(200, canonicalWire());
    await waitFor("canonical stream", () => readControl(timeoutInput(mounted.root, 2)) === 120, mounted.root);
    await flushUi();
    assert.deepEqual({
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      connect: readControl(timeoutInput(mounted.root, 0)),
      stream: readControl(timeoutInput(mounted.root, 2)),
      clientRoot: readControl(inputIn(mounted.root, "client-root-field")),
      startup: Boolean(region(mounted.root, "startup-title")),
      dock: Boolean(region(mounted.root, "dock-icon-title")),
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
    }, {
      proxy: PROXY_B,
      connect: 21,
      stream: 120,
      clientRoot: NORMALIZED_ROOT,
      startup: false,
      dock: true,
      puts: 1,
      gets: 1,
    });
    fire(saveButton(mounted.root));
    const secondPut = await expectSettings("PUT");
    const secondGetsBeforeAck = settingsSince(mark, "GET").length;
    assert.deepEqual({
      expectedRevision: secondPut.body?.expectedRevision,
      processGeneration: secondPut.body?.processGeneration,
      proxyUrl: secondPut.body?.proxyUrl,
      connectTimeoutSecs: secondPut.body?.connectTimeoutSecs,
      streamIdleTimeoutSecs: secondPut.body?.streamIdleTimeoutSecs,
      clientRootUrl: secondPut.body?.clientRootUrl,
      sendsAutoStart: "autoStart" in (secondPut.body ?? {}),
      showDockIcon: secondPut.body?.showDockIcon,
    }, {
      expectedRevision: 8,
      processGeneration: 100,
      proxyUrl: PROXY_B,
      connectTimeoutSecs: 21,
      streamIdleTimeoutSecs: 120,
      clientRootUrl: NORMALIZED_ROOT,
      sendsAutoStart: false,
      showDockIcon: true,
    });
    secondPut.resolve(200, { revision: 9, processGeneration: 100 });
    const secondRead = await expectSettings("GET");
    assert.equal(secondPut.status, 200);
    assert.equal(settingsSince(mark, "PUT").length, 2);
    assert.equal(settingsSince(mark, "GET").length, secondGetsBeforeAck + 1);
    secondRead.resolve(200, canonicalWire({ revision: 9 }));
    await flushUi();
    assert.equal(messageCount("error"), 0);
  });

  test("a canonical read that settles before the save continuation is not reverted", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    immediateSettingsGet = { status: 200, body: canonicalWire() };
    fire(saveButton(mounted.root));
    const put = await expectSettings("PUT");
    assert.equal(put.body?.expectedRevision, 7);
    assert.equal(put.body?.processGeneration, 99);
    put.resolve(200, { revision: 8, processGeneration: 99 });
    await flushUi();
    await flushUi();
    assert.deepEqual({
      stream: readControl(timeoutInput(mounted.root, 2)),
      clientRoot: readControl(inputIn(mounted.root, "client-root-field")),
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      startup: Boolean(region(mounted.root, "startup-title")),
      dock: Boolean(region(mounted.root, "dock-icon-title")),
      gets: settingsSince(mark, "GET").length,
      loading: saveButton(mounted.root).props["data-loading"],
    }, {
      stream: 120,
      clientRoot: NORMALIZED_ROOT,
      proxy: CANONICAL_PROXY,
      startup: false,
      dock: true,
      gets: 1,
      loading: "false",
    });
    fire(saveButton(mounted.root));
    const second = await expectSettings("PUT");
    assert.equal(second.body?.expectedRevision, 8);
    assert.equal(second.body?.processGeneration, 100);
    assert.equal(second.body?.streamIdleTimeoutSecs, 120);
    assert.equal("autoStart" in (second.body ?? {}), false);
    second.resolve(200, { revision: 9, processGeneration: 100 });
    await settleSettings(settingsSince(calls.length - 1, "GET"), 200, canonicalWire({ revision: 9 }));
  });

  test("a revision conflict reloads once, keeps the edit, and the next save uses the recovery pair", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_A);
    await flushUi();
    fire(saveButton(mounted.root));
    const put = await expectSettings("PUT");
    put.resolve(409, {
      code: "revisionConflict",
      message: "settings changed since they were loaded; reload and try again",
      currentRevision: 11,
      processGeneration: 99,
    });
    await waitFor("conflict recovery read", () => settingsSince(mark, "GET").length >= 1, mounted.root);
    const recovery = settingsWire(11, {
      proxyUrl: CANONICAL_PROXY,
      clientRootUrl: NORMALIZED_ROOT,
      streamIdleTimeoutSecs: 90,
    });
    await settleSettings(settingsSince(mark, "GET"), 200, recovery);
    await settleSettings(settingsSince(mark, "GET"), 200, recovery);
    await flushUi();
    assert.deepEqual({
      gets: settingsSince(mark, "GET").length,
      puts: settingsSince(mark, "PUT").length,
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      stream: readControl(timeoutInput(mounted.root, 2)),
      clientRoot: readControl(inputIn(mounted.root, "client-root-field")),
      warning: warningSignals(mounted.root) > 0,
      error: messageCount("error"),
    }, {
      gets: 1,
      puts: 1,
      proxy: PROXY_A,
      stream: 90,
      clientRoot: NORMALIZED_ROOT,
      warning: true,
      error: 0,
    });
    fire(saveButton(mounted.root));
    const retry = await expectSettings("PUT");
    assert.equal(retry.body?.expectedRevision, 11);
    assert.equal(retry.body?.processGeneration, 99);
    assert.equal(retry.body?.proxyUrl, PROXY_A);
    assert.equal(retry.body?.streamIdleTimeoutSecs, 90);
    retry.resolve(200, { revision: 12, processGeneration: 99 });
    await settleSettings(settingsSince(mark, "GET").filter((call) => call.pending), 200, settingsWire(12));
  });

  test("dropping the session during save does not toast or restore into the replacement page", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_A);
    writeControl(timeoutInput(mounted.root, 0), 21);
    await flushUi();
    fire(saveButton(mounted.root));
    const put = await expectSettings("PUT");
    const seen = pageMessages().length;
    const dropped = useSettingsStore();
    useSessionStore().dropSession();
    unmountCurrent();
    put.resolve(200, { revision: 8, processGeneration: 99 });
    await flushUi();
    await settleSettings(settingsSince(mark, "GET"), 200, settingsWire(8, {
      proxyUrl: POISON_PROXY,
      gatewayPort: 19042,
    }));
    assert.deepEqual({
      messages: pageMessages().length - seen,
      gets: settingsSince(mark, "GET").length,
      settings: dropped.settings,
      href: testWindow.location.href,
      opened: testWindow.__opened.length,
    }, {
      messages: 0,
      gets: 0,
      settings: null,
      href: PAGE_HREF,
      opened: 0,
    });
    const replacement = await boot();
    await flushUi();
    assert.deepEqual({
      proxy: readControl(inputIn(replacement.root, "proxy-settings")),
      connect: readControl(timeoutInput(replacement.root, 0)),
      loading: saveButton(replacement.root).props["data-loading"],
      recovery: recoveryAnchor(replacement.root)?.props.href ?? null,
      href: testWindow.location.href,
      opened: testWindow.__opened.length,
      messages: pageMessages().length,
    }, {
      proxy: LOADED_PROXY,
      connect: 30,
      loading: "false",
      recovery: null,
      href: PAGE_HREF,
      opened: 0,
      messages: 0,
    });
    assert.equal(readControl(inputIn(replacement.root, "proxy-settings")) === POISON_PROXY, false);
  });

  test("dropping the session during a host toggle does not toast or restore into the replacement page", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    const startup = region(mounted.root, "startup-title");
    const toggle = startup && walkHostNodes(startup).find((node) => node.props.role === "switch");
    if (!toggle) throw new Error("fixture: startup switch missing");
    fire(toggle);
    const put = await expectSettings("PUT");
    const seen = pageMessages().length;
    const dropped = useSettingsStore();
    useSessionStore().dropSession();
    unmountCurrent();
    put.resolve(200, { revision: 8, processGeneration: 99 });
    await flushUi();
    await settleSettings(settingsSince(mark, "GET"), 200, settingsWire(8, { proxyUrl: POISON_PROXY, autoStart: true }));
    assert.deepEqual({
      messages: pageMessages().length - seen,
      gets: settingsSince(mark, "GET").length,
      settings: dropped.settings,
      href: testWindow.location.href,
    }, {
      messages: 0,
      gets: 0,
      settings: null,
      href: PAGE_HREF,
    });
    const replacement = await boot();
    const replaced = region(replacement.root, "startup-title");
    const replacedToggle = replaced && walkHostNodes(replaced).find((node) => node.props.role === "switch");
    assert.equal(replacedToggle?.props["aria-checked"], "false");
    assert.equal(replacedToggle?.props["data-loading"], "false");
    assert.equal(readControl(inputIn(replacement.root, "proxy-settings")), LOADED_PROXY);
    assert.equal(pageMessages().length, 0);
    assert.equal(testWindow.location.href, PAGE_HREF);
  });

  test("a failed port bind keeps the newer draft and does not navigate", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    writeControl(inputIn(mounted.root, "gateway-port-field"), 19042);
    await flushUi();
    fire(saveButton(mounted.root));
    const put = await expectSettings("PUT");
    assert.equal(put.body?.gatewayPort, 19042);
    writeControl(timeoutInput(mounted.root, 0), 21);
    put.resolve(500, { code: "internal", message: "bind failed" });
    await flushUi();
    await settleSettings(settingsSince(mark, "GET"), 200, settingsWire(9, { gatewayPort: 9042 }));
    assert.deepEqual({
      port: readControl(inputIn(mounted.root, "gateway-port-field")),
      connect: readControl(timeoutInput(mounted.root, 0)),
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
      success: messageCount("success"),
      recovery: recoveryAnchor(mounted.root)?.props.href ?? null,
      href: testWindow.location.href,
      opened: testWindow.__opened.length,
    }, {
      port: 19042,
      connect: 21,
      puts: 1,
      gets: 0,
      success: 0,
      recovery: null,
      href: PAGE_HREF,
      opened: 0,
    });
  });

  test("manual port recovery preserves the path, query, and hash until the link is used", { timeout: 20000 }, async () => {
    const mounted = await boot();
    writeControl(inputIn(mounted.root, "gateway-port-field"), 19042);
    await flushUi();
    fire(saveButton(mounted.root));
    const put = await expectSettings("PUT");
    put.resolve(200, { revision: 8, processGeneration: 99 });
    const read = await expectSettings("GET");
    read.resolve(200, settingsWire(8, { gatewayPort: 19042 }));
    await flushUi();
    const anchor = recoveryAnchor(mounted.root);
    const href = typeof anchor?.props.href === "string" ? new URL(anchor.props.href) : null;
    assert.deepEqual({
      origin: href?.origin ?? null,
      pathname: href?.pathname ?? null,
      search: href?.search ?? null,
      hash: href?.hash ?? null,
      page: testWindow.location.href,
      opened: testWindow.__opened.length,
    }, {
      origin: "http://127.0.0.1:19042",
      pathname: "/dashboard/index.html",
      search: "?lane=keep",
      hash: "#/settings",
      page: PAGE_HREF,
      opened: 0,
    });
  });

  test("a host toggle sends only the changed field and the current pair", { timeout: 20000 }, async () => {
    const mounted = await boot();
    const mark = calls.length;
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_B);
    await flushUi();
    const startup = region(mounted.root, "startup-title");
    const toggle = startup && walkHostNodes(startup).find((node) => node.props.role === "switch");
    if (!toggle) throw new Error("fixture: startup switch missing");
    fire(toggle);
    const put = await expectSettings("PUT");
    assert.deepEqual({
      keys: Object.keys(put.body ?? {}).sort(),
      autoStart: put.body?.autoStart,
      expectedRevision: put.body?.expectedRevision,
      processGeneration: put.body?.processGeneration,
    }, {
      keys: ["autoStart", "expectedRevision", "processGeneration"],
      autoStart: true,
      expectedRevision: 7,
      processGeneration: 99,
    });
    put.resolve(200, { revision: 8, processGeneration: 99 });
    const read = await expectSettings("GET");
    assert.equal(settingsSince(mark, "PUT").length, 1);
    read.resolve(200, settingsWire(8, { autoStart: true, proxyUrl: CANONICAL_PROXY }));
    await flushUi();
    assert.equal(readControl(inputIn(mounted.root, "proxy-settings")), PROXY_B);
    const checked = region(mounted.root, "startup-title");
    const after = checked && walkHostNodes(checked).find((node) => node.props.role === "switch");
    assert.equal(after?.props["aria-checked"], "true");
    assert.equal(messageCount("error"), 0);
  });

  test("a page settings read still in flight cannot roll an acknowledged draft back to an older body", { timeout: 20000 }, async () => {
    const mounted = await boot();
    fire(saveButton(mounted.root));
    const setupPut = await expectSettings("PUT");
    setupPut.resolve(200, { revision: 8, processGeneration: 99 });
    const setupRead = await expectSettings("GET");
    setupRead.resolve(503, { code: "unavailable", message: "canonical unavailable" });
    await waitFor("canonical read warning", () => (
      useSettingsStore().refreshError !== "" && warningAlerts(mounted.root).length > 0
    ), mounted.root);
    const openerNotice = canonicalReadNotice(mounted.root, useSettingsStore().refreshError);
    const opener = openerNotice ? retryControl(openerNotice) : undefined;
    assert.ok(opener, "the canonical read warning exposes a read-only retry");
    const mark = calls.length;
    fire(opener);
    const warm = await expectSettings("GET");
    assert.deepEqual({
      method: warm.method,
      body: warm.body,
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
    }, {
      method: "GET",
      body: null,
      puts: 0,
      gets: 1,
    });
    writeControl(inputIn(mounted.root, "proxy-settings"), PROXY_A);
    await flushUi();
    fire(saveButton(mounted.root));
    const put = await expectSettings("PUT");
    assert.equal(warm.pending, true);
    assert.equal(settingsSince(mark, "GET").length, 1);
    assert.deepEqual({
      expectedRevision: put.body?.expectedRevision,
      processGeneration: put.body?.processGeneration,
      proxyUrl: put.body?.proxyUrl,
      connectTimeoutSecs: put.body?.connectTimeoutSecs,
    }, {
      expectedRevision: 7,
      processGeneration: 99,
      proxyUrl: PROXY_A,
      connectTimeoutSecs: 30,
    });
    assert.equal(timeoutInput(mounted.root, 0).props.disabled, undefined);
    writeControl(timeoutInput(mounted.root, 0), 21);
    put.resolve(200, { revision: 9, processGeneration: 100 });
    await waitFor("save released", () => saveButton(mounted.root).props["data-loading"] === "false", mounted.root);
    await waitFor("detached canonical read", () => (
      settingsSince(mark, "GET").some((call) => call.pending && call !== warm)
    ), mounted.root);
    const detached = settingsSince(mark, "GET").find((call) => call.pending && call !== warm);
    if (!detached) throw new Error("fixture: acknowledged save did not start a canonical read");
    assert.equal(settingsSince(mark, "PUT").length, 1);
    detached.resolve(503, { code: "unavailable", message: "canonical unavailable" });
    await waitFor("detached read failed", () => (
      useSettingsStore().refreshError !== "" && useSettingsStore().canonicalConfirmed === false
    ), mounted.root);
    warm.resolve(200, settingsWire(4, {
      processGeneration: 50,
      proxyUrl: POISON_PROXY,
      streamIdleTimeoutSecs: 45,
      clientRootUrl: "http://old-raw.example",
      gatewayPort: 19042,
      autoStartSupported: false,
      dockVisibilitySupported: true,
      showDockIcon: false,
    }));
    await flushUi();
    const raced = useSettingsStore();
    assert.deepEqual({
      confirmed: raced.canonicalConfirmed,
      revision: raced.settings?.revision ?? null,
      process: raced.settings?.process_generation ?? null,
      storeProxy: raced.settings?.proxy_url ?? null,
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      connect: readControl(timeoutInput(mounted.root, 0)),
      stream: readControl(timeoutInput(mounted.root, 2)),
      clientRoot: readControl(inputIn(mounted.root, "client-root-field")),
      port: readControl(inputIn(mounted.root, "gateway-port-field")),
      startup: Boolean(region(mounted.root, "startup-title")),
      dock: Boolean(region(mounted.root, "dock-icon-title")),
      recovery: recoveryAnchor(mounted.root)?.props.href ?? null,
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
      error: messageCount("error"),
    }, {
      confirmed: false,
      revision: 9,
      process: 100,
      storeProxy: LOADED_PROXY,
      proxy: PROXY_A,
      connect: 21,
      stream: 300,
      clientRoot: LOADED_ROOT,
      port: 9042,
      startup: true,
      dock: false,
      recovery: null,
      puts: 1,
      gets: 2,
      error: 0,
    });
    const retryNotice = canonicalReadNotice(mounted.root, raced.refreshError);
    const retry = retryNotice ? retryControl(retryNotice) : undefined;
    assert.ok(retry, "the canonical read warning exposes a read-only retry");
    fire(retry);
    const reread = await expectSettings("GET");
    assert.equal(reread.method, "GET");
    assert.equal(reread.body, null);
    assert.equal(settingsSince(mark, "PUT").length, 1);
    assert.equal(settingsSince(mark, "GET").length, 3);
    reread.resolve(200, canonicalWire({ revision: 9 }));
    await waitFor("canonical fields", () => (
      useSettingsStore().canonicalConfirmed === true
      && useSettingsStore().refreshError === ""
      && readControl(timeoutInput(mounted.root, 2)) === 120
      && readControl(timeoutInput(mounted.root, 0)) === 21
    ), mounted.root);
    await flushUi();
    const confirmed = useSettingsStore();
    assert.deepEqual({
      confirmed: confirmed.canonicalConfirmed,
      refreshError: confirmed.refreshError,
      revision: confirmed.settings?.revision ?? null,
      process: confirmed.settings?.process_generation ?? null,
      proxy: readControl(inputIn(mounted.root, "proxy-settings")),
      connect: readControl(timeoutInput(mounted.root, 0)),
      stream: readControl(timeoutInput(mounted.root, 2)),
      clientRoot: readControl(inputIn(mounted.root, "client-root-field")),
      startup: Boolean(region(mounted.root, "startup-title")),
      dock: Boolean(region(mounted.root, "dock-icon-title")),
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
    }, {
      confirmed: true,
      refreshError: "",
      revision: 9,
      process: 100,
      proxy: CANONICAL_PROXY,
      connect: 21,
      stream: 120,
      clientRoot: NORMALIZED_ROOT,
      startup: false,
      dock: true,
      puts: 1,
      gets: 3,
    });
    fire(saveButton(mounted.root));
    const second = await expectSettings("PUT");
    assert.deepEqual({
      expectedRevision: second.body?.expectedRevision,
      processGeneration: second.body?.processGeneration,
      proxyUrl: second.body?.proxyUrl,
      connectTimeoutSecs: second.body?.connectTimeoutSecs,
      streamIdleTimeoutSecs: second.body?.streamIdleTimeoutSecs,
      clientRootUrl: second.body?.clientRootUrl,
      gatewayPort: second.body?.gatewayPort,
      sendsAutoStart: "autoStart" in (second.body ?? {}),
      showDockIcon: second.body?.showDockIcon,
      puts: settingsSince(mark, "PUT").length,
      gets: settingsSince(mark, "GET").length,
    }, {
      expectedRevision: 9,
      processGeneration: 100,
      proxyUrl: CANONICAL_PROXY,
      connectTimeoutSecs: 21,
      streamIdleTimeoutSecs: 120,
      clientRootUrl: NORMALIZED_ROOT,
      gatewayPort: 9042,
      sendsAutoStart: false,
      showDockIcon: true,
      puts: 2,
      gets: 3,
    });
    second.resolve(200, { revision: 10, processGeneration: 100 });
    const trailing = await expectSettings("GET");
    assert.equal(settingsSince(mark, "PUT").length, 2);
    assert.equal(settingsSince(mark, "GET").length, 4);
    trailing.resolve(200, canonicalWire({ revision: 10 }));
    await flushUi();
    assert.equal(readControl(timeoutInput(mounted.root, 0)), 21);
    assert.equal(messageCount("error"), 0);
  });
});
