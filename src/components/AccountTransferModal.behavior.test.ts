import assert from "node:assert/strict";
import { before, test } from "node:test";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createPinia, setActivePinia } from "pinia";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { defineComponent, h, ref, ssrContextKey, type App } from "vue";
import {
  createVueHostRenderer,
  deferred,
  installTestWindow,
  settle,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

const renderer = createVueHostRenderer();
type AsyncFn = (...args: unknown[]) => unknown;
type CloseMode = "hide" | "unmount" | "session";
type TextInputHandler = (value: string) => void;
type BundleFileEvent = {
  target: {
    files: Array<{ name: string; size: number; text: () => Promise<string> }>;
    value: string;
  };
};
type BundleFileHandler = (event: BundleFileEvent) => void;

interface OcgUiRegistry {
  component(name: string): unknown;
  namespace(): unknown;
}

function assertTextInput(value: unknown, detail: string): asserts value is TextInputHandler {
  if (typeof value !== "function") throw new Error(detail);
}

function assertBundleFileChange(value: unknown, detail: string): asserts value is BundleFileHandler {
  if (typeof value !== "function") throw new Error(detail);
}

let Transfer: new () => unknown;
let useSessionStore: () => { dropSession: () => void };
const handlers: Record<string, AsyncFn> = {};
const downloads: Array<{ href: string; download: string }> = [];
const renderErrors: string[] = [];

function installDocument(): void {
  const document = {
    visibilityState: "visible",
    hidden: false,
    documentElement: { lang: "", style: {} },
    body: { style: {}, appendChild() {} },
    head: { appendChild() {}, removeChild() {} },
    addEventListener() {},
    removeEventListener() {},
    getElementById() { return null; },
    querySelector() { return null; },
    querySelectorAll() { return []; },
    createElement(tag: string) {
      const node = {
        tag,
        href: "",
        download: "",
        style: {},
        setAttribute(name: string, value: string) { (node as Record<string, unknown>)[name] = value; },
        click() { downloads.push({ href: node.href, download: node.download }); },
      };
      return node;
    },
  };
  Object.defineProperty(globalThis, "document", { configurable: true, value: document });
}

function installLocalStorage(): void {
  const backing = new Map<string, string>();
  const storage: Storage = {
    get length() { return backing.size; },
    clear: () => backing.clear(),
    getItem: (key) => backing.get(key) ?? null,
    key: (index) => [...backing.keys()][index] ?? null,
    removeItem: (key) => { backing.delete(key); },
    setItem: (key, value) => { backing.set(key, String(value)); },
  };
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storage });
}

function installUiKit(): void {
  const pass = defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("div", { ...attrs, "data-ui": "pass" }, ["default", "trigger", "header", "footer", "icon"].flatMap((slot) => slots[slot]?.() ?? []));
    },
  });
  const known = new Map<string, unknown>();
  const generic = (name: string) => defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("div", { ...attrs, "data-ui": name }, ["default", "trigger", "header", "footer", "icon", "extra", "action"].flatMap((slot) => slots[slot]?.() ?? []));
    },
  });
  const NButton = defineComponent({
    inheritAttrs: false,
    props: ["disabled", "loading", "type"],
    setup(props, { attrs, slots }) {
      return () => h("button", {
        ...attrs,
        type: "button",
        class: attrs.class,
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
  const NInput = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled"],
    emits: ["update:value"],
    setup(props, { attrs, emit }) {
      return () => h("input", {
        ...attrs,
        class: attrs.class,
        value: props.value ?? "",
        disabled: props.disabled ? true : undefined,
        onInput: (payload: unknown) => {
          const value = typeof payload === "string"
            ? payload
            : (payload as { target?: { value?: string } } | null)?.target?.value ?? "";
          emit("update:value", value);
        },
      });
    },
  });
  const NModal = defineComponent({
    inheritAttrs: false,
    props: ["show"],
    setup(props, { attrs, slots }) {
      return () => props.show === false ? null : h("div", { role: "dialog", class: attrs.class }, [
        slots.header?.(), slots.default?.(), slots.footer?.(),
      ]);
    },
  });
  const NCheckbox = defineComponent({
    inheritAttrs: false,
    props: ["checked", "disabled"],
    emits: ["update:checked"],
    setup(props, { attrs, emit, slots }) {
      return () => h("button", {
        ...attrs,
        type: "button",
        role: "checkbox",
        class: attrs.class,
        "aria-checked": props.checked ? "true" : "false",
        disabled: props.disabled ? true : undefined,
        onClick: () => { if (!props.disabled) emit("update:checked", !props.checked); },
      }, slots.default?.());
    },
  });
  const NForm = defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("form", { ...attrs, class: attrs.class }, slots.default?.());
    },
  });
  const specials: Record<string, unknown> = { NButton, NInput, NModal, NCheckbox, NForm, NTooltip: pass, NPopover: pass };
  const registry: OcgUiRegistry = {
    component(name: string) {
      if (known.has(name)) return known.get(name);
      const value = specials[name] ?? generic(name);
      known.set(name, value);
      return value;
    },
    namespace() {
      return new Proxy({}, { get: (_target, key) => registry.component(String(key)) });
    },
  };
  Object.assign(globalThis, { __ocgUi: registry });
}

function uiBindings(clause: string): string | null {
  const trimmed = clause.trim();
  if (!trimmed) return "";
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
    name: "ocg-transfer-ui-stub",
    enforce: "pre" as const,
    transform(code: string, id: string) {
      const normalized = id.replaceAll("\\", "/");
      if (!normalized.includes("/src/") || normalized.includes("/node_modules/")) return null;
      let changed = false;
      const next = code.replace(pattern, (full, typeOnly: string | undefined, clause: string, source: string) => {
        const ui = source === "naive-ui" || (source.startsWith("naive-ui/") && !source.includes("/locales/"))
          || source === "@vicons/antd" || source === "motion-v";
        if (!ui) return full;
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

function byClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter((node) => classTokens(node.props.class).includes(className));
}

function fire(node: HostNode): void {
  const click = node.props.onClick;
  if (typeof click !== "function") throw new Error("fixture: control has no click handler");
  const result = click();
  if (result && typeof (result as Promise<unknown>).then === "function") {
    void (result as Promise<unknown>).catch((error: unknown) => {
      renderErrors.push(error instanceof Error ? error.message : String(error));
    });
  }
}

async function waitFor(label: string, predicate: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 30; attempt += 1) {
    if (predicate()) return;
    await settle(4);
  }
  throw new Error(`fixture: timed out waiting for ${label}${renderErrors.length ? `\n${renderErrors.join("\n")}` : ""}`);
}

function bind(api: Record<string, unknown>, key: string): void {
  api[key] = (...args: unknown[]) => {
    const handler = handlers[key];
    if (!handler) throw new Error(`fixture: unmocked ${key}`);
    return handler(...args);
  };
}

async function mount(mode: "export" | "import"): Promise<{ app: App; root: HostNode; show: { value: boolean }; imported: number[] }> {
  downloads.splice(0, downloads.length);
  renderErrors.splice(0, renderErrors.length);
  setActivePinia(createPinia());
  const show = ref(true);
  const imported: number[] = [];
  const root: HostNode = { children: [], props: {}, type: "root" };
  const pinia = createPinia();
  setActivePinia(pinia);
  const app = renderer.createApp(defineComponent({
    setup: () => () => h(Transfer as never, {
      show: show.value,
      mode,
      "onUpdate:show": (value: boolean) => { show.value = value; },
      onImported: (count: number) => { imported.push(count); },
    }),
  }));
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.config.errorHandler = (error) => {
    renderErrors.push(error instanceof Error ? error.message : String(error));
  };
  app.mount(root);
  await waitFor("transfer actions", () => byClass(root, "transfer-actions").length > 0);
  if (renderErrors.length > 0) throw new Error(`fixture: transfer render failed\n${renderErrors.join("\n")}`);
  return { app, root, show, imported };
}

function fields(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.type === "input" && node.props.type !== "file");
}

function actionButtons(root: HostNode): HostNode[] {
  const actions = byClass(root, "transfer-actions")[0];
  if (!actions) throw new Error("fixture: transfer actions should render");
  return walkHostNodes(actions).filter((node) => node.type === "button");
}

async function closeOf(mounted: Awaited<ReturnType<typeof mount>>, mode: CloseMode): Promise<void> {
  if (mode === "hide") mounted.show.value = false;
  else if (mode === "unmount") mounted.app.unmount();
  else useSessionStore().dropSession();
  await settle(4);
}

before(async () => {
  installLocalStorage();
  installDocument();
  const testWindow = installTestWindow();
  Object.assign(testWindow, { localStorage: globalThis.localStorage, document: globalThis.document });
  globalThis.localStorage.setItem("ocg-manager.locale", "zh-CN");
  Object.assign(URL, { createObjectURL: () => "blob:account-transfer", revokeObjectURL: () => undefined });
  globalThis.fetch = async (input: unknown) => {
    throw new Error(`account transfer test blocked a network fetch ${String(input)}`);
  };
  installUiKit();
  const outDir = path.resolve(".artifacts/frontend-logic-repair/account-tests/transfer-client");
  await build({
    configFile: false,
    root: process.cwd(),
    logLevel: "error",
    cacheDir: path.resolve(".artifacts/frontend-logic-repair/account-tests/transfer-vite-cache"),
    plugins: [uiStubPlugin(), vue()],
    resolve: { alias: { "@": path.resolve("src") } },
    build: {
      emptyOutDir: true,
      lib: {
        entry: path.resolve(".artifacts/frontend-logic-repair/account-tests/transfer-entry.ts"),
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
  Transfer = client.Transfer;
  useSessionStore = client.useSessionStore;
  bind(client.dashboardApi, "exportAccountTransfer");
  bind(client.dashboardApi, "previewAccountTransfer");
  bind(client.dashboardApi, "importAccountTransfer");
}, { timeout: 180000 });

async function runExport(mode: CloseMode): Promise<void> {
  const gate = deferred<{ bundle: string; filename: string; exportedAccounts: number; skippedAccounts: number }>();
  let calls = 0;
  handlers.exportAccountTransfer = () => {
    calls += 1;
    return gate.promise;
  };
  const mounted = await mount("export");
  try {
    const inputs = fields(mounted.root);
    if (inputs.length < 2) throw new Error("fixture: export password fields should render");
    const firstInput = inputs[0]!.props.onInput;
    const secondInput = inputs[1]!.props.onInput;
    assertTextInput(firstInput, "fixture: export password field should accept input");
    assertTextInput(secondInput, "fixture: export confirm field should accept input");
    firstInput("abcdefghijkl");
    secondInput("abcdefghijkl");
    await settle(4);
    const primary = actionButtons(mounted.root).find((button) => button.props["data-variant"] === "primary");
    if (!primary) throw new Error("fixture: export control should render");
    fire(primary);
    await waitFor("export request", () => calls === 1);
    await closeOf(mounted, mode);
    gate.resolve({ bundle: "synthetic-bytes", filename: "bundle.ocgbackup", exportedAccounts: 1, skippedAccounts: 0 });
    await settle(8);
    assert.deepEqual({ calls, downloads: downloads.length }, { calls: 1, downloads: 0 });
  } finally {
    gate.resolve({ bundle: "", filename: "", exportedAccounts: 0, skippedAccounts: 0 });
    mounted.app.unmount();
  }
}

async function runImport(mode: CloseMode): Promise<void> {
  const gate = deferred<{ importedAccounts: number }>();
  let calls = 0;
  handlers.previewAccountTransfer = async () => ({
    importableAccounts: 1,
    duplicateAccounts: 0,
    items: [{ index: 0, disposition: "import", name: "synth", providerId: "opencode", reason: null }],
  });
  handlers.importAccountTransfer = () => {
    calls += 1;
    return gate.promise;
  };
  const mounted = await mount("import");
  try {
    const file = walkHostNodes(mounted.root).find((node) => node.props.type === "file");
    const onFile = file?.props.onChange;
    assertBundleFileChange(onFile, "fixture: bundle file control should render");
    onFile({
      target: {
        files: [{ name: "bundle.ocgbackup", size: 32, text: async () => "synthetic-bundle" }],
        value: "bundle.ocgbackup",
      },
    });
    await settle(6);
    const password = fields(mounted.root)[0];
    const onPassword = password?.props.onInput;
    assertTextInput(onPassword, "fixture: import password field should render");
    onPassword("abcdefghijkl");
    await settle(4);
    const preview = actionButtons(mounted.root)[1];
    if (!preview) throw new Error("fixture: preview control should render");
    fire(preview);
    await waitFor("import preview", () => byClass(mounted.root, "transfer-confirmation").length > 0 || walkHostNodes(mounted.root).some((node) => node.props.role === "checkbox"));
    const confirm = walkHostNodes(mounted.root).find((node) => node.props.role === "checkbox");
    if (!confirm) throw new Error("fixture: import confirmation should render");
    fire(confirm);
    await settle(4);
    const primary = actionButtons(mounted.root).find((button) => button.props["data-variant"] === "primary");
    if (!primary) throw new Error("fixture: import control should render");
    fire(primary);
    await waitFor("import request", () => calls === 1);
    await closeOf(mounted, mode);
    gate.resolve({ importedAccounts: 1 });
    await settle(8);
    assert.deepEqual({ calls, imported: mounted.imported.length, downloads: downloads.length }, { calls: 1, imported: 0, downloads: 0 });
  } finally {
    gate.resolve({ importedAccounts: 0 });
    mounted.app.unmount();
  }
}

test("closed export suppresses a late download", { timeout: 20000 }, async () => { await runExport("hide"); });
test("unmounted export suppresses a late download", { timeout: 20000 }, async () => { await runExport("unmount"); });
test("session reset suppresses a late account-transfer download", { timeout: 20000 }, async () => { await runExport("session"); });
test("closed import suppresses a stale imported event", { timeout: 20000 }, async () => { await runImport("hide"); });
test("unmounted import suppresses a stale imported event", { timeout: 20000 }, async () => { await runImport("unmount"); });
test("session reset suppresses a stale imported event", { timeout: 20000 }, async () => { await runImport("session"); });
