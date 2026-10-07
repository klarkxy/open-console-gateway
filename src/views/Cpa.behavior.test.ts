import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, beforeEach, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { createPinia, getActivePinia, setActivePinia, type Pinia } from "pinia";
import { defineComponent, h, KeepAlive, ref, ssrContextKey, type App, type Component, type Ref } from "vue";
import {
  button,
  createVueHostRenderer,
  deferred,
  fireTimers,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
  type TestWindow,
} from "../test-helpers/vue-host-runtime.ts";

type CpaApi = Record<string, (...args: unknown[]) => Promise<unknown>>;

let buildDir: string;
let Cpa: Component;
let api: CpaApi;
let testPinia: Pinia | null = null;

// Node's test process has Event but no DOM MouseEvent. Vue click listeners
// receive one, and the store treats a non-numeric argument as a foreign session.
if (typeof globalThis.MouseEvent !== "function") {
  class MouseEvent extends Event {
    constructor(type: string, init?: EventInit) {
      super(type, init);
    }
  }
  Object.defineProperty(globalThis, "MouseEvent", { configurable: true, value: MouseEvent });
}

function cpaHarnessPlugin() {
  const prefix = "\0cpa-component-harness:";
  const modules: Record<string, string> = {
    naive: `
      import { defineComponent, h } from "vue";
      const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
      } });
      export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) { return () => h("button", attrs, slots.default?.()); } });
      export const NAlert = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, [attrs.title, ...Object.values(slots).flatMap((slot) => slot?.() ?? [])]);
      } });
      export const NCard = pass; export const NEmpty = pass; export const NForm = pass;
      export const NFormItem = pass; export const NInput = pass; export const NSpin = pass; export const NSpace = pass;
      export const NSwitch = pass; export const NTabPane = pass; export const NTabs = pass; export const NTag = pass;
      export const NTooltip = pass;
      export const useDialog = () => ({ warning: (options) => options.onPositiveClick?.() });
      export const useMessage = () => {
        const record = (type) => (...args) => { (globalThis.__cpaMessages ??= []).push({ type, args }); };
        return { error: record("error"), success: record("success"), warning: record("warning") };
      };
    `,
    api: `
      export const dashboardV3 = new Proxy({}, { get: (_, key) => (...args) => globalThis.__cpaComponentApi[key](...args) });
      export const dashboardV4 = new Proxy({}, { get: (_, key) => (...args) => globalThis.__cpaComponentApi[key](...args) });
    `,
    store: `
      export const useControlPlaneStore = () => ({
        hasTokens: () => true,
        refresh: async () => ({ expectedRevision: 1, processGeneration: 1 }),
        runMutation: async (run) => run({ expectedRevision: 1, processGeneration: 1 }),
      });
    `,
    session: `
      import { ref } from "vue";
      import { getActivePinia } from "pinia";
      const authenticated = ref(true);
      function dropSession() {
        authenticated.value = false;
        const store = getActivePinia()?._s.get("cpa");
        if (typeof store?.clear === "function") store.clear();
      }
      globalThis.__cpaSession = {
        setAuthenticated(value) { authenticated.value = value; },
        drop() { dropSession(); },
      };
      export const useSessionStore = () => ({
        get authenticated() { return authenticated.value; },
        dropSession,
      });
    `,
    i18n: `export const t = (key, values = {}) => key.replace(/\\{(\\w+)\\}/g, (_, name) => String(values[name] ?? ""));`,
    errors: `export const dashboardErrorDetail = (error) => error instanceof Error ? error.message : String(error);`,
    clipboard: `
      import { ref } from "vue";
      const copiedTarget = ref("");
      export const useClipboard = () => ({ copiedTarget, copy: async () => {}, cleanup: () => {} });
    `,
  };
  const sources: Record<string, string> = {
    "naive-ui": "naive",
    "../api/dashboard-v3.ts": "api",
    "../api/dashboard-v4.ts": "api",
    "../stores/controlPlane.ts": "store",
    "../stores/session.ts": "session",
    "../i18n/index.ts": "i18n",
    "../utils/errors.ts": "errors",
    "../utils/format.ts": "clipboard",
  };
  return {
    name: "cpa-component-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      const importerPath = importer?.replaceAll("\\", "/") ?? "";
      const cpaSurface = importerPath.includes("/src/views/Cpa.vue")
        || importerPath.includes("/src/components/CpaKeyRow.vue")
        || importerPath.includes("/src/stores/cpa.ts");
      if (!cpaSurface) return null;
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/views/Cpa.vue?vue&type=style") || id.includes("/src/components/CpaKeyRow.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

const renderer = createVueHostRenderer();

function integration(overrides: Record<string, unknown> = {}) {
  return {
    accountId: null, baseUrl: "http://127.0.0.1:8317", baseUrlReadOnly: false, configured: true,
    currentOperation: null, enabled: true, inferenceKeyConfigured: true, installedVersion: "1.0.0",
    latestVersion: null, managementKeyConfigured: true, modelCount: 1, modelsRefreshedAt: null,
    processGeneration: 1, revision: 1, runtimeOwned: true, runtimeRunning: false, runtimeSupported: true,
    runtimeUnavailableReason: null, updateAvailable: false, ...overrides,
  };
}

function runtime(overrides: Record<string, unknown> = {}) {
  return {
    actions: { install: false, start: true, stop: false, checkUpdate: true, update: false, rollback: false, remove: true },
    clientKeysAvailable: true,
    codexDeviceLoginAvailable: overrides.running === true,
    startupRestorePending: overrides.desiredRunning === true,
    assetSha256: null, baseUrl: "http://127.0.0.1:8317", currentOperation: null, currentVersion: "1.0.0",
    error: null, installed: true, latestVersion: null, owned: true, phase: "idle", port: 8317,
    previousVersion: null, processGeneration: 1, revision: 1, running: false, desiredRunning: false, supported: true,
    unavailableReason: null, updateAvailable: false, ...overrides,
  };
}

type RecordedMessage = { type: string; args: unknown[] };

function recordedMessages(): RecordedMessage[] {
  return (globalThis as unknown as { __cpaMessages?: RecordedMessage[] }).__cpaMessages ?? [];
}

// Structural queries below target element kinds (alerts, tags, classed regions)
// rather than rendered wording, so copy edits cannot break behavior coverage.
/** n-alert stubs render as divs carrying a string `type` and no `size` prop. */
function alerts(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter(
    (node) => node.type === "div" && typeof node.props.type === "string" && node.props.size === undefined,
  );
}

/** n-tag stubs render as divs carrying both `size="small"` and a string `type`. */
function statusTags(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter(
    (node) => node.type === "div" && node.props.size === "small" && typeof node.props.type === "string",
  );
}

function byClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter(
    (node) => typeof node.props.class === "string" && node.props.class.split(" ").includes(className),
  );
}

type CpaSessionHandle = {
  drop: () => void;
  setAuthenticated: (value: boolean) => void;
};

function cpaSession(): CpaSessionHandle {
  const handle = (globalThis as { __cpaSession?: CpaSessionHandle }).__cpaSession;
  if (!handle) throw new Error("CPA session harness should be installed");
  return handle;
}

type CatalogModel = { id: string; enabled: boolean };

function catalogBody(models: CatalogModel[]) {
  return {
    models: models.map((model) => ({ id: model.id, ownedBy: "synthetic", enabled: model.enabled })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  };
}

function runtimeKey(hint: string, fingerprint = `fp-${hint}`) {
  return { fingerprint, hint, protected: false as const };
}

function accountRow(name: string, overrides: Record<string, unknown> = {}) {
  return {
    authIndex: "auth-synth",
    disabled: false,
    email: null,
    label: null,
    mutable: true,
    name,
    provider: "synthetic",
    quota: null,
    runtimeOnly: false,
    status: null,
    statusMessage: null,
    unavailable: false,
    ...overrides,
  };
}

type ApiCall = { method: string; args: unknown[] };

function tracked<T>(calls: ApiCall[], method: string, run: (...args: unknown[]) => Promise<T>) {
  return async (...args: unknown[]) => {
    calls.push({ method, args });
    return run(...args);
  };
}

function uiClick(): MouseEvent {
  return new MouseEvent("click", { bubbles: true });
}

function press(root: HostNode, label: string): Promise<void> {
  const target = button(root, label);
  const click = target.props.onClick as ((event: MouseEvent) => Promise<void> | void) | undefined;
  return Promise.resolve(click?.(uiClick()));
}

/** The busy-phase poll is armed only after the initial load finishes its child reads. */
async function finishInitialLoad(): Promise<void> {
  await settle(40);
}

function buttonsByLabel(root: HostNode, label: string): HostNode[] {
  return walkHostNodes(root).filter((node) => node.type === "button" && text(node).trim() === label);
}

type CpaStoreView = {
  integration: unknown;
  cpaAccounts: unknown[];
  runtimeKeys: Array<{ hint?: string; fingerprint?: string }>;
  catalogModels: unknown[];
  loading: boolean;
  keysLoading: boolean;
  accountsLoading: boolean;
  catalogLoading: boolean;
  loaded: boolean;
};

function activeCpa(): CpaStoreView {
  const store = getActivePinia()?._s.get("cpa") as CpaStoreView | undefined;
  if (!store) throw new Error("CPA store should be installed on the active Pinia");
  return store;
}

function errorMessages(): RecordedMessage[] {
  return recordedMessages().filter((message) => message.type === "error");
}

function messagesOf(type: string): RecordedMessage[] {
  return recordedMessages().filter((message) => message.type === type);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function passwordField(root: HostNode, label: string): HostNode {
  const field = walkHostNodes(root).find((node) => {
    const raw = node.props.inputProps ?? node.props["input-props"];
    return isRecord(raw) && raw["aria-label"] === label;
  });
  assert.ok(field, label);
  return field;
}

function setPasswordField(root: HostNode, label: string, value: string): void {
  const field = passwordField(root, label);
  const update = field.props["onUpdate:value"] ?? field.props.onUpdateValue;
  assert.equal(typeof update, "function", label);
  if (typeof update === "function") update(value);
}

function passwordValue(root: HostNode, label: string): unknown {
  return passwordField(root, label).props.value;
}

// A supported host with no configured connection shows the managed install.
// The cleared external draft stays mounted only after that mode is chosen again.
async function revealClearedExternalDraft(root: HostNode): Promise<void> {
  await press(root, "外部连接");
}

function disconnectBusy(root: HostNode): boolean {
  return buttonsByLabel(root, "断开并清除").some((node) => node.props.loading === true);
}

function integrationRecord(): Record<string, unknown> | null {
  const value = activeCpa().integration;
  if (!isRecord(value)) return null;
  const record: Record<string, unknown> = {};
  for (const key of Object.keys(value)) record[key] = Reflect.get(value, key);
  return record;
}

function accountNames(): string[] {
  return activeCpa().cpaAccounts.flatMap((row) => {
    if (!isRecord(row) || typeof row.name !== "string") return [];
    return [row.name];
  });
}

function externalIntegration(overrides: Record<string, unknown> = {}) {
  return integration({
    runtimeOwned: false,
    runtimeRunning: false,
    runtimeSupported: true,
    ...overrides,
  });
}

function externalRuntime(overrides: Record<string, unknown> = {}) {
  return runtime({ owned: false, running: false, supported: true, clientKeysAvailable: false, ...overrides });
}

function clearedIntegration(overrides: Record<string, unknown> = {}) {
  return externalIntegration({
    accountId: null,
    baseUrl: "http://127.0.0.1:8317",
    baseUrlReadOnly: true,
    configured: false,
    currentOperation: null,
    enabled: false,
    inferenceKeyConfigured: false,
    installedVersion: null,
    latestVersion: null,
    managementKeyConfigured: false,
    modelCount: 0,
    modelsRefreshedAt: null,
    processGeneration: 2,
    revision: 6,
    runtimeOwned: true,
    runtimeRunning: false,
    runtimeSupported: true,
    runtimeUnavailableReason: null,
    updateAvailable: false,
    ...overrides,
  });
}

function installComponentApi(componentApi: CpaApi): void {
  (globalThis as unknown as { __cpaMessages?: RecordedMessage[] }).__cpaMessages = [];
  cpaSession().setAuthenticated(true);
  api = {
    getCpaCatalog: async () => ({ models: [], sourceUrl: null, refreshedAt: null, revision: { revision: 1, processGeneration: 1, pricingRevision: "p" } }),
    // Default to a valid empty discovery so the CLI import section stays quiet;
    // tests that care about discovery override this explicitly.
    getCpaCliImports: async () => ({ sources: [] }),
    ...componentApi,
  };
  (globalThis as { __cpaComponentApi?: CpaApi }).__cpaComponentApi = api;
}

function installTestPinia(app: App): void {
  const pinia = testPinia ?? createPinia();
  testPinia = pinia;
  setActivePinia(pinia);
  app.use(pinia);
}

async function mount(componentApi: CpaApi): Promise<{ app: App; root: HostNode; window: TestWindow }> {
  const testWindow = installTestWindow();
  installComponentApi(componentApi);
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Cpa);
  installTestPinia(app);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  return { app, root, window: testWindow };
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "cpa-component-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [cpaHarnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      lib: {
        entry: path.resolve("src/views/Cpa.vue"),
        fileName: () => "cpa.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue", "pinia"] },
    },
  });
  Cpa = (await import(pathToFileURL(path.join(buildDir, "cpa.mjs")).href)).default;
});

beforeEach(() => {
  testPinia = createPinia();
  setActivePinia(testPinia);
});

after(async () => { await rm(buildDir, { force: true, recursive: true }); });

test("a synchronous lifecycle success immediately refreshes integration, accounts, and keys", async () => {
  let running = false;
  const calls = { accounts: 0, integration: 0, keys: 0 };
  const mounted = await mount({
    getCpaIntegration: async () => { calls.integration += 1; return integration({ runtimeRunning: running }); },
    getCpaRuntime: async () => runtime({ running }),
    getCpaAccounts: async () => { calls.accounts += 1; return { accounts: [] }; },
    getCpaRuntimeKeys: async () => { calls.keys += 1; return { keys: [], processGeneration: 1, revision: 1 }; },
    startCpaRuntime: async () => { running = true; return runtime({ running: true }); },
  });
  calls.accounts = calls.integration = calls.keys = 0;
  const typesBeforeStart = statusTags(mounted.root).map((tag) => tag.props.type);
  const start = button(mounted.root, "启动");
  await (start.props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(calls.integration, 1);
  assert.equal(calls.accounts, 1);
  assert.equal(calls.keys, 1);
  const typesAfterStart = statusTags(mounted.root).map((tag) => tag.props.type);
  assert.ok(
    !typesBeforeStart.includes("success") && typesAfterStart.includes("success"),
    "starting the runtime flips the status tag to the running state",
  );
  mounted.app.unmount();
});

test("runtime actions consume server eligibility and block denied dispatch", async () => {
  let starts = 0;
  let keys = 0;
  const denied = { install: false, start: false, stop: false, checkUpdate: false, update: false, rollback: false, remove: false };
  const mounted = await mount({
    getCpaIntegration: async () => integration(),
    getCpaRuntime: async () => runtime({ actions: denied, clientKeysAvailable: false }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => { keys += 1; return { keys: [] }; },
    startCpaRuntime: async () => { starts += 1; return runtime(); },
  });
  const controls = byClass(mounted.root, "cpa-runtime-actions")[0];
  assert.ok(controls);
  const buttons = walkHostNodes(controls).filter((node) => node.type === "button");
  assert.equal(buttons.length, 7);
  assert.ok(buttons.every((node) => node.props.disabled === true));
  await (buttons[1]!.props.onClick as () => Promise<void>)();
  assert.equal(starts, 0);
  assert.equal(keys, 0, "server capability hides the client-key read despite installed/owned facts");
  mounted.app.unmount();
});

test("check-update commits its runtime receipt and update uses the current server version", async () => {
  let reads = 0;
  let expectedVersion: unknown;
  const actions = { install: false, start: true, stop: false, checkUpdate: true, update: true, rollback: false, remove: true };
  const receipt = runtime({ actions, latestVersion: "1.2.0", updateAvailable: true });
  const mounted = await mount({
    getCpaIntegration: async () => integration(),
    getCpaRuntime: async () => { reads += 1; return runtime(); },
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    checkCpaRuntimeUpdate: async () => ({
      currentVersion: "1.0.0", latestVersion: "stale-check-version", updateAvailable: true,
      releaseUrl: "https://example.com/release", revision: 1, processGeneration: 1, runtime: receipt,
    }),
    updateCpaRuntime: async (input) => { expectedVersion = (input as { expectedVersion: unknown }).expectedVersion; return runtime(); },
  });
  const controls = () => walkHostNodes(byClass(mounted.root, "cpa-runtime-actions")[0]!).filter((node) => node.type === "button");
  await (controls()[3]!.props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(reads, 1, "check receipt supplies current runtime without a follow-up GET");
  assert.equal(controls()[4]!.props.disabled, false);
  await (controls()[4]!.props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(expectedVersion, "1.2.0");
  mounted.app.unmount();
});

test("a successful client Key creation keeps its one-time secret visible when list refresh fails", async () => {
  let keyReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads > 1) throw new Error("list refresh failed");
      return { keys: [], processGeneration: 1, revision: 1 };
    },
    createCpaRuntimeKey: async () => ({ fingerprint: "fp-new", hint: "sk-…new", processGeneration: 1, revision: 2, secret: "sk-one-time-secret" }),
  });
  await (button(mounted.root, "添加客户端 Key").props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(keyReads, 2);
  assert.match(text(mounted.root), /sk-one-time-secret/);
  mounted.app.unmount();
});

test("a runtime fetch failure is a visible recoverable error and not confirmed managed support", async () => {
  const mounted = await mount({
    getCpaIntegration: async () => integration({
      configured: false,
      runtimeOwned: false,
      runtimeSupported: true,
      installedVersion: null,
    }),
    getCpaRuntime: async () => {
      throw new Error("runtime down");
    },
  });
  assert.ok(
    alerts(mounted.root).some((node) => node.props.type === "error" && /runtime down/.test(String(node.props.title))),
    "the runtime fetch failure surfaces an error alert carrying the backend message",
  );
  assert.equal(button(mounted.root, "托管安装").props.disabled, true);
  assert.ok(
    !alerts(mounted.root).some((node) => node.props.type === "warning"),
    "managed support stays unconfirmed: no unsupported-environment notice renders",
  );
  mounted.app.unmount();
});

test("runtime polling stays serial while a request is in flight", async () => {
  let runtimeReads = 0;
  const pending = deferred<ReturnType<typeof runtime>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ currentOperation: "install" }),
    getCpaRuntime: async () => {
      runtimeReads += 1;
      if (runtimeReads === 1) return runtime({ phase: "downloading", currentOperation: "install" });
      return pending.promise;
    },
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
  });
  assert.equal(runtimeReads, 1);
  await finishInitialLoad();
  await fireTimers(mounted.window);
  assert.equal(runtimeReads, 2);
  await fireTimers(mounted.window);
  assert.equal(runtimeReads, 2);
  pending.resolve(runtime({ phase: "downloading", currentOperation: "install" }));
  await settle();
  mounted.app.unmount();
});

test("runtime polling is gated on page visibility", async () => {
  const visibility = { state: "hidden" as "hidden" | "visible" };
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    writable: true,
    value: { get visibilityState() { return visibility.state; } },
  });
  try {
    let runtimeReads = 0;
    const mounted = await mount({
      getCpaIntegration: async () => integration({ currentOperation: "install" }),
      getCpaRuntime: async () => {
        runtimeReads += 1;
        return runtime({ phase: "downloading", currentOperation: "install" });
      },
      getCpaAccounts: async () => ({ accounts: [] }),
      getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
    });
    assert.equal(runtimeReads, 1, "the initial load still reads the runtime");
    await finishInitialLoad();
    await fireTimers(mounted.window);
    await fireTimers(mounted.window);
    assert.equal(runtimeReads, 1, "hidden polls skip the network read but keep re-arming");
    visibility.state = "visible";
    await fireTimers(mounted.window);
    assert.equal(runtimeReads, 2, "a visible poll reads the runtime again");
    mounted.app.unmount();
  } finally {
    delete (globalThis as { document?: unknown }).document;
  }
});

test("stale runtime polls are ignored after a refresh", async () => {
  let runtimeReads = 0;
  const stale = deferred<ReturnType<typeof runtime>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration(),
    getCpaRuntime: async () => {
      runtimeReads += 1;
      if (runtimeReads === 1) return runtime({ phase: "downloading", latestVersion: "1.0.0" });
      if (runtimeReads === 2) return stale.promise;
      return runtime({ phase: "downloading", latestVersion: "fresh-keep" });
    },
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
  });
  await finishInitialLoad();
  await fireTimers(mounted.window);
  assert.equal(runtimeReads, 2);
  await press(mounted.root, "刷新");
  await settle();
  stale.resolve(runtime({ phase: "idle", latestVersion: "stale-idle" }));
  await settle();
  assert.doesNotMatch(text(mounted.root), /stale-idle/);
  assert.match(text(mounted.root), /fresh-keep/);
  mounted.app.unmount();
});

test("a runtime poll failure stays visible with a local retry", async () => {
  let runtimeReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration(),
    getCpaRuntime: async () => {
      runtimeReads += 1;
      if (runtimeReads === 1) return runtime({ phase: "downloading" });
      if (runtimeReads === 2) throw new Error("poll failed");
      return runtime({ phase: "idle" });
    },
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
  });
  await finishInitialLoad();
  await fireTimers(mounted.window);
  await settle();
  assert.ok(
    alerts(mounted.root).some((node) => node.props.type === "error" && /poll failed/.test(String(node.props.title))),
    "the poll failure surfaces an error alert carrying the backend message",
  );
  assert.ok(byClass(mounted.root, "cpa-phase").length >= 1, "the last known phase indicator stays visible during the failure");
  const retries = buttonsByLabel(mounted.root, "重试");
  assert.ok(retries.length >= 1);
  await (retries[retries.length - 1].props.onClick as (event: MouseEvent) => Promise<void>)(uiClick());
  await settle();
  assert.ok(
    !alerts(mounted.root).some((node) => node.props.type === "error" && /poll failed/.test(String(node.props.title))),
    "a successful retry clears the poll failure alert",
  );
  assert.equal(byClass(mounted.root, "cpa-phase").length, 0, "the stale phase indicator clears once the runtime is idle again");
  mounted.app.unmount();
});

test("the persisted model catalog lists ids grouped by source", async () => {
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2, modelsRefreshedAt: "2026-09-07T01:51:55.000Z" }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
    getCpaCatalog: async () => ({
      models: [
        { id: "gpt-5", ownedBy: "openai", enabled: true },
        { id: "claude-sonnet", ownedBy: "anthropic", enabled: false },
      ],
      sourceUrl: "http://127.0.0.1:8317",
      refreshedAt: "2026-09-07T01:51:55.000Z",
      revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
    }),
  });
  assert.match(text(mounted.root), /gpt-5/);
  assert.match(text(mounted.root), /claude-sonnet/);
  assert.match(text(mounted.root), /openai/);
  assert.match(text(mounted.root), /anthropic/);
  // The catalog meta line renders exactly once and carries the data-derived selected/total counts.
  const catalogMeta = byClass(mounted.root, "cpa-catalog-meta");
  assert.equal(catalogMeta.length, 1);
  assert.match(text(catalogMeta[0]), /1\s*\/\s*2/);
  assert.match(text(mounted.root), /http:\/\/127\.0\.0\.1:8317/);
  assert.equal(button(mounted.root, "gpt-5").props["aria-pressed"], true);
  assert.equal(button(mounted.root, "claude-sonnet").props["aria-pressed"], false);
  mounted.app.unmount();
});

test("refreshing the model catalog renders returned ids and sources", async () => {
  let catalog = {
    models: [] as Array<{ id: string; ownedBy: string; enabled: boolean }>,
    sourceUrl: null as string | null,
    refreshedAt: null as string | null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  };
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 0 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
    getCpaCatalog: async () => catalog,
    refreshCpaModels: async () => {
      catalog = {
        models: [{ id: "grok-4", ownedBy: "xai", enabled: false }],
        sourceUrl: "http://127.0.0.1:8317",
        refreshedAt: "2026-09-07T02:00:00.000Z",
        revision: { revision: 2, processGeneration: 1, pricingRevision: "p" },
      };
      return {
        models: [{ id: "grok-4", ownedBy: "xai" }],
        sourceUrl: catalog.sourceUrl,
        refreshedAt: catalog.refreshedAt,
        processGeneration: 1,
        revision: 2,
      };
    },
  });
  await (button(mounted.root, "刷新模型目录").props.onClick as () => Promise<void>)();
  await settle();
  assert.match(text(mounted.root), /grok-4/);
  assert.match(text(mounted.root), /xai/);
  assert.equal(button(mounted.root, "grok-4").props["aria-pressed"], false);
  mounted.app.unmount();
});

test("selecting a catalog card saves the routed subset", async () => {
  const puts: string[][] = [];
  let models = [
    { id: "gpt-5", ownedBy: "openai", enabled: false },
    { id: "claude-sonnet", ownedBy: "anthropic", enabled: true },
  ];
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
    getCpaCatalog: async () => ({
      models,
      sourceUrl: "http://127.0.0.1:8317",
      refreshedAt: "2026-09-07T01:51:55.000Z",
      revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
    }),
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      puts.push(input.enabledIds);
      models = models.map((model) => ({ ...model, enabled: input.enabledIds.includes(model.id) }));
      return {
        models,
        sourceUrl: "http://127.0.0.1:8317",
        refreshedAt: "2026-09-07T01:51:55.000Z",
        revision: { revision: 2, processGeneration: 1, pricingRevision: "p" },
      };
    },
  });
  await (button(mounted.root, "gpt-5").props.onClick as () => Promise<void>)();
  await settle();
  assert.deepEqual(puts, [["gpt-5", "claude-sonnet"]]);
  assert.equal(button(mounted.root, "gpt-5").props["aria-pressed"], true);
  await (button(mounted.root, "全部关闭").props.onClick as () => Promise<void>)();
  await settle();
  assert.deepEqual(puts[1], []);
  assert.equal(button(mounted.root, "gpt-5").props["aria-pressed"], false);
  assert.equal(button(mounted.root, "claude-sonnet").props["aria-pressed"], false);
  mounted.app.unmount();
});

test("rapid CPA card clicks keep the latest selection instead of the stale response", async () => {
  const writes: string[][] = [];
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const first = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => snapshot([]),
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return writes.length === 1 ? first.promise : snapshot(input.enabledIds);
    },
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  (button(mounted.root, "model-b").props.onClick as () => void)();
  await settle();
  first.resolve(snapshot(["model-a"]));
  await settle();
  assert.deepEqual(writes.at(-1), ["model-a", "model-b"]);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], true);
  assert.equal(button(mounted.root, "model-b").props["aria-pressed"], true);
  mounted.app.unmount();
});

test("clicks queued before the first save starts coalesce into the latest selection", async () => {
  const writes: string[][] = [];
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => snapshot([]),
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return snapshot(input.enabledIds);
    },
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  (button(mounted.root, "model-b").props.onClick as () => void)();
  await settle();
  assert.deepEqual(writes, [["model-a", "model-b"]]);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], true);
  assert.equal(button(mounted.root, "model-b").props["aria-pressed"], true);
  mounted.app.unmount();
});

test("toggling a model back off while its save is pending keeps the off selection", async () => {
  const writes: string[][] = [];
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const first = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => snapshot([]),
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return writes.length === 1 ? first.promise : snapshot(input.enabledIds);
    },
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  first.resolve(snapshot(["model-a"]));
  await settle();
  assert.deepEqual(writes.at(-1), []);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], false);
  assert.equal(button(mounted.root, "model-b").props["aria-pressed"], false);
  mounted.app.unmount();
});

test("turning everything off while a save is pending wins over the in-flight selection", async () => {
  const writes: string[][] = [];
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const first = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => snapshot(["model-a", "model-b"]),
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return writes.length === 1 ? first.promise : snapshot(input.enabledIds);
    },
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  (button(mounted.root, "全部关闭").props.onClick as () => void)();
  await settle();
  first.resolve(snapshot(["model-b"]));
  await settle();
  assert.deepEqual(writes.at(-1), []);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], false);
  assert.equal(button(mounted.root, "model-b").props["aria-pressed"], false);
  mounted.app.unmount();
});

test("a failed catalog save resyncs from the server and later selections still save", async () => {
  const writes: string[][] = [];
  let catalogReads = 0;
  let serverEnabled = ["model-a"];
  let failFirstSave = true;
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      catalogReads += 1;
      return snapshot(serverEnabled);
    },
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      if (failFirstSave) {
        failFirstSave = false;
        throw new Error("revision conflict");
      }
      serverEnabled = [...input.enabledIds];
      return snapshot(serverEnabled);
    },
  });
  const readsAfterLoad = catalogReads;
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  // The rejected save resynced from the server instead of keeping the stale toggle.
  assert.equal(catalogReads, readsAfterLoad + 1);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], true);
  (button(mounted.root, "model-b").props.onClick as () => void)();
  await settle();
  assert.deepEqual(writes.at(-1), ["model-a", "model-b"]);
  assert.equal(button(mounted.root, "model-b").props["aria-pressed"], true);
  mounted.app.unmount();
});

test("disconnecting while a catalog save is pending drops the stale write response", async () => {
  const writes: string[][] = [];
  let catalogReads = 0;
  let disconnected = false;
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const first = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => (disconnected
      ? integration({ configured: false, modelCount: 0, runtimeOwned: false, installedVersion: null })
      : integration({ modelCount: 2, runtimeOwned: false })),
    getCpaRuntime: async () => runtime({ owned: false }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      catalogReads += 1;
      return snapshot([]);
    },
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return first.promise;
    },
    deleteCpaIntegration: async () => {
      disconnected = true;
    },
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  assert.equal(writes.length, 1);
  const readsBeforeDisconnect = catalogReads;
  await (button(mounted.root, "断开并清除").props.onClick as () => Promise<void>)();
  await settle();
  first.resolve(snapshot(["model-a"]));
  await settle();
  // The stale in-flight response neither resynced nor resurrected the selection.
  assert.equal(catalogReads, readsBeforeDisconnect);
  assert.doesNotMatch(text(mounted.root), /model-a/);
  assert.ok(
    alerts(mounted.root).some((node) => node.props.type === "info" && typeof node.props.title === "string"),
    "the catalog falls back to the not-configured guidance alert after disconnect",
  );
  mounted.app.unmount();
});

test("a full refresh while a save is pending keeps the freshly loaded catalog", async () => {
  let fresh = false;
  const snapshot = (enabledIds: string[], extra: string[] = []) => ({
    models: [...enabledIds, ...extra].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const first = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => (fresh ? snapshot(["model-c"], ["model-a", "model-b"]) : snapshot([], ["model-a", "model-b"])),
    putCpaCatalog: async () => first.promise,
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  fresh = true;
  await (button(mounted.root, "刷新").props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(button(mounted.root, "model-c").props["aria-pressed"], true);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], false);
  first.resolve(snapshot(["model-a"], ["model-b"]));
  await settle();
  // The stale pre-refresh response must not overwrite the newly loaded generation.
  assert.equal(button(mounted.root, "model-c").props["aria-pressed"], true);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], false);
  mounted.app.unmount();
});

test("unmounting with a catalog save pending leaves no resync behind when the response lands", async () => {
  let catalogReads = 0;
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const first = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      catalogReads += 1;
      return snapshot([]);
    },
    putCpaCatalog: async () => first.promise,
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  const readsBeforeUnmount = catalogReads;
  mounted.app.unmount();
  first.resolve(snapshot(["model-a"]));
  await settle();
  assert.equal(catalogReads, readsBeforeUnmount);
});

test("a delayed error resync does not erase selections made while the next save is pending", async () => {
  const writes: string[][] = [];
  let reads = 0;
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b", "model-c"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const failedPut = deferred<ReturnType<typeof snapshot>>();
  const recovery = deferred<ReturnType<typeof snapshot>>();
  const secondPut = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 3 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      reads += 1;
      return reads === 1 ? snapshot([]) : recovery.promise;
    },
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return writes.length === 1 ? failedPut.promise : writes.length === 2 ? secondPut.promise : snapshot(input.enabledIds);
    },
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  failedPut.reject(new Error("conflict"));
  await settle();
  (button(mounted.root, "model-b").props.onClick as () => void)();
  await settle();
  recovery.resolve(snapshot([]));
  await settle();
  (button(mounted.root, "model-c").props.onClick as () => void)();
  await settle();
  secondPut.resolve(snapshot(["model-a", "model-b"]));
  await settle();
  assert.deepEqual(writes, [["model-a"], ["model-a", "model-b"], ["model-a", "model-b", "model-c"]]);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], true);
  assert.equal(button(mounted.root, "model-b").props["aria-pressed"], true);
  assert.equal(button(mounted.root, "model-c").props["aria-pressed"], true);
  // The failure was superseded during its own resync: no stale error toast.
  assert.deepEqual(recordedMessages().filter((message) => message.type === "error"), []);
  mounted.app.unmount();
});

test("a delayed error resync crossing disconnect applies nothing and toasts nothing", async () => {
  const writes: string[][] = [];
  let catalogReads = 0;
  let disconnected = false;
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b", "model-c"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const failedPut = deferred<ReturnType<typeof snapshot>>();
  const recovery = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => (disconnected
      ? integration({ configured: false, modelCount: 0, runtimeOwned: false, installedVersion: null })
      : integration({ modelCount: 3, runtimeOwned: false })),
    getCpaRuntime: async () => runtime({ owned: false }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      catalogReads += 1;
      return catalogReads === 1 ? snapshot([]) : recovery.promise;
    },
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return failedPut.promise;
    },
    deleteCpaIntegration: async () => {
      disconnected = true;
    },
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  failedPut.reject(new Error("conflict"));
  await settle();
  await (button(mounted.root, "断开并清除").props.onClick as () => Promise<void>)();
  await settle();
  const readsBeforeResolve = catalogReads;
  recovery.resolve(snapshot(["model-a"]));
  await settle();
  assert.equal(writes.length, 1);
  assert.equal(catalogReads, readsBeforeResolve);
  assert.doesNotMatch(text(mounted.root), /model-a/);
  assert.ok(
    alerts(mounted.root).some((node) => node.props.type === "info" && typeof node.props.title === "string"),
    "the catalog falls back to the not-configured guidance alert after disconnect",
  );
  assert.deepEqual(recordedMessages().filter((message) => message.type === "error"), []);
  mounted.app.unmount();
});

test("a delayed error resync crossing unmount applies nothing and toasts nothing", async () => {
  let catalogReads = 0;
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b", "model-c"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const failedPut = deferred<ReturnType<typeof snapshot>>();
  const recovery = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 3 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      catalogReads += 1;
      return catalogReads === 1 ? snapshot([]) : recovery.promise;
    },
    putCpaCatalog: async () => failedPut.promise,
  });
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  failedPut.reject(new Error("conflict"));
  await settle();
  mounted.app.unmount();
  const readsBeforeResolve = catalogReads;
  recovery.resolve(snapshot(["model-a"]));
  await settle();
  assert.equal(catalogReads, readsBeforeResolve);
  assert.deepEqual(recordedMessages().filter((message) => message.type === "error"), []);
});

test("a refresh snapshot does not erase a selection made while the refresh is in flight", async () => {
  const writes: string[][] = [];
  let reads = 0;
  const snapshot = (enabledIds: string[]) => ({
    models: ["model-a", "model-b", "model-c"].map((id) => ({ id, ownedBy: "openai", enabled: enabledIds.includes(id) })),
    sourceUrl: null,
    refreshedAt: null,
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
  });
  const refreshGet = deferred<ReturnType<typeof snapshot>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 3 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      reads += 1;
      return reads === 1 ? snapshot([]) : refreshGet.promise;
    },
    refreshCpaModels: async () => ({
      models: [],
      sourceUrl: null,
      refreshedAt: null,
      processGeneration: 1,
      revision: 2,
    }),
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return snapshot(input.enabledIds);
    },
  });
  const refreshing = (button(mounted.root, "刷新模型目录").props.onClick as () => Promise<void>)();
  await settle();
  (button(mounted.root, "model-a").props.onClick as () => void)();
  await settle();
  assert.deepEqual(writes, [["model-a"]]);
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], true);
  refreshGet.resolve(snapshot([]));
  await refreshing;
  await settle();
  assert.equal(button(mounted.root, "model-a").props["aria-pressed"], true);
  assert.deepEqual(writes, [["model-a"]]);
  mounted.app.unmount();
});

const Away = defineComponent({
  name: "CpaAway",
  setup() {
    return () => h("div", { class: "cpa-away" });
  },
});

async function mountKept(componentApi: CpaApi): Promise<{
  app: App;
  root: HostNode;
  showing: Ref<boolean>;
  window: TestWindow;
}> {
  const testWindow = installTestWindow();
  installComponentApi(componentApi);
  const showing = ref(true);
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(defineComponent({
    name: "CpaKeepAliveHost",
    setup() {
      return () => h(KeepAlive, null, {
        default: () => (showing.value ? h(Cpa) : h(Away)),
      });
    },
  }));
  installTestPinia(app);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle(40);
  return { app, root, showing, window: testWindow };
}

function revealedSecret(root: HostNode): string {
  return byClass(root, "cpa-secret-value").map((node) => text(node)).join("");
}

function assertSecretIsLocal(calls: ApiCall[], secret: string): void {
  assert.equal(
    JSON.stringify(calls).includes(secret),
    false,
    "one-time client key secret stays on the reveal and is never sent back",
  );
}

function trackPromise(promise: Promise<void>): { status: () => "pending" | "settled" } {
  let status: "pending" | "settled" = "pending";
  void promise.then(
    () => { status = "settled"; },
    () => { status = "settled"; },
  );
  return { status: () => status };
}

function deferredRead<T>(): ReturnType<typeof deferred<T>> {
  const gate = deferred<T>();
  void gate.promise.catch(() => undefined);
  return gate;
}

async function waitForCount(read: () => number, count: number, label: string): Promise<void> {
  for (let i = 0; i < 200; i++) {
    if (read() >= count) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail(`${label}: expected ${count} requests, saw ${read()}`);
}

function keyRows(root: HostNode): HostNode[] {
  return byClass(root, "cpa-key-row");
}

function accountRows(root: HostNode): HostNode[] {
  return byClass(root, "cpa-account-row");
}

function catalogCards(root: HostNode): HostNode[] {
  return byClass(root, "cpa-catalog-card");
}

/** Read notices are typed error/warning (or role alert/status). Tags carry `size`. */
function readNotices(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => {
    if (node.type !== "div" || node.props.size !== undefined) return false;
    const kind = node.props.type;
    const role = node.props.role;
    return kind === "error" || kind === "warning" || role === "alert" || role === "status";
  });
}

function noticesWithMarker(root: HostNode, marker: string): HostNode[] {
  return readNotices(root).filter((node) => (
    String(node.props.title ?? "").includes(marker) || text(node).includes(marker)
  ));
}

function isReadErrorNotice(node: HostNode): boolean {
  if (node.props.type === "error") return true;
  if (node.props.type === "warning") return false;
  return node.props.role === "alert";
}

function isReadWarningNotice(node: HostNode): boolean {
  if (node.props.type === "warning") return true;
  if (node.props.type === "error") return false;
  return node.props.role === "status";
}

function retryControl(notice: HostNode): HostNode {
  const control = walkHostNodes(notice).find((node) => node.type === "button");
  assert.ok(control, "the read notice must expose a retry control");
  return control!;
}

function assertFirstLoadReadError(root: HostNode, marker: string): HostNode {
  const marked = noticesWithMarker(root, marker);
  const error = marked.find(isReadErrorNotice);
  assert.ok(error, "a first-load read failure must surface an error carrying the fixture marker");
  assert.equal(
    marked.some(isReadWarningNotice),
    false,
    "a first-load read failure must not use the background-read warning surface",
  );
  retryControl(error);
  return error;
}

function assertBackgroundReadWarning(root: HostNode, marker: string): HostNode {
  const marked = noticesWithMarker(root, marker);
  const warning = marked.find(isReadWarningNotice);
  assert.ok(warning, "a failed background read must surface a warning carrying the fixture marker");
  assert.equal(
    marked.some(isReadErrorNotice),
    false,
    "a failed background read must not use the first-load error surface",
  );
  retryControl(warning);
  return warning;
}

async function assertBackgroundReadRetry(options: {
  root: HostNode;
  marker: string;
  reads: () => number;
  writes: () => number;
  others?: Array<{ read: () => number; label: string }>;
}): Promise<void> {
  const warning = assertBackgroundReadWarning(options.root, options.marker);
  const readsBefore = options.reads();
  const writesBefore = options.writes();
  const otherBefore = (options.others ?? []).map((item) => item.read());
  await Promise.resolve(
    (retryControl(warning).props.onClick as ((event: MouseEvent) => Promise<void> | void) | undefined)?.(uiClick()),
  );
  await settle();
  await waitForCount(options.reads, readsBefore + 1, `${options.marker} retry GET`);
  assert.equal(options.reads(), readsBefore + 1, "retry issues exactly one GET");
  assert.equal(options.writes(), writesBefore, "retry must not issue a second write");
  for (const [index, item] of (options.others ?? []).entries()) {
    assert.equal(item.read(), otherBefore[index], `${item.label} must not be re-fetched by the retry`);
  }
  assert.equal(
    noticesWithMarker(options.root, options.marker).length,
    0,
    "a successful retry clears the read warning",
  );
}

test("manual refresh during a pending catalog read keeps one eligible read for the new generation", async () => {
  const gates: Array<ReturnType<typeof deferred<ReturnType<typeof catalogBody>>>> = [];
  const writes: string[][] = [];
  const putGate = deferred<ReturnType<typeof catalogBody>>();
  let reads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => {
      const gate = deferred<ReturnType<typeof catalogBody>>();
      gates.push(gate);
      reads += 1;
      return gate.promise;
    },
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return putGate.promise;
    },
  });
  try {
    assert.equal(reads, 1);
    const refresh = press(mounted.root, "刷新");
    await settle();
    const supersededStillPending = reads >= 2;
    if (!supersededStillPending) {
      gates[0]?.resolve(catalogBody([{ id: "model-stale", enabled: true }]));
      await settle();
      await refresh;
      await settle();
      assert.doesNotMatch(text(mounted.root), /model-stale/);
    }
    assert.ok(
      reads >= 2,
      "manual refresh during a pending catalog GET must leave a current eligible catalog read",
    );
    const current = reads - 1;
    gates[current]?.resolve(catalogBody([
      { id: "model-fresh", enabled: true },
      { id: "model-peer", enabled: false },
    ]));
    await refresh;
    await settle();
    assert.equal(button(mounted.root, "model-fresh").props["aria-pressed"], true);
    assert.equal(button(mounted.root, "model-peer").props["aria-pressed"], false);
    press(mounted.root, "model-peer");
    await settle();
    assert.deepEqual(writes, [["model-fresh", "model-peer"]]);
    if (supersededStillPending) {
      gates[0]?.resolve(catalogBody([{ id: "model-stale", enabled: true }]));
      await settle();
    }
    for (let index = 1; index < current; index += 1) {
      gates[index]?.resolve(catalogBody([{ id: "model-stale", enabled: false }]));
    }
    await settle();
    assert.equal(writes.length, 1, "a superseded catalog read must not replay the selection write");
    assert.equal(button(mounted.root, "model-fresh").props["aria-pressed"], true);
    assert.equal(button(mounted.root, "model-peer").props["aria-pressed"], true);
    assert.doesNotMatch(text(mounted.root), /model-stale/);
    putGate.resolve(catalogBody([
      { id: "model-fresh", enabled: true },
      { id: "model-peer", enabled: true },
    ]));
    await settle();
    assert.equal(writes.length, 1);
    assert.equal(button(mounted.root, "model-fresh").props["aria-pressed"], true);
    assert.equal(button(mounted.root, "model-peer").props["aria-pressed"], true);
    assert.doesNotMatch(text(mounted.root), /model-stale/);
  } finally {
    mounted.app.unmount();
  }
});

test("a catalog write started before a refresh is not replayed over the loaded generation", async () => {
  const writes: string[][] = [];
  let fresh = false;
  const putGate = deferred<ReturnType<typeof catalogBody>>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ modelCount: 2 }),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCatalog: async () => (fresh
      ? catalogBody([{ id: "model-kept", enabled: true }, { id: "model-old", enabled: false }])
      : catalogBody([{ id: "model-old", enabled: false }, { id: "model-kept", enabled: false }])),
    putCpaCatalog: async (...args: unknown[]) => {
      const input = args[0] as { enabledIds: string[] };
      writes.push([...input.enabledIds]);
      return putGate.promise;
    },
  });
  try {
    press(mounted.root, "model-old");
    await settle();
    assert.deepEqual(writes, [["model-old"]]);
    fresh = true;
    await press(mounted.root, "刷新");
    await settle();
    assert.equal(button(mounted.root, "model-kept").props["aria-pressed"], true);
    putGate.resolve(catalogBody([{ id: "model-old", enabled: true }, { id: "model-kept", enabled: false }]));
    await settle();
    assert.deepEqual(writes, [["model-old"]]);
    assert.equal(button(mounted.root, "model-kept").props["aria-pressed"], true);
    assert.equal(button(mounted.root, "model-old").props["aria-pressed"], false);
  } finally {
    mounted.app.unmount();
  }
});

test("an older client-key list cannot erase a key created while that list was in flight", async () => {
  const calls: ApiCall[] = [];
  const initial = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let reads = 0;
  const created = runtimeKey("hint-created");
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      reads += 1;
      if (reads === 1) return initial.promise;
      return { keys: [created], processGeneration: 1, revision: 2 };
    }),
    createCpaRuntimeKey: tracked(calls, "createCpaRuntimeKey", async () => ({
      ...created,
      processGeneration: 1,
      revision: 2,
      secret: "secret-local-once",
    })),
  });
  try {
    assert.equal(reads, 1);
    await press(mounted.root, "添加客户端 Key");
    await settle();
    assert.equal(revealedSecret(mounted.root), "secret-local-once");
    assert.match(text(mounted.root), /hint-created/);
    assert.doesNotMatch(byClass(mounted.root, "cpa-key-row").map((node) => text(node)).join("\n"), /secret-local-once/);
    assertSecretIsLocal(calls, "secret-local-once");
    initial.resolve({ keys: [runtimeKey("hint-stale")] });
    await settle();
    assert.match(text(mounted.root), /hint-created/);
    assert.match(text(mounted.root), /acct-live/);
    assert.equal(revealedSecret(mounted.root), "secret-local-once");
    assert.doesNotMatch(text(mounted.root), /hint-stale/);
    assertSecretIsLocal(calls, "secret-local-once");
  } finally {
    mounted.app.unmount();
  }
});

test("consecutive client-key creates survive the original list resolving last", async () => {
  const calls: ApiCall[] = [];
  const initial = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let reads = 0;
  let creates = 0;
  const firstKey = runtimeKey("hint-first");
  const secondKey = runtimeKey("hint-second");
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      reads += 1;
      if (reads === 1) return initial.promise;
      if (reads === 2) return { keys: [firstKey], processGeneration: 1, revision: 2 };
      return { keys: [firstKey, secondKey], processGeneration: 1, revision: 3 };
    }),
    createCpaRuntimeKey: tracked(calls, "createCpaRuntimeKey", async () => {
      creates += 1;
      const key = creates === 1 ? firstKey : secondKey;
      return { ...key, processGeneration: 1, revision: creates + 1, secret: `secret-step-${creates}` };
    }),
  });
  try {
    await press(mounted.root, "添加客户端 Key");
    await settle();
    await press(mounted.root, "添加客户端 Key");
    await settle();
    assert.match(text(mounted.root), /hint-first/);
    assert.match(text(mounted.root), /hint-second/);
    assert.equal(revealedSecret(mounted.root), "secret-step-2");
    initial.resolve({ keys: [runtimeKey("hint-stale")] });
    await settle();
    assert.match(text(mounted.root), /hint-first/);
    assert.match(text(mounted.root), /hint-second/);
    assert.match(text(mounted.root), /acct-live/);
    assert.equal(revealedSecret(mounted.root), "secret-step-2");
    assert.doesNotMatch(text(mounted.root), /hint-stale/);
    assert.doesNotMatch(byClass(mounted.root, "cpa-key-row").map((node) => text(node)).join("\n"), /secret-step-/);
    assertSecretIsLocal(calls, "secret-step-1");
    assertSecretIsLocal(calls, "secret-step-2");
  } finally {
    mounted.app.unmount();
  }
});

test("an older client-key list cannot roll back a completed rotation", async () => {
  const calls: ApiCall[] = [];
  const initial = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let reads = 0;
  const before = runtimeKey("hint-before", "fp-rotate");
  const rotated = runtimeKey("hint-rotated", before.fingerprint);
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      reads += 1;
      if (reads === 1) return initial.promise;
      if (reads === 2) return { keys: [before], processGeneration: 1, revision: 2 };
      return { keys: [rotated], processGeneration: 1, revision: 3 };
    }),
    createCpaRuntimeKey: tracked(calls, "createCpaRuntimeKey", async () => ({
      ...before,
      processGeneration: 1,
      revision: 2,
      secret: "secret-before",
    })),
    rotateCpaRuntimeKey: tracked(calls, "rotateCpaRuntimeKey", async () => ({
      ...rotated,
      processGeneration: 1,
      revision: 3,
      secret: "secret-rotated",
    })),
  });
  try {
    await press(mounted.root, "添加客户端 Key");
    await settle();
    await press(mounted.root, "轮换 Key");
    await settle();
    assert.match(text(mounted.root), /hint-rotated/);
    assert.equal(revealedSecret(mounted.root), "secret-rotated");
    initial.resolve({ keys: [before] });
    await settle();
    assert.match(text(mounted.root), /hint-rotated/);
    assert.equal(revealedSecret(mounted.root), "secret-rotated");
    assert.doesNotMatch(text(mounted.root), /hint-before/);
    assertSecretIsLocal(calls, "secret-before");
    assertSecretIsLocal(calls, "secret-rotated");
  } finally {
    mounted.app.unmount();
  }
});

test("an older client-key list cannot resurrect a deleted key or its one-time secret", async () => {
  const calls: ApiCall[] = [];
  const initial = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let reads = 0;
  const created = runtimeKey("hint-deleted");
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      reads += 1;
      if (reads === 1) return initial.promise;
      if (reads === 2) return { keys: [created], processGeneration: 1, revision: 2 };
      return { keys: [], processGeneration: 1, revision: 3 };
    }),
    createCpaRuntimeKey: tracked(calls, "createCpaRuntimeKey", async () => ({
      ...created,
      processGeneration: 1,
      revision: 2,
      secret: "secret-deleted",
    })),
    deleteCpaRuntimeKey: tracked(calls, "deleteCpaRuntimeKey", async () => ({ processGeneration: 1, revision: 3 })),
  });
  try {
    await press(mounted.root, "添加客户端 Key");
    await settle();
    assert.equal(revealedSecret(mounted.root), "secret-deleted");
    await press(mounted.root, "删除");
    await settle();
    assert.equal(calls.filter((call) => call.method === "deleteCpaRuntimeKey").length, 1);
    assert.doesNotMatch(text(mounted.root), /hint-deleted/);
    assert.equal(revealedSecret(mounted.root), "");
    initial.resolve({ keys: [created] });
    await settle();
    assert.doesNotMatch(text(mounted.root), /hint-deleted/);
    assert.doesNotMatch(text(mounted.root), /secret-deleted/);
    assert.match(text(mounted.root), /acct-live/);
    assert.equal(calls.filter((call) => call.method === "deleteCpaRuntimeKey").length, 1);
    assertSecretIsLocal(calls, "secret-deleted");
  } finally {
    mounted.app.unmount();
  }
});

test("an account reload and a runtime poll commit without cancelling the current client key", async () => {
  let runtimeReads = 0;
  let accountReads = 0;
  const poll = deferred<ReturnType<typeof runtime>>();
  const accountReload = deferred<{ accounts: ReturnType<typeof accountRow>[] }>();
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true, runtimeOwned: true }),
    getCpaRuntime: async () => {
      runtimeReads += 1;
      if (runtimeReads === 1) {
        return runtime({
          currentOperation: "install",
          latestVersion: "runtime-initial",
          phase: "downloading",
          running: true,
        });
      }
      return poll.promise;
    },
    getCpaAccounts: async () => {
      accountReads += 1;
      if (accountReads === 1) return { accounts: [accountRow("acct-live")] };
      return accountReload.promise;
    },
    getCpaRuntimeKeys: async () => ({ keys: [runtimeKey("hint-live")], processGeneration: 1, revision: 1 }),
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: true }]),
    setCpaAccountStatus: async () => ({ processGeneration: 1, revision: 2 }),
  });
  try {
    assert.equal(runtimeReads, 1);
    assert.match(text(mounted.root), /hint-live/);
    await finishInitialLoad();
    await fireTimers(mounted.window);
    assert.equal(runtimeReads, 2);
    const statusUpdate = press(mounted.root, "停用");
    await settle();
    assert.equal(accountReads, 2);
    accountReload.resolve({ accounts: [accountRow("acct-fresh")] });
    await statusUpdate;
    await settle();
    poll.resolve(runtime({
      currentOperation: "install",
      latestVersion: "runtime-marker",
      phase: "downloading",
      running: true,
    }));
    await settle();
    assert.match(text(mounted.root), /acct-fresh/);
    assert.match(text(mounted.root), /runtime-marker/);
    assert.match(text(mounted.root), /hint-live/);
    assert.match(text(mounted.root), /model-live/);
    assert.doesNotMatch(text(mounted.root), /acct-live/);
  } finally {
    mounted.app.unmount();
  }
});

test("a newer account list wins over an older in-flight list and keeps the current key", async () => {
  let accountReads = 0;
  const reloads: Array<ReturnType<typeof deferred<{ accounts: ReturnType<typeof accountRow>[] }>>> = [];
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      accountReads += 1;
      if (accountReads === 1) return { accounts: [accountRow("acct-live")] };
      const gate = deferred<{ accounts: ReturnType<typeof accountRow>[] }>();
      reloads.push(gate);
      return gate.promise;
    },
    getCpaRuntimeKeys: async () => ({ keys: [runtimeKey("hint-held")], processGeneration: 1, revision: 1 }),
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: true }]),
    setCpaAccountStatus: async () => ({ processGeneration: 1, revision: 2 }),
  });
  try {
    assert.match(text(mounted.root), /acct-live/);
    const statusUpdate = press(mounted.root, "停用");
    await settle();
    assert.equal(accountReads, 2);
    const refresh = press(mounted.root, "刷新");
    await settle();
    assert.equal(accountReads, 3);
    reloads[1]?.resolve({ accounts: [accountRow("acct-fresh")] });
    await settle();
    reloads[0]?.resolve({ accounts: [accountRow("acct-stale")] });
    await statusUpdate;
    await refresh;
    await settle();
    assert.match(text(mounted.root), /acct-fresh/);
    assert.doesNotMatch(text(mounted.root), /acct-stale/);
    assert.match(text(mounted.root), /hint-held/);
    assert.match(text(mounted.root), /model-live/);
  } finally {
    mounted.app.unmount();
  }
});

test("logout clears CPA server rows and a late create cannot repopulate the secret", async () => {
  const calls: ApiCall[] = [];
  const createGate = deferred<ReturnType<typeof runtimeKey> & { processGeneration: number; revision: number; secret: string }>();
  const keyGates: Array<ReturnType<typeof deferred<{ keys: ReturnType<typeof runtimeKey>[] }>>> = [];
  let creates = 0;
  let keyReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true, modelCount: 1 }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: true }]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      keyReads += 1;
      if (keyReads < 3) return { keys: [runtimeKey("hint-live")], processGeneration: 1, revision: keyReads };
      const gate = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
      keyGates.push(gate);
      return gate.promise;
    }),
    createCpaRuntimeKey: tracked(calls, "createCpaRuntimeKey", async () => {
      creates += 1;
      if (creates === 1) {
        return { ...runtimeKey("hint-live"), processGeneration: 1, revision: 2, secret: "secret-live" };
      }
      return createGate.promise;
    }),
  });
  try {
    await press(mounted.root, "添加客户端 Key");
    await settle();
    assert.equal(revealedSecret(mounted.root), "secret-live");
    const creating = press(mounted.root, "添加客户端 Key");
    await settle();
    cpaSession().drop();
    const cleared = activeCpa();
    assert.equal(cleared.integration, null);
    assert.equal(cleared.runtimeKeys.length, 0);
    assert.equal(cleared.cpaAccounts.length, 0);
    assert.equal(cleared.catalogModels.length, 0);
    assert.equal(cleared.loaded, false);
    assert.equal(cleared.loading, false);
    assert.equal(cleared.keysLoading, false);
    assert.equal(cleared.accountsLoading, false);
    assert.equal(cleared.catalogLoading, false);
    await settle();
    createGate.resolve({ ...runtimeKey("hint-poison"), processGeneration: 1, revision: 4, secret: "secret-poison" });
    await settle();
    keyGates[0]?.resolve({ keys: [runtimeKey("hint-poison")] });
    await creating;
    await settle();
    assert.equal(revealedSecret(mounted.root), "");
    assert.doesNotMatch(text(mounted.root), /secret-live|secret-poison|hint-live|hint-poison|acct-live|model-live/);
    assertSecretIsLocal(calls, "secret-live");
    assertSecretIsLocal(calls, "secret-poison");
  } finally {
    mounted.app.unmount();
  }
});

test("a 401 drops the session so a later key list cannot repopulate the reveal", async () => {
  const calls: ApiCall[] = [];
  const keyGate = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let accountReads = 0;
  let keyReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      accountReads += 1;
      if (accountReads === 1) return { accounts: [accountRow("acct-live")] };
      cpaSession().drop();
      throw Object.assign(new Error("unauthorized"), { status: 401 });
    },
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: true }]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [runtimeKey("hint-live")], processGeneration: 1, revision: 1 };
      return keyGate.promise;
    }),
    createCpaRuntimeKey: tracked(calls, "createCpaRuntimeKey", async () => ({
      ...runtimeKey("hint-live"),
      processGeneration: 1,
      revision: 2,
      secret: "secret-live",
    })),
    setCpaAccountStatus: async () => ({ processGeneration: 1, revision: 2 }),
  });
  try {
    const creating = press(mounted.root, "添加客户端 Key");
    await settle();
    assert.equal(revealedSecret(mounted.root), "secret-live");
    assert.equal(keyReads, 2);
    const statusUpdate = press(mounted.root, "停用");
    await settle();
    assert.equal(accountReads, 2);
    keyGate.resolve({ keys: [runtimeKey("hint-poison")] });
    await creating;
    await statusUpdate;
    await settle();
    assert.equal(revealedSecret(mounted.root), "");
    assert.doesNotMatch(text(mounted.root), /secret-live|hint-poison|hint-live|acct-live|model-live/);
    assertSecretIsLocal(calls, "secret-live");
  } finally {
    mounted.app.unmount();
  }
});

test("a cached page ignores key and account payloads that resolve after it is left", async () => {
  const createGate = deferred<ReturnType<typeof runtimeKey> & { processGeneration: number; revision: number; secret: string }>();
  const keyGates: Array<ReturnType<typeof deferred<{ keys: ReturnType<typeof runtimeKey>[] }>>> = [];
  const accountGates: Array<ReturnType<typeof deferred<{ accounts: ReturnType<typeof accountRow>[] }>>> = [];
  let integrationReads = 0;
  let creates = 0;
  let armed = false;
  let armedAccountReads = 0;
  let armedKeyReads = 0;
  const mounted = await mountKept({
    getCpaIntegration: async () => {
      integrationReads += 1;
      return integration({ runtimeRunning: true, modelCount: 1 });
    },
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      if (!armed) return { accounts: [accountRow("acct-live")] };
      armedAccountReads += 1;
      if (armedAccountReads > 1) return { accounts: [accountRow("acct-live")] };
      const gate = deferred<{ accounts: ReturnType<typeof accountRow>[] }>();
      accountGates.push(gate);
      return gate.promise;
    },
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: false }]),
    getCpaRuntimeKeys: async () => {
      if (!armed) return { keys: [runtimeKey("hint-live")], processGeneration: 1, revision: 1 };
      armedKeyReads += 1;
      if (armedKeyReads > 1) return { keys: [runtimeKey("hint-live")], processGeneration: 1, revision: 2 };
      const gate = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
      keyGates.push(gate);
      return gate.promise;
    },
    createCpaRuntimeKey: async () => {
      creates += 1;
      if (creates === 1) {
        return { ...runtimeKey("hint-live"), processGeneration: 1, revision: 2, secret: "secret-live" };
      }
      return createGate.promise;
    },
    setCpaAccountStatus: async () => ({ processGeneration: 1, revision: 2 }),
  });
  try {
    mounted.showing.value = false;
    await settle(40);
    mounted.showing.value = true;
    await settle(40);
    const readsAfterReturn = integrationReads;
    await press(mounted.root, "添加客户端 Key");
    await settle();
    assert.equal(revealedSecret(mounted.root), "secret-live");
    armed = true;
    const creating = press(mounted.root, "添加客户端 Key");
    const statusUpdate = press(mounted.root, "停用");
    await settle();
    assert.equal(accountGates.length, 1);
    mounted.showing.value = false;
    await settle(40);
    createGate.resolve({ ...runtimeKey("hint-poison"), processGeneration: 1, revision: 4, secret: "secret-poison" });
    await settle();
    keyGates[0]?.resolve({ keys: [runtimeKey("hint-poison")] });
    accountGates[0]?.resolve({ accounts: [accountRow("acct-poison")] });
    await creating;
    await statusUpdate;
    await settle();
    mounted.showing.value = true;
    await settle(40);
    assert.equal(integrationReads, readsAfterReturn, "returning inside the revalidation window must not start another load");
    assert.equal(revealedSecret(mounted.root), "secret-live");
    assert.doesNotMatch(text(mounted.root), /secret-poison|hint-poison|acct-poison/);
    assert.match(text(mounted.root), /hint-live/);
    assert.match(text(mounted.root), /acct-live/);
  } finally {
    mounted.app.unmount();
  }
});

test("unmounting ignores a late key mutation and the next mount does not reveal it", async () => {
  const createGate = deferred<ReturnType<typeof runtimeKey> & { processGeneration: number; revision: number; secret: string }>();
  let creates = 0;
  let keyReads = 0;
  const first = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads < 3) return { keys: [runtimeKey("hint-live")], processGeneration: 1, revision: keyReads };
      return { keys: [runtimeKey("hint-poison")], processGeneration: 1, revision: 4 };
    },
    createCpaRuntimeKey: async () => {
      creates += 1;
      if (creates === 1) {
        return { ...runtimeKey("hint-live"), processGeneration: 1, revision: 2, secret: "secret-live" };
      }
      return createGate.promise;
    },
  });
  await press(first.root, "添加客户端 Key");
  await settle();
  const creating = press(first.root, "添加客户端 Key");
  await settle();
  first.app.unmount();
  createGate.resolve({ ...runtimeKey("hint-poison"), processGeneration: 1, revision: 4, secret: "secret-poison" });
  await creating.catch(() => undefined);
  await settle();
  const second = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
  });
  try {
    assert.equal(revealedSecret(second.root), "");
    assert.doesNotMatch(text(second.root), /secret-poison|hint-poison|secret-live/);
  } finally {
    second.app.unmount();
  }
});

test("a deleted CLI import can be imported again after a failed account read and a later success", async () => {
  const calls: ApiCall[] = [];
  const importedName = "ocg-cli-codex-synth";
  const source = {
    available: true,
    provider: "codex" as const,
    reason: null,
    source: "synthetic-cli",
    supported: true,
  };
  let accountReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCliImports: async () => ({ sources: [source] }),
    getCpaAccounts: async () => {
      accountReads += 1;
      if (accountReads === 2) return { accounts: [accountRow(importedName)] };
      if (accountReads === 3) throw new Error("account revalidation failed");
      return { accounts: [] };
    },
    importCpaCliAccount: tracked(calls, "importCpaCliAccount", async () => ({
      name: `${importedName}.json`,
      outcome: "imported" as const,
      processGeneration: 1,
      provider: "codex" as const,
      revision: 2,
    })),
    deleteCpaAccount: tracked(calls, "deleteCpaAccount", async () => ({ processGeneration: 1, revision: 3 })),
  });
  try {
    const importCount = () => calls.filter((call) => call.method === "importCpaCliAccount").length;
    const deleteCount = () => calls.filter((call) => call.method === "deleteCpaAccount").length;
    await press(mounted.root, "导入 Codex");
    await settle();
    assert.equal(importCount(), 1);
    assert.match(text(mounted.root), new RegExp(importedName));
    assert.equal(button(mounted.root, "导入 Codex").props.disabled, true);
    await press(mounted.root, "删除");
    await settle();
    assert.equal(accountReads, 3);
    assert.equal(deleteCount(), 1);
    assert.equal(importCount(), 1);
    await press(mounted.root, "刷新");
    await settle();
    assert.equal(accountReads, 4);
    assert.equal(deleteCount(), 1);
    assert.equal(importCount(), 1);
    assert.doesNotMatch(text(mounted.root), new RegExp(importedName));
    assert.ok(!button(mounted.root, "导入 Codex").props.disabled, "a successful account read must allow the deleted CLI source to be imported again");
    await press(mounted.root, "导入 Codex");
    await settle();
    assert.equal(importCount(), 2);
    assert.equal(deleteCount(), 1);
  } finally {
    mounted.app.unmount();
  }
});

test("retry clicks pass the click event and still commit keys, accounts, and catalog", async () => {
  let keyReads = 0;
  let accountReads = 0;
  let catalogReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      accountReads += 1;
      if (accountReads === 1) throw new Error("accounts-down");
      return { accounts: [accountRow("acct-retried")] };
    },
    getCpaCatalog: async () => {
      catalogReads += 1;
      if (catalogReads === 1) throw new Error("catalog-down");
      return catalogBody([{ id: "model-retried", enabled: true }]);
    },
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads === 1) throw new Error("keys-down");
      return { keys: [runtimeKey("hint-retried")], processGeneration: 1, revision: 2 };
    },
  });
  try {
    assert.equal(keyReads, 1);
    assert.equal(accountReads, 1);
    assert.equal(catalogReads, 1);
    assertFirstLoadReadError(mounted.root, "keys-down");
    assertFirstLoadReadError(mounted.root, "accounts-down");
    assertFirstLoadReadError(mounted.root, "catalog-down");
    assert.equal(keyRows(mounted.root).length, 0, "a first-load key error must not present an empty success");
    assert.equal(accountRows(mounted.root).length, 0, "a first-load account error must not present an empty success");
    assert.equal(catalogCards(mounted.root).length, 0, "a first-load catalog error must not present an empty success");
    const retries = buttonsByLabel(mounted.root, "重试");
    assert.equal(retries.length, 3, "keys, accounts, and catalog each expose one retry");
    for (const retry of retries) {
      const event = uiClick();
      assert.ok(event instanceof MouseEvent, "the retry handler must receive a click event");
      await (retry.props.onClick as (event: MouseEvent) => Promise<void> | void)(event);
    }
    await settle();
    assert.equal(keyReads, 2, "the client-key retry must issue a GET");
    assert.equal(accountReads, 2, "the account retry must issue a GET");
    assert.equal(catalogReads, 2, "the catalog retry must issue a GET");
    assert.match(text(mounted.root), /hint-retried/);
    assert.match(text(mounted.root), /acct-retried/);
    assert.match(text(mounted.root), /model-retried/);
    assert.equal(activeCpa().runtimeKeys[0]?.hint, "hint-retried");
    assert.equal(activeCpa().keysLoading, false);
    assert.equal(activeCpa().accountsLoading, false);
    assert.equal(activeCpa().catalogLoading, false);
    assert.equal(noticesWithMarker(mounted.root, "keys-down").length, 0);
    assert.equal(noticesWithMarker(mounted.root, "accounts-down").length, 0);
    assert.equal(noticesWithMarker(mounted.root, "catalog-down").length, 0);
  } finally {
    mounted.app.unmount();
  }
});

test("existing catalog rows stay on a failed refresh with a warning retry that issues one GET", async () => {
  const calls: ApiCall[] = [];
  let keyReads = 0;
  let accountReads = 0;
  let catalogReads = 0;
  const models = [
    { id: "model-kept", enabled: true },
    { id: "model-peer", enabled: false },
  ];
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true, modelCount: 2 }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      accountReads += 1;
      return { accounts: [accountRow("acct-kept")] };
    },
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      return { keys: [runtimeKey("hint-kept")], processGeneration: 1, revision: 1 };
    },
    getCpaCatalog: tracked(calls, "getCpaCatalog", async () => {
      catalogReads += 1;
      if (catalogReads === 2) throw new Error("catalog-refresh-failed");
      return catalogBody(models);
    }),
    putCpaCatalog: tracked(calls, "putCpaCatalog", async () => catalogBody(models)),
    refreshCpaModels: tracked(calls, "refreshCpaModels", async () => ({
      ...catalogBody(models),
      processGeneration: 1,
      revision: 2,
    })),
  });
  try {
    await finishInitialLoad();
    assert.equal(catalogCards(mounted.root).length, 2);
    assert.match(text(mounted.root), /model-kept/);
    assert.match(text(mounted.root), /model-peer/);
    const keysAtRefresh = keyReads;
    const accountsAtRefresh = accountReads;
    await press(mounted.root, "刷新");
    await waitForCount(() => catalogReads, 2, "catalog refresh GET");
    await waitForCount(() => keyReads, keysAtRefresh + 1, "page refresh key GET");
    await waitForCount(() => accountReads, accountsAtRefresh + 1, "page refresh account GET");
    await settle();
    assert.equal(catalogReads, 2);
    assert.equal(catalogCards(mounted.root).length, 2, "cached catalog rows stay after a failed refresh");
    assert.match(text(mounted.root), /model-kept/);
    assert.match(text(mounted.root), /model-peer/);
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "catalog-refresh-failed",
      reads: () => catalogReads,
      writes: () => calls.filter((call) => call.method === "putCpaCatalog" || call.method === "refreshCpaModels").length,
      others: [
        { read: () => keyReads, label: "client-key GET" },
        { read: () => accountReads, label: "account GET" },
      ],
    });
    assert.equal(keyReads, keysAtRefresh + 1, "page refresh may reread keys once; retry must not");
    assert.equal(accountReads, accountsAtRefresh + 1, "page refresh may reread accounts once; retry must not");
    assert.equal(catalogCards(mounted.root).length, 2);
    assert.match(text(mounted.root), /model-kept/);
    assert.match(text(mounted.root), /model-peer/);
  } finally {
    mounted.app.unmount();
  }
});

test("a late old-session rotate cannot invalidate the next session's key read", async () => {
  const rotateGate = deferred<ReturnType<typeof runtimeKey> & { processGeneration: number; revision: number; secret: string }>();
  const nextKeys = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let keyReads = 0;
  let rotates = 0;
  const before = runtimeKey("hint-before");
  const api: CpaApi = {
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-next")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-next", enabled: true }]),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [before], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return nextKeys.promise;
      return { keys: [runtimeKey("hint-poison", before.fingerprint)], processGeneration: 1, revision: 9 };
    },
    rotateCpaRuntimeKey: async () => {
      rotates += 1;
      return rotateGate.promise;
    },
  };
  const first = await mount(api);
  let primary: App | null = first.app;
  try {
    const rotating = press(first.root, "轮换 Key");
    await settle();
    assert.equal(rotates, 1);
    const errorsBeforeDrop = errorMessages().length;
    cpaSession().drop();
    const cleared = activeCpa();
    assert.equal(cleared.integration, null);
    assert.equal(cleared.runtimeKeys.length, 0);
    assert.equal(cleared.cpaAccounts.length, 0);
    assert.equal(cleared.loading, false);
    assert.equal(cleared.keysLoading, false);
    await settle();
    assert.equal(revealedSecret(first.root), "");
    primary.unmount();
    primary = null;
    const second = await mount(api);
    try {
      assert.equal(keyReads, 2, "the next session keeps its own client-key read in flight");
      rotateGate.resolve({
        ...runtimeKey("hint-poison", before.fingerprint),
        processGeneration: 1,
        revision: 4,
        secret: "secret-poison",
      });
      await rotating;
      await settle();
      assert.equal(errorMessages().length, errorsBeforeDrop, "a stale rotate must not notify");
      nextKeys.resolve({ keys: [runtimeKey("hint-next")] });
      await settle();
      assert.match(text(second.root), /hint-next/);
      assert.match(text(second.root), /acct-next/);
      assert.match(text(second.root), /model-next/);
      assert.doesNotMatch(text(second.root), /hint-poison|hint-before|secret-poison/);
      assert.equal(revealedSecret(second.root), "");
      assert.deepEqual(activeCpa().runtimeKeys.map((key) => key.hint), ["hint-next"]);
    } finally {
      second.app.unmount();
    }
  } finally {
    primary?.unmount();
  }
});

test("a late old-session delete cannot remove the next session's keys", async () => {
  const deleteGate = deferred<{ processGeneration: number; revision: number }>();
  const nextKeys = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let keyReads = 0;
  let deletes = 0;
  const removed = runtimeKey("hint-removed", "fp-shared");
  const api: CpaApi = {
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [removed], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return nextKeys.promise;
      return { keys: [], processGeneration: 1, revision: 9 };
    },
    deleteCpaRuntimeKey: async () => {
      deletes += 1;
      return deleteGate.promise;
    },
  };
  const first = await mount(api);
  let primary: App | null = first.app;
  try {
    press(first.root, "删除");
    await settle();
    assert.equal(deletes, 1);
    cpaSession().drop();
    assert.equal(activeCpa().runtimeKeys.length, 0);
    await settle();
    primary.unmount();
    primary = null;
    const second = await mount(api);
    try {
      assert.equal(keyReads, 2, "the next session keeps its own client-key read in flight");
      deleteGate.resolve({ processGeneration: 1, revision: 4 });
      await settle();
      assert.equal(deletes, 1, "the stale delete is not replayed");
      assert.equal(errorMessages().length, 0, "a stale delete must not notify");
      nextKeys.resolve({ keys: [runtimeKey("hint-kept", "fp-shared")] });
      await settle();
      assert.match(text(second.root), /hint-kept/);
      assert.doesNotMatch(text(second.root), /hint-removed/);
      assert.deepEqual(activeCpa().runtimeKeys.map((key) => key.fingerprint), ["fp-shared"]);
      assert.equal(activeCpa().runtimeKeys[0]?.hint, "hint-kept");
    } finally {
      second.app.unmount();
    }
  } finally {
    primary?.unmount();
  }
});

test("an unmounted client-key acknowledgement cannot invalidate the next page or release its create", async () => {
  const firstCreate = deferred<ReturnType<typeof runtimeKey> & { processGeneration: number; revision: number; secret: string }>();
  const secondCreate = deferred<ReturnType<typeof runtimeKey> & { processGeneration: number; revision: number; secret: string }>();
  const nextKeys = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let keyReads = 0;
  let creates = 0;
  const api: CpaApi = {
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: true }]),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [runtimeKey("hint-old")], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return nextKeys.promise;
      return { keys: [runtimeKey("hint-kept")], processGeneration: 1, revision: keyReads };
    },
    createCpaRuntimeKey: async () => {
      creates += 1;
      return creates === 1 ? firstCreate.promise : secondCreate.promise;
    },
  };
  const first = await mount(api);
  let primary: App | null = first.app;
  try {
    const creating = press(first.root, "添加客户端 Key");
    await settle();
    assert.equal(creates, 1);
    primary.unmount();
    primary = null;
    const second = await mount(api);
    try {
      assert.equal(keyReads, 2, "the next page keeps its own client-key read in flight");
      const newer = press(second.root, "添加客户端 Key");
      await settle();
      assert.equal(creates, 2);
      assert.equal(button(second.root, "添加客户端 Key").props.loading, true);
      const errorsBeforeAck = errorMessages().length;
      firstCreate.resolve({
        ...runtimeKey("hint-poison"),
        processGeneration: 1,
        revision: 4,
        secret: "secret-poison",
      });
      await creating;
      await settle();
      assert.equal(errorMessages().length, errorsBeforeAck, "a stale create must not notify");
      assert.equal(button(second.root, "添加客户端 Key").props.loading, true, "a stale finally must not release the newer create");
      assert.equal(revealedSecret(second.root), "");
      nextKeys.resolve({ keys: [runtimeKey("hint-next")] });
      await settle();
      assert.match(text(second.root), /hint-next/);
      assert.doesNotMatch(text(second.root), /hint-poison|secret-poison/);
      secondCreate.resolve({
        ...runtimeKey("hint-kept"),
        processGeneration: 1,
        revision: 5,
        secret: "secret-kept",
      });
      await newer;
      await settle();
      assert.equal(revealedSecret(second.root), "secret-kept");
      assert.match(text(second.root), /hint-kept/);
      assert.equal(button(second.root, "添加客户端 Key").props.loading, false);
      assert.doesNotMatch(text(second.root), /secret-poison|hint-poison/);
      assert.match(text(second.root), /acct-live/);
      assert.match(text(second.root), /model-live/);
    } finally {
      second.app.unmount();
    }
  } finally {
    primary?.unmount();
  }
});

test("a stale client-key failure does not notify or discard the newer key read", async () => {
  const createGate = deferred<ReturnType<typeof runtimeKey> & { processGeneration: number; revision: number; secret: string }>();
  const nextKeys = deferred<{ keys: ReturnType<typeof runtimeKey>[] }>();
  let keyReads = 0;
  let creates = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: false }]),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [runtimeKey("hint-old")], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return nextKeys.promise;
      return { keys: [runtimeKey("hint-poison")], processGeneration: 1, revision: 9 };
    },
    createCpaRuntimeKey: async () => {
      creates += 1;
      return createGate.promise;
    },
  });
  try {
    const creating = press(mounted.root, "添加客户端 Key");
    await settle();
    assert.equal(creates, 1);
    const refresh = press(mounted.root, "刷新");
    await settle();
    assert.equal(keyReads, 2, "refresh leaves a current client-key read in flight");
    const errorsBeforeFailure = errorMessages().length;
    createGate.reject(new Error("create-stale"));
    await creating;
    await settle();
    assert.equal(errorMessages().length, errorsBeforeFailure, "a stale create failure must not notify");
    assert.equal(button(mounted.root, "添加客户端 Key").props.loading, false);
    nextKeys.resolve({ keys: [runtimeKey("hint-fresh")] });
    await refresh;
    await settle();
    assert.match(text(mounted.root), /hint-fresh/);
    assert.match(text(mounted.root), /acct-live/);
    assert.match(text(mounted.root), /model-live/);
    assert.doesNotMatch(text(mounted.root), /hint-poison|hint-old|secret-/);
    assert.equal(revealedSecret(mounted.root), "");
    assert.deepEqual(activeCpa().runtimeKeys.map((key) => key.hint), ["hint-fresh"]);
  } finally {
    mounted.app.unmount();
  }
});

test("the first client-key GET is the only load gate; session reset clears committed rows without a new GET", async () => {
  const initial = deferredRead<{ keys: ReturnType<typeof runtimeKey>[]; processGeneration: number; revision: number }>();
  let keyReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-first")] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads === 1) return initial.promise;
      throw new Error("session reset must not start a new client-key GET");
    },
  });
  try {
    await waitForCount(() => keyReads, 1, "initial client-key GET");
    assert.equal(keyRows(mounted.root).length, 0, "first-load rows wait for the initial GET");
    assert.doesNotMatch(text(mounted.root), /hint-first/);
    initial.resolve({ keys: [runtimeKey("hint-first")], processGeneration: 1, revision: 1 });
    await settle();
    assert.match(text(mounted.root), /hint-first/);
    assert.match(text(mounted.root), /acct-first/);
    assert.equal(keyReads, 1);
    cpaSession().drop();
    await settle();
    assert.equal(keyRows(mounted.root).length, 0);
    assert.doesNotMatch(text(mounted.root), /hint-first|acct-first/);
    assert.equal(keyReads, 1, "reset must not start a new GET");
  } finally {
    initial.reject(new Error("unsettled first-load GET"));
    mounted.app.unmount();
  }
});

test("creating a client Key settles at ack while its one follow-up GET is deferred", async () => {
  const calls: ApiCall[] = [];
  const followup = deferredRead<{ keys: ReturnType<typeof runtimeKey>[]; processGeneration: number; revision: number }>();
  let keyReads = 0;
  let accountReads = 0;
  let catalogReads = 0;
  const kept = runtimeKey("hint-kept");
  const created = runtimeKey("hint-created");
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      accountReads += 1;
      return { accounts: [accountRow("acct-kept")] };
    },
    getCpaCatalog: async () => {
      catalogReads += 1;
      return catalogBody([]);
    },
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [kept], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return followup.promise;
      if (keyReads === 3) return { keys: [kept, created], processGeneration: 1, revision: 3 };
      throw new Error("client-key create retry must issue only one GET");
    }),
    createCpaRuntimeKey: tracked(calls, "createCpaRuntimeKey", async () => ({
      ...created,
      processGeneration: 1,
      revision: 2,
      secret: "secret-ack-now",
    })),
  });
  await finishInitialLoad();
  const creating = press(mounted.root, "添加客户端 Key");
  const trackedAction = trackPromise(creating);
  try {
    await waitForCount(() => keyReads, 2, "create follow-up GET");
    assert.equal(trackedAction.status(), "settled", "the create handler must settle at the ack");
    assert.equal(calls.filter((call) => call.method === "createCpaRuntimeKey").length, 1);
    assert.equal(keyReads, 2);
    assert.equal(button(mounted.root, "添加客户端 Key").props.loading, false);
    assert.ok(!button(mounted.root, "添加客户端 Key").props.disabled);
    assert.equal(revealedSecret(mounted.root), "secret-ack-now");
    assert.match(text(mounted.root), /hint-kept/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.equal(keyRows(mounted.root).length >= 1, true, "committed key rows stay rendered while revalidation is pending");
    followup.reject(new Error("create revalidation failed"));
    await settle();
    assert.equal(calls.filter((call) => call.method === "createCpaRuntimeKey").length, 1, "a failed read must not replay the create");
    assert.equal(keyReads, 2);
    assert.equal(revealedSecret(mounted.root), "secret-ack-now");
    assert.match(text(mounted.root), /hint-kept/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.equal(keyRows(mounted.root).length >= 1, true, "cached key rows stay after the failed revalidation GET");
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "create revalidation failed",
      reads: () => keyReads,
      writes: () => calls.filter((call) => call.method === "createCpaRuntimeKey").length,
      others: [
        { read: () => accountReads, label: "account GET" },
        { read: () => catalogReads, label: "catalog GET" },
      ],
    });
    assert.equal(revealedSecret(mounted.root), "secret-ack-now");
    assert.match(text(mounted.root), /hint-kept/);
    assert.match(text(mounted.root), /acct-kept/);
    assertSecretIsLocal(calls, "secret-ack-now");
  } finally {
    followup.reject(new Error("unsettled create follow-up GET"));
    await creating.then(() => undefined, () => undefined);
    mounted.app.unmount();
  }
});

test("rotating a client Key settles at ack with a usable secret while the follow-up GET is deferred", async () => {
  const calls: ApiCall[] = [];
  const followup = deferredRead<{ keys: ReturnType<typeof runtimeKey>[]; processGeneration: number; revision: number }>();
  let keyReads = 0;
  let accountReads = 0;
  const kept = { ...runtimeKey("hint-kept", "fp-kept"), protected: true as const };
  const before = runtimeKey("hint-before", "fp-rotate");
  const rotated = runtimeKey("hint-rotated", "fp-rotate");
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      accountReads += 1;
      return { accounts: [accountRow("acct-kept")] };
    },
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [kept, before], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return followup.promise;
      if (keyReads === 3) return { keys: [kept, rotated], processGeneration: 1, revision: 3 };
      throw new Error("client-key rotate retry must issue only one GET");
    }),
    rotateCpaRuntimeKey: tracked(calls, "rotateCpaRuntimeKey", async () => ({
      ...rotated,
      processGeneration: 1,
      revision: 2,
      secret: "secret-rotated-now",
    })),
  });
  await finishInitialLoad();
  assert.match(text(mounted.root), /hint-before/);
  const rotating = press(mounted.root, "轮换 Key");
  const trackedAction = trackPromise(rotating);
  try {
    await waitForCount(() => keyReads, 2, "rotate follow-up GET");
    assert.equal(trackedAction.status(), "settled", "the rotate handler must settle at the ack");
    assert.equal(calls.filter((call) => call.method === "rotateCpaRuntimeKey").length, 1);
    const rotateButtons = buttonsByLabel(mounted.root, "轮换 Key");
    assert.equal(rotateButtons.length >= 1, true, "the key row stays rendered while revalidation is pending");
    assert.equal(rotateButtons.every((node) => node.props.loading !== true), true, "key action flags release at ack");
    assert.equal(revealedSecret(mounted.root), "secret-rotated-now");
    assert.match(text(mounted.root), /hint-kept/);
    assert.match(text(mounted.root), /acct-kept/);
    followup.reject(new Error("rotate revalidation failed"));
    await settle();
    assert.equal(calls.filter((call) => call.method === "rotateCpaRuntimeKey").length, 1);
    assert.equal(keyReads, 2);
    assert.equal(revealedSecret(mounted.root), "secret-rotated-now");
    assert.match(text(mounted.root), /hint-kept/);
    assert.match(text(mounted.root), /hint-rotated/);
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "rotate revalidation failed",
      reads: () => keyReads,
      writes: () => calls.filter((call) => call.method === "rotateCpaRuntimeKey").length,
      others: [{ read: () => accountReads, label: "account GET" }],
    });
    assert.equal(revealedSecret(mounted.root), "secret-rotated-now");
    assert.match(text(mounted.root), /hint-kept/);
    assert.match(text(mounted.root), /hint-rotated/);
    assertSecretIsLocal(calls, "secret-rotated-now");
  } finally {
    followup.reject(new Error("unsettled rotate follow-up GET"));
    await rotating.then(() => undefined, () => undefined);
    mounted.app.unmount();
  }
});

test("deleting a client Key removes the row at ack before its follow-up GET", async () => {
  const calls: ApiCall[] = [];
  const followup = deferredRead<{ keys: ReturnType<typeof runtimeKey>[]; processGeneration: number; revision: number }>();
  let keyReads = 0;
  let accountReads = 0;
  const gone = runtimeKey("hint-gone", "fp-gone");
  const kept = runtimeKey("hint-kept", "fp-kept");
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => {
      accountReads += 1;
      return { accounts: [accountRow("acct-kept")] };
    },
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: tracked(calls, "getCpaRuntimeKeys", async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [gone, kept], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return followup.promise;
      if (keyReads === 3) return { keys: [kept], processGeneration: 1, revision: 3 };
      throw new Error("client-key delete retry must issue only one GET");
    }),
    deleteCpaRuntimeKey: tracked(calls, "deleteCpaRuntimeKey", async () => ({ processGeneration: 1, revision: 2 })),
  });
  await finishInitialLoad();
  assert.match(text(mounted.root), /hint-gone/);
  press(mounted.root, "删除");
  try {
    await waitForCount(() => keyReads, 2, "delete follow-up GET");
    await waitForCount(() => calls.filter((call) => call.method === "deleteCpaRuntimeKey").length, 1, "delete write");
    assert.equal(button(mounted.root, "添加客户端 Key").props.loading, false, "key action flags release at ack");
    assert.equal(calls.filter((call) => call.method === "deleteCpaRuntimeKey").length, 1);
    assert.doesNotMatch(text(mounted.root), /hint-gone/);
    assert.match(text(mounted.root), /hint-kept/);
    assert.match(text(mounted.root), /acct-kept/);
    followup.reject(new Error("delete revalidation failed"));
    await settle();
    assert.equal(calls.filter((call) => call.method === "deleteCpaRuntimeKey").length, 1, "a failed read must not replay the delete");
    assert.equal(keyReads, 2);
    assert.doesNotMatch(text(mounted.root), /hint-gone/);
    assert.match(text(mounted.root), /hint-kept/);
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "delete revalidation failed",
      reads: () => keyReads,
      writes: () => calls.filter((call) => call.method === "deleteCpaRuntimeKey").length,
      others: [{ read: () => accountReads, label: "account GET" }],
    });
    assert.doesNotMatch(text(mounted.root), /hint-gone/);
    assert.match(text(mounted.root), /hint-kept/);
  } finally {
    followup.reject(new Error("unsettled delete follow-up GET"));
    await settle();
    mounted.app.unmount();
  }
});

test("account status settles at ack with the receipt field applied while its GET is deferred", async () => {
  const calls: ApiCall[] = [];
  const followup = deferredRead<{ accounts: ReturnType<typeof accountRow>[] }>();
  let accountReads = 0;
  let keyReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      return { keys: [runtimeKey("hint-kept")], processGeneration: 1, revision: 1 };
    },
    getCpaCatalog: async () => catalogBody([]),
    getCpaAccounts: tracked(calls, "getCpaAccounts", async () => {
      accountReads += 1;
      if (accountReads === 1) {
        return { accounts: [accountRow("acct-target"), accountRow("acct-kept")] };
      }
      if (accountReads === 2) return followup.promise;
      if (accountReads === 3) {
        return { accounts: [accountRow("acct-target", { disabled: true }), accountRow("acct-kept")] };
      }
      throw new Error("account status retry must issue only one GET");
    }),
    setCpaAccountStatus: tracked(calls, "setCpaAccountStatus", async () => ({ processGeneration: 1, revision: 2 })),
  });
  await finishInitialLoad();
  const updating = press(mounted.root, "停用");
  const trackedAction = trackPromise(updating);
  try {
    await waitForCount(() => accountReads, 2, "status follow-up GET");
    assert.equal(trackedAction.status(), "settled", "the status handler must settle at the ack");
    assert.equal(calls.filter((call) => call.method === "setCpaAccountStatus").length, 1);
    assert.ok(button(mounted.root, "启用"), "the receipt disables the targeted account in place");
    assert.ok(button(mounted.root, "停用"), "the sibling account stays enabled");
    assert.match(text(mounted.root), /acct-target/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.match(text(mounted.root), /hint-kept/);
    assert.equal(accountRows(mounted.root).length, 2);
    followup.reject(new Error("status revalidation failed"));
    await settle();
    assert.equal(calls.filter((call) => call.method === "setCpaAccountStatus").length, 1);
    assert.equal(accountReads, 2);
    assert.ok(button(mounted.root, "启用"));
    assert.match(text(mounted.root), /acct-target/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.equal(accountRows(mounted.root).length, 2);
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "status revalidation failed",
      reads: () => accountReads,
      writes: () => calls.filter((call) => call.method === "setCpaAccountStatus").length,
      others: [{ read: () => keyReads, label: "client-key GET" }],
    });
    assert.ok(button(mounted.root, "启用"));
    assert.match(text(mounted.root), /acct-target/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.equal(accountRows(mounted.root).length, 2);
  } finally {
    followup.reject(new Error("unsettled status follow-up GET"));
    await updating.then(() => undefined, () => undefined);
    mounted.app.unmount();
  }
});

test("account quota reset settles at ack with the receipt field applied while its GET is deferred", async () => {
  const calls: ApiCall[] = [];
  const followup = deferredRead<{ accounts: ReturnType<typeof accountRow>[] }>();
  let accountReads = 0;
  let keyReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      return { keys: [], processGeneration: 1, revision: 1 };
    },
    getCpaCatalog: async () => catalogBody([]),
    getCpaAccounts: tracked(calls, "getCpaAccounts", async () => {
      accountReads += 1;
      if (accountReads === 1) {
        return {
          accounts: [
            accountRow("acct-quota", { quota: { remaining: 42 } }),
            accountRow("acct-kept", { quota: { remaining: 7 } }),
          ],
        };
      }
      if (accountReads === 2) return followup.promise;
      if (accountReads === 3) {
        return {
          accounts: [
            accountRow("acct-quota", { quota: null }),
            accountRow("acct-kept", { quota: { remaining: 7 } }),
          ],
        };
      }
      throw new Error("quota reset retry must issue only one GET");
    }),
    resetCpaQuota: tracked(calls, "resetCpaQuota", async () => ({ processGeneration: 1, revision: 2 })),
  });
  await finishInitialLoad();
  assert.match(text(mounted.root), /42/);
  const resetting = press(mounted.root, "重置配额");
  const trackedAction = trackPromise(resetting);
  try {
    await waitForCount(() => accountReads, 2, "quota reset follow-up GET");
    assert.equal(trackedAction.status(), "settled", "the quota reset handler must settle at the ack");
    assert.equal(calls.filter((call) => call.method === "resetCpaQuota").length, 1);
    assert.doesNotMatch(text(mounted.root), /42/, "the receipt clears the targeted quota in place");
    assert.match(text(mounted.root), /7/);
    assert.match(text(mounted.root), /acct-quota/);
    assert.match(text(mounted.root), /acct-kept/);
    followup.reject(new Error("quota revalidation failed"));
    await settle();
    assert.equal(calls.filter((call) => call.method === "resetCpaQuota").length, 1);
    assert.equal(accountReads, 2);
    assert.doesNotMatch(text(mounted.root), /42/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.match(text(mounted.root), /acct-quota/);
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "quota revalidation failed",
      reads: () => accountReads,
      writes: () => calls.filter((call) => call.method === "resetCpaQuota").length,
      others: [{ read: () => keyReads, label: "client-key GET" }],
    });
    assert.doesNotMatch(text(mounted.root), /42/);
    assert.match(text(mounted.root), /7/);
    assert.match(text(mounted.root), /acct-quota/);
    assert.match(text(mounted.root), /acct-kept/);
  } finally {
    followup.reject(new Error("unsettled quota follow-up GET"));
    await resetting.then(() => undefined, () => undefined);
    mounted.app.unmount();
  }
});

test("deleting an account removes the row at ack before its follow-up GET", async () => {
  const calls: ApiCall[] = [];
  const followup = deferredRead<{ accounts: ReturnType<typeof accountRow>[] }>();
  let accountReads = 0;
  let keyReads = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      return { keys: [], processGeneration: 1, revision: 1 };
    },
    getCpaCatalog: async () => catalogBody([]),
    getCpaAccounts: tracked(calls, "getCpaAccounts", async () => {
      accountReads += 1;
      if (accountReads === 1) {
        return { accounts: [accountRow("acct-gone"), accountRow("acct-kept")] };
      }
      if (accountReads === 2) return followup.promise;
      if (accountReads === 3) return { accounts: [accountRow("acct-kept")] };
      throw new Error("account delete retry must issue only one GET");
    }),
    deleteCpaAccount: tracked(calls, "deleteCpaAccount", async () => ({ processGeneration: 1, revision: 2 })),
  });
  await finishInitialLoad();
  assert.match(text(mounted.root), /acct-gone/);
  press(mounted.root, "删除");
  try {
    await waitForCount(() => accountReads, 2, "account delete follow-up GET");
    await waitForCount(() => calls.filter((call) => call.method === "deleteCpaAccount").length, 1, "account delete write");
    const deleteButtons = buttonsByLabel(mounted.root, "删除");
    assert.equal(deleteButtons.some((node) => node.props.loading === true), false, "account action flags release at ack");
    assert.equal(calls.filter((call) => call.method === "deleteCpaAccount").length, 1);
    assert.doesNotMatch(text(mounted.root), /acct-gone/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.equal(accountRows(mounted.root).length, 1);
    followup.reject(new Error("account delete revalidation failed"));
    await settle();
    assert.equal(calls.filter((call) => call.method === "deleteCpaAccount").length, 1);
    assert.equal(accountReads, 2);
    assert.doesNotMatch(text(mounted.root), /acct-gone/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.equal(accountRows(mounted.root).length, 1);
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "account delete revalidation failed",
      reads: () => accountReads,
      writes: () => calls.filter((call) => call.method === "deleteCpaAccount").length,
      others: [{ read: () => keyReads, label: "client-key GET" }],
    });
    assert.doesNotMatch(text(mounted.root), /acct-gone/);
    assert.match(text(mounted.root), /acct-kept/);
    assert.equal(accountRows(mounted.root).length, 1);
  } finally {
    followup.reject(new Error("unsettled account-delete follow-up GET"));
    await settle();
    mounted.app.unmount();
  }
});

test("dropping the session before a deferred client-key GET commits no secret and starts no new GET", async () => {
  const followup = deferredRead<{ keys: ReturnType<typeof runtimeKey>[]; processGeneration: number; revision: number }>();
  let keyReads = 0;
  let creates = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: async () => {
      keyReads += 1;
      if (keyReads === 1) return { keys: [runtimeKey("hint-kept")], processGeneration: 1, revision: 1 };
      if (keyReads === 2) return followup.promise;
      throw new Error("session drop must not start a new client-key GET");
    },
    createCpaRuntimeKey: async () => {
      creates += 1;
      return {
        ...runtimeKey("hint-late"),
        processGeneration: 1,
        revision: 2,
        secret: "secret-late",
      };
    },
  });
  const creating = press(mounted.root, "添加客户端 Key");
  try {
    await waitForCount(() => keyReads, 2, "create follow-up GET before drop");
    const readsAtDrop = keyReads;
    cpaSession().drop();
    await settle();
    followup.resolve({ keys: [runtimeKey("hint-poison")], processGeneration: 1, revision: 9 });
    await creating.then(() => undefined, () => undefined);
    await settle();
    assert.equal(creates, 1);
    assert.equal(keyReads, readsAtDrop, "the deferred callback must not start a new GET");
    assert.equal(revealedSecret(mounted.root), "");
    assert.doesNotMatch(text(mounted.root), /secret-late|hint-poison|hint-late|hint-kept|acct-live/);
  } finally {
    followup.reject(new Error("unsettled drop follow-up GET"));
    await creating.then(() => undefined, () => undefined);
    mounted.app.unmount();
  }
});

test("dropping the session before a deferred CLI import GET commits no notice and starts no new GET", async () => {
  const followup = deferredRead<{ accounts: ReturnType<typeof accountRow>[] }>();
  let accountReads = 0;
  let imports = 0;
  const mounted = await mount({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    getCpaCatalog: async () => catalogBody([]),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCliImports: async () => ({
      sources: [{ available: true, provider: "codex", reason: null, source: "synthetic-cli", supported: true }],
    }),
    getCpaAccounts: async () => {
      accountReads += 1;
      if (accountReads === 1) return { accounts: [accountRow("acct-live")] };
      if (accountReads === 2) return followup.promise;
      throw new Error("session drop must not start a new account GET");
    },
    importCpaCliAccount: async () => {
      imports += 1;
      return {
        name: "ocg-cli-codex-late.json",
        outcome: "imported" as const,
        processGeneration: 1,
        provider: "codex" as const,
        revision: 2,
      };
    },
  });
  const importing = press(mounted.root, "导入 Codex");
  try {
    await waitForCount(() => accountReads, 2, "import follow-up GET before drop");
    const readsAtDrop = accountReads;
    cpaSession().drop();
    await settle();
    followup.resolve({ accounts: [accountRow("ocg-cli-codex-poison")] });
    await importing.then(() => undefined, () => undefined);
    await settle();
    assert.equal(imports, 1);
    assert.equal(accountReads, readsAtDrop, "the deferred callback must not start a new GET");
    assert.equal(
      alerts(mounted.root).filter((node) => node.props.type === "success").length,
      0,
      "a dropped session must not keep or revive an import notice",
    );
    assert.doesNotMatch(text(mounted.root), /ocg-cli-codex-late|ocg-cli-codex-poison|acct-live/);
  } finally {
    followup.reject(new Error("unsettled import follow-up GET"));
    await importing.then(() => undefined, () => undefined);
    mounted.app.unmount();
  }
});

test("disconnect projects cleared CPA state at ack while the follow-up read stays deferred", async () => {
  const followup = deferredRead<ReturnType<typeof integration>>();
  let integrationReads = 0;
  let deletes = 0;
  const mounted = await mount({
    getCpaIntegration: async () => {
      integrationReads += 1;
      if (integrationReads === 1) {
        return externalIntegration({
          configured: true,
          inferenceKeyConfigured: true,
          managementKeyConfigured: true,
          modelCount: 1,
          baseUrlReadOnly: false,
        });
      }
      return followup.promise;
    },
    getCpaRuntime: async () => externalRuntime(),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-old")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-old", enabled: true }]),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCliImports: async () => ({
      sources: [{ available: true, provider: "codex", reason: null, source: "codex-cli", supported: true }],
    }),
    importCpaCliAccount: async () => ({
      name: "ocg-cli-codex-a1b2c3.json",
      outcome: "imported" as const,
      processGeneration: 1,
      provider: "codex" as const,
      revision: 2,
    }),
    deleteCpaIntegration: async () => {
      deletes += 1;
      return { processGeneration: 1, revision: 4 };
    },
  });
  try {
    await finishInitialLoad();
    await press(mounted.root, "导入 Codex");
    await settle();
    assert.equal(alerts(mounted.root).some((node) => node.props.type === "success"), true);
    assert.equal(button(mounted.root, "导入 Codex").props.disabled, true);
    setPasswordField(mounted.root, "Inference Key", "secret-inference");
    setPasswordField(mounted.root, "Management Key", "secret-management");
    await settle();
    const readsAtDelete = integrationReads;
    press(mounted.root, "断开并清除");
    await waitForCount(() => deletes, 1, "disconnect write");
    await waitForCount(() => integrationReads, readsAtDelete + 1, "deferred integration GET");
    const projected = integrationRecord();
    assert.equal(deletes, 1);
    assert.equal(disconnectBusy(mounted.root), false, "the disconnect operation releases at ack");
    assert.equal(projected?.configured, false);
    assert.equal(projected?.enabled, false);
    assert.equal(projected?.accountId, null);
    assert.equal(projected?.inferenceKeyConfigured, false);
    assert.equal(projected?.managementKeyConfigured, false);
    assert.equal(projected?.modelCount, 0);
    assert.equal(projected?.revision, 4);
    assert.deepEqual(accountNames(), []);
    assert.equal(activeCpa().catalogModels.length, 0);
    assert.equal(alerts(mounted.root).some((node) => node.props.type === "success"), false);
    assert.doesNotMatch(text(mounted.root), /acct-old|model-old|secret-inference|secret-management/);
    await revealClearedExternalDraft(mounted.root);
    assert.equal(passwordValue(mounted.root, "Inference Key"), "");
    assert.equal(passwordValue(mounted.root, "Management Key"), "");
  } finally {
    followup.reject(new Error("unsettled disconnect follow-up GET"));
    await settle();
    mounted.app.unmount();
  }
});

test("a failed disconnect read warns once and its retry is one GET", async () => {
  const followup = deferredRead<ReturnType<typeof integration>>();
  const calls: ApiCall[] = [];
  let integrationReads = 0;
  const canonical = clearedIntegration();
  const mounted = await mount({
    getCpaIntegration: tracked(calls, "getCpaIntegration", async () => {
      integrationReads += 1;
      if (integrationReads === 1) {
        return externalIntegration({
          configured: true,
          modelCount: 1,
          inferenceKeyConfigured: true,
          managementKeyConfigured: true,
          baseUrlReadOnly: false,
        });
      }
      if (integrationReads === 2) return followup.promise;
      if (integrationReads === 3) return canonical;
      throw new Error("disconnect retry must issue only one integration GET");
    }),
    getCpaRuntime: async () => externalRuntime(),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-old")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-old", enabled: true }]),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    deleteCpaIntegration: tracked(calls, "deleteCpaIntegration", async () => ({ processGeneration: 1, revision: 2 })),
  });
  try {
    await finishInitialLoad();
    const deletes = () => calls.filter((call) => call.method === "deleteCpaIntegration").length;
    press(mounted.root, "断开并清除");
    await waitForCount(deletes, 1, "disconnect write");
    await waitForCount(() => integrationReads, 2, "deferred integration GET");
    followup.reject(new Error("disconnect-read-failed"));
    await settle();
    assert.equal(deletes(), 1, "a failed read must not replay the delete");
    assert.equal(errorMessages().length, 0, "a failed follow-up read stays off the mutation error toast");
    await assertBackgroundReadRetry({
      root: mounted.root,
      marker: "disconnect-read-failed",
      reads: () => integrationReads,
      writes: deletes,
    });
    assert.equal(integrationReads, 3);
    const stored = integrationRecord();
    assert.equal(stored?.configured, false);
    assert.equal(stored?.revision, canonical.revision);
  } finally {
    followup.reject(new Error("unsettled disconnect read"));
    await settle();
    mounted.app.unmount();
  }
});

test("a successful disconnect read adopts the cleared integration and keeps secrets empty", async () => {
  let integrationReads = 0;
  const canonical = clearedIntegration({ baseUrl: "http://127.0.0.1:8400" });
  const mounted = await mount({
    getCpaIntegration: async () => {
      integrationReads += 1;
      if (integrationReads === 1) {
        return externalIntegration({
          configured: true,
          baseUrl: "http://127.0.0.1:8317",
          baseUrlReadOnly: false,
          inferenceKeyConfigured: true,
          managementKeyConfigured: true,
          modelCount: 1,
        });
      }
      return canonical;
    },
    getCpaRuntime: async () => externalRuntime(),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-old")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-old", enabled: true }]),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCliImports: async () => ({
      sources: [{ available: true, provider: "codex", reason: null, source: "codex-cli", supported: true }],
    }),
    deleteCpaIntegration: async () => ({ processGeneration: 1, revision: 2 }),
  });
  try {
    await finishInitialLoad();
    setPasswordField(mounted.root, "Inference Key", "secret-inference");
    setPasswordField(mounted.root, "Management Key", "secret-management");
    const baseUrl = passwordField(mounted.root, "基础地址");
    const updateBase = baseUrl.props["onUpdate:value"];
    assert.equal(typeof updateBase, "function");
    if (typeof updateBase === "function") updateBase("http://secret-base.example");
    await settle();
    await press(mounted.root, "断开并清除");
    await settle();
    assert.equal(integrationReads, 2);
    const stored = integrationRecord();
    assert.ok(stored);
    for (const [key, expected] of Object.entries(canonical)) {
      assert.deepEqual(stored[key], expected, key);
    }
    assert.deepEqual(accountNames(), []);
    assert.equal(activeCpa().catalogModels.length, 0);
    await revealClearedExternalDraft(mounted.root);
    assert.equal(passwordValue(mounted.root, "Inference Key"), "");
    assert.equal(passwordValue(mounted.root, "Management Key"), "");
    assert.equal(passwordValue(mounted.root, "基础地址"), canonical.baseUrl);
    assert.equal(text(mounted.root).includes("secret-inference"), false);
    assert.equal(text(mounted.root).includes("secret-management"), false);
    assert.equal(text(mounted.root).includes("secret-base"), false);
    assert.equal(disconnectBusy(mounted.root), false);
  } finally {
    mounted.app.unmount();
  }
});

test("a disconnect acknowledgement after logout starts no read and leaves the replacement intact", async () => {
  const deleteGate = deferred<{ processGeneration: number; revision: number }>();
  void deleteGate.promise.catch(() => undefined);
  let phase: "old" | "next" | "closed" = "old";
  let integrationReads = 0;
  let accountReads = 0;
  let catalogReads = 0;
  let cliReads = 0;
  let deletes = 0;
  const api: CpaApi = {
    getCpaIntegration: async () => {
      integrationReads += 1;
      if (phase === "closed") {
        return externalIntegration({
          accountId: "poison",
          baseUrl: "http://poison.example",
          configured: false,
          revision: 99,
        });
      }
      if (phase === "next") {
        return externalIntegration({
          accountId: "replacement",
          baseUrl: "http://127.0.0.1:9000",
          configured: true,
          inferenceKeyConfigured: true,
          managementKeyConfigured: true,
          modelCount: 1,
          revision: 8,
        });
      }
      return externalIntegration({ configured: true, modelCount: 1, revision: 1 });
    },
    getCpaRuntime: async () => externalRuntime({ revision: phase === "next" ? 8 : 1 }),
    getCpaAccounts: async () => {
      accountReads += 1;
      if (phase === "closed") return { accounts: [accountRow("acct-poison")] };
      return { accounts: [accountRow(phase === "next" ? "acct-next" : "acct-old")] };
    },
    getCpaCatalog: async () => {
      catalogReads += 1;
      if (phase === "closed") return catalogBody([{ id: "model-poison", enabled: true }]);
      return catalogBody([{ id: phase === "next" ? "model-next" : "model-old", enabled: true }]);
    },
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    getCpaCliImports: async () => {
      cliReads += 1;
      return { sources: [] };
    },
    deleteCpaIntegration: async () => {
      deletes += 1;
      return deleteGate.promise;
    },
  };
  const first = await mount(api);
  let primary: App | null = first.app;
  try {
    await finishInitialLoad();
    setPasswordField(first.root, "Inference Key", "secret-old-inference");
    setPasswordField(first.root, "Management Key", "secret-old-management");
    await settle();
    const disconnect = press(first.root, "断开并清除");
    void disconnect.then(() => undefined, () => undefined);
    await waitForCount(() => deletes, 1, "disconnect write");
    const readsAtDrop = { integrationReads, accountReads, catalogReads, cliReads };
    const errorsAtDrop = errorMessages().length;
    const successAtDrop = messagesOf("success").length;
    cpaSession().drop();
    await settle();
    assert.equal(integrationReads, readsAtDrop.integrationReads);
    assert.equal(accountReads, readsAtDrop.accountReads);
    assert.equal(catalogReads, readsAtDrop.catalogReads);
    assert.equal(cliReads, readsAtDrop.cliReads);
    assert.equal(integrationRecord(), null);
    primary.unmount();
    primary = null;
    phase = "next";
    const second = await mount(api);
    try {
      await finishInitialLoad();
      assert.match(text(second.root), /acct-next/);
      assert.match(text(second.root), /model-next/);
      const readsAtAck = { integrationReads, accountReads, catalogReads, cliReads };
      phase = "closed";
      deleteGate.resolve({ processGeneration: 1, revision: 4 });
      await settle();
      assert.equal(deletes, 1);
      assert.equal(integrationReads, readsAtAck.integrationReads, "a late ack must not start an integration GET");
      assert.equal(accountReads, readsAtAck.accountReads, "a late ack must not start an account GET");
      assert.equal(catalogReads, readsAtAck.catalogReads, "a late ack must not start a catalog GET");
      assert.equal(cliReads, readsAtAck.cliReads, "a late ack must not start a CLI discovery GET");
      assert.equal(errorMessages().length, errorsAtDrop);
      assert.equal(messagesOf("success").length, successAtDrop);
      assert.equal(integrationRecord()?.revision, 8);
      assert.equal(integrationRecord()?.baseUrl, "http://127.0.0.1:9000");
      assert.equal(integrationRecord()?.accountId, "replacement");
      assert.deepEqual(accountNames(), ["acct-next"]);
      assert.equal(passwordValue(second.root, "Inference Key"), "");
      assert.equal(passwordValue(second.root, "Management Key"), "");
      assert.equal(disconnectBusy(second.root), false);
      assert.doesNotMatch(text(second.root), /poison|acct-old|model-old|secret-old/);
    } finally {
      second.app.unmount();
    }
  } finally {
    deleteGate.reject(new Error("unsettled disconnect ack"));
    primary?.unmount();
  }
});

test("a stale disconnect finally cannot release a newer disconnect or OAuth flow", async () => {
  const deleteGate = deferred<{ processGeneration: number; revision: number }>();
  void deleteGate.promise.catch(() => undefined);
  const followup = deferredRead<ReturnType<typeof integration>>();
  const oauthGate = deferred<{
    expiresIn: number | null;
    flow: string;
    processGeneration: number;
    provider: "codex";
    revision: number;
    state: string;
    url: string;
    userCode: string | null;
  }>();
  void oauthGate.promise.catch(() => undefined);
  let integrationReads = 0;
  let deletes = 0;
  const mounted = await mount({
    getCpaIntegration: async () => {
      integrationReads += 1;
      if (integrationReads === 1) {
        return externalIntegration({ configured: true, modelCount: 1, baseUrlReadOnly: false });
      }
      return followup.promise;
    },
    getCpaRuntime: async () => externalRuntime(),
    getCpaAccounts: async () => ({ accounts: [accountRow("acct-live")] }),
    getCpaCatalog: async () => catalogBody([{ id: "model-live", enabled: true }]),
    getCpaRuntimeKeys: async () => ({ keys: [] }),
    deleteCpaIntegration: async () => {
      deletes += 1;
      return deleteGate.promise;
    },
    startCpaOAuth: async () => oauthGate.promise,
  });
  try {
    await finishInitialLoad();
    press(mounted.root, "断开并清除");
    await waitForCount(() => deletes, 1, "first disconnect");
    assert.equal(disconnectBusy(mounted.root), true);
    press(mounted.root, "断开并清除");
    await settle();
    assert.equal(deletes, 1, "a second click must not start another delete while the first is in flight");
    assert.equal(disconnectBusy(mounted.root), true);
    const oauth = press(mounted.root, "Codex 浏览器登录");
    void oauth.then(() => undefined, () => undefined);
    await settle();
    assert.equal(button(mounted.root, "Codex 浏览器登录").props.loading, true);
    const readsAtAck = integrationReads;
    deleteGate.resolve({ processGeneration: 1, revision: 4 });
    await settle();
    await waitForCount(() => integrationReads, readsAtAck + 1, "deferred integration GET");
    assert.equal(deletes, 1, "the ack must not replay disconnect");
    assert.equal(disconnectBusy(mounted.root), false, "the finished disconnect releases only its own ticket");
    assert.equal(
      button(mounted.root, "Codex 浏览器登录").props.loading,
      true,
      "a stale finally must not release the newer OAuth flow",
    );
    assert.deepEqual(accountNames(), []);
    assert.equal(activeCpa().catalogModels.length, 0);
    followup.resolve(clearedIntegration());
    await settle();
    assert.equal(deletes, 1);
    assert.equal(button(mounted.root, "Codex 浏览器登录").props.loading, true);
    assert.deepEqual(accountNames(), []);
    assert.doesNotMatch(text(mounted.root), /acct-live|model-live/);
  } finally {
    deleteGate.reject(new Error("unsettled disconnect"));
    followup.reject(new Error("unsettled disconnect read"));
    oauthGate.reject(new Error("unsettled oauth"));
    await settle();
    mounted.app.unmount();
  }
});
