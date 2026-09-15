import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { ssrContextKey, type App, type Component } from "vue";
import {
  button,
  createVueHostRenderer,
  deferred,
  fireTimers,
  installTestWindow,
  settle,
  text,
  type HostNode,
  type TestWindow,
} from "../test-helpers/vue-host-runtime.ts";

type CpaApi = Record<string, (...args: unknown[]) => Promise<unknown>>;

let buildDir: string;
let Cpa: Component;
let api: CpaApi;

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
    "../i18n/index.ts": "i18n",
    "../utils/errors.ts": "errors",
    "../utils/format.ts": "clipboard",
  };
  return {
    name: "cpa-component-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      if (!importer?.replaceAll("\\", "/").includes("/src/views/Cpa.vue")) return null;
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/views/Cpa.vue?vue&type=style")) return "";
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
    assetSha256: null, baseUrl: "http://127.0.0.1:8317", currentOperation: null, currentVersion: "1.0.0",
    error: null, installed: true, latestVersion: null, owned: true, phase: "idle", port: 8317,
    previousVersion: null, processGeneration: 1, revision: 1, running: false, supported: true,
    unavailableReason: null, updateAvailable: false, ...overrides,
  };
}

type RecordedMessage = { type: string; args: unknown[] };

function recordedMessages(): RecordedMessage[] {
  return (globalThis as unknown as { __cpaMessages?: RecordedMessage[] }).__cpaMessages ?? [];
}

async function mount(componentApi: CpaApi): Promise<{ app: App; root: HostNode; window: TestWindow }> {
  const testWindow = installTestWindow();
  (globalThis as unknown as { __cpaMessages?: RecordedMessage[] }).__cpaMessages = [];
  api = {
    getCpaCatalog: async () => ({ models: [], sourceUrl: null, refreshedAt: null, revision: { revision: 1, processGeneration: 1, pricingRevision: "p" } }),
    // Default to a valid empty discovery so the CLI import section stays quiet;
    // tests that care about discovery override this explicitly.
    getCpaCliImports: async () => ({ sources: [] }),
    ...componentApi,
  };
  (globalThis as { __cpaComponentApi?: CpaApi }).__cpaComponentApi = api;
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Cpa);
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
      rollupOptions: { external: ["vue"] },
    },
  });
  Cpa = (await import(pathToFileURL(path.join(buildDir, "cpa.mjs")).href)).default;
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
  const start = button(mounted.root, "启动");
  await (start.props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(calls.integration, 1);
  assert.equal(calls.accounts, 1);
  assert.equal(calls.keys, 1);
  assert.match(text(mounted.root), /运行中/);
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
  assert.match(text(mounted.root), /加载 CPA 运行时失败: runtime down/);
  assert.equal(button(mounted.root, "托管安装").props.disabled, true);
  assert.doesNotMatch(text(mounted.root), /当前环境不支持托管 CPA 运行时/);
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
  await fireTimers(mounted.window);
  assert.equal(runtimeReads, 2);
  await fireTimers(mounted.window);
  assert.equal(runtimeReads, 2);
  pending.resolve(runtime({ phase: "downloading", currentOperation: "install" }));
  await settle();
  mounted.app.unmount();
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
  await fireTimers(mounted.window);
  assert.equal(runtimeReads, 2);
  await (button(mounted.root, "刷新").props.onClick as () => Promise<void>)();
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
  await fireTimers(mounted.window);
  await settle();
  assert.match(text(mounted.root), /CPA 运行时状态刷新失败: poll failed/);
  assert.match(text(mounted.root), /下载中/);
  const retries = mounted.root.children.flatMap(function walk(node: HostNode): HostNode[] {
    return [node, ...node.children.flatMap(walk)];
  }).filter((node) => node.type === "button" && text(node).trim() === "重试");
  assert.ok(retries.length >= 1);
  await (retries[retries.length - 1].props.onClick as () => Promise<void>)();
  await settle();
  assert.doesNotMatch(text(mounted.root), /CPA 运行时状态刷新失败/);
  assert.doesNotMatch(text(mounted.root), /下载中/);
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
  assert.match(text(mounted.root), /已选 1 \/ 2/);
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
  assert.match(text(mounted.root), /请先在概览中配置并启动 CPA/);
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
  assert.match(text(mounted.root), /请先在概览中配置并启动 CPA/);
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
