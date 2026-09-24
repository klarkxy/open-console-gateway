import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
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
  walkHostNodes,
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
      export const useMessage = () => ({ error() {}, success() {}, warning() {} });
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
      export const useClipboard = () => ({
        copiedTarget,
        copy: async (target, value) => { globalThis.__cpaCopied = { target, value }; copiedTarget.value = target; },
        cleanup: () => {},
      });
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

function importButton(root: HostNode, provider: string): HostNode {
  return button(root, `导入 ${provider}`);
}

// Structural queries for state alerts: the harness forwards attrs (class, type)
// onto host node props, and the page marks OAuth/import notices with the
// cpa-oauth-status class, so presence/absence and notice kind are assertable
// without matching rendered copy.
function hasClass(node: HostNode, className: string): boolean {
  return typeof node.props.class === "string" && node.props.class.split(/\s+/).includes(className);
}

function elementsByClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter((node) => hasClass(node, className));
}

function oauthStatusAlerts(root: HostNode, type: "info" | "warning" | "success"): HostNode[] {
  return elementsByClass(root, "cpa-oauth-status").filter((node) => node.props.type === type);
}

// Inline failure alerts (discovery, load errors) carry a title but no dedicated
// class; titled divs of the given type identify them.
function titledAlerts(root: HostNode, type: string): HostNode[] {
  return walkHostNodes(root).filter(
    (node) => node.type === "div" && node.props.title !== undefined && node.props.type === type,
  );
}

function hasButton(root: HostNode, label: string): boolean {
  return walkHostNodes(root).some(
    (node) => node.type === "button" && text(node).trim() === label,
  );
}

function oauthComponentApi(overrides: CpaApi): CpaApi {
  return {
    getCpaIntegration: async () => integration(),
    getCpaRuntime: async () => runtime(),
    getCpaAccounts: async () => ({ accounts: [] }),
    getCpaRuntimeKeys: async () => ({ keys: [], processGeneration: 1, revision: 1 }),
    getCpaCatalog: async () => ({ models: [], sourceUrl: null, refreshedAt: null, revision: { revision: 1, processGeneration: 1, pricingRevision: "p" } }),
    getCpaCliImports: async () => ({ sources: [] }),
    importCpaCliAccount: async () => { throw new Error("importCpaCliAccount not stubbed"); },
    cancelCpaOAuth: async () => ({ revision: 1, processGeneration: 1 }),
    ...overrides,
  };
}

async function mount(componentApi: CpaApi): Promise<{ app: App; root: HostNode; window: TestWindow }> {
  const testWindow = installTestWindow();
  api = componentApi;
  (globalThis as { __cpaComponentApi?: CpaApi }).__cpaComponentApi = api;
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Cpa);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  return { app, root, window: testWindow };
}

before(async () => {
  buildDir = await mkdtemp(path.join(process.cwd(), ".ocg-cpa-oauth-"));
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

test("OAuth polling is single-flight and schedules the next poll only after completion", async () => {
  const pending = deferred<unknown>();
  let reads = 0;
  const mounted = await mount(oauthComponentApi({
    startCpaOAuth: async () => ({ state: "flow-1", url: null, flow: "browser", revision: 1, processGeneration: 1 }),
    getCpaOAuthStatus: async () => {
      reads += 1;
      return reads === 1 ? pending.promise : { state: "flow-1", status: "pending" };
    },
  }));
  try {
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(mounted.window.__timers.size, 1, "the first poll is scheduled");
    await fireTimers(mounted.window);
    assert.equal(reads, 1, "the first status request is in flight");
    await fireTimers(mounted.window);
    assert.equal(reads, 1, "no overlapping request while the previous one is pending");
    pending.resolve({ state: "flow-1", status: "pending" });
    await settle();
    assert.equal(mounted.window.__timers.size, 1, "a non-terminal response schedules the next poll");
    await fireTimers(mounted.window);
    assert.equal(reads, 2, "the next poll runs only after the previous response was applied");
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 1, "the in-progress OAuth panel stays visible");
  } finally { mounted.app.unmount(); }
});

test("a stale terminal response cannot clear a newer OAuth flow", async () => {
  const stale = deferred<unknown>();
  let starts = 0;
  let accountReads = 0;
  const statusStates: string[] = [];
  const mounted = await mount(oauthComponentApi({
    startCpaOAuth: async () => ({ state: `flow-${++starts}`, url: null, flow: "browser", revision: 1, processGeneration: 1 }),
    getCpaOAuthStatus: async (state: unknown) => {
      statusStates.push(String(state));
      return state === "flow-1" ? stale.promise : { state, status: "pending" };
    },
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  try {
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    await fireTimers(mounted.window);
    assert.deepEqual(statusStates, ["flow-1"], "the first flow has a status request in flight");
    await (button(mounted.root, "取消当前授权").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 0, "the cancelled flow panel is gone");
    const accountReadsAfterCancel = accountReads;
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(starts, 2, "a new flow started");
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 1, "the new flow panel is shown");
    stale.resolve({ state: "flow-1", status: "ok" });
    await settle();
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 1, "the stale terminal response leaves the new flow panel alone");
    assert.equal(accountReads, accountReadsAfterCancel, "the stale success does not refresh accounts");
    await fireTimers(mounted.window);
    assert.deepEqual(statusStates, ["flow-1", "flow-2"], "the new flow keeps polling with its own state");
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 1);
  } finally { mounted.app.unmount(); }
});

test("unmounting with a poll in flight cancels the flow and ignores the late response", async () => {
  const stale = deferred<unknown>();
  let cancels = 0;
  let accountReads = 0;
  const mounted = await mount(oauthComponentApi({
    startCpaOAuth: async () => ({ state: "flow-1", url: null, flow: "browser", revision: 1, processGeneration: 1 }),
    getCpaOAuthStatus: async () => stale.promise,
    cancelCpaOAuth: async () => { cancels += 1; return { revision: 1, processGeneration: 1 }; },
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
  await settle();
  await fireTimers(mounted.window);
  const accountReadsBeforeUnmount = accountReads;
  mounted.app.unmount();
  await settle();
  assert.equal(cancels, 1, "leaving the page cancels the active flow");
  stale.resolve({ state: "flow-1", status: "ok" });
  await settle();
  assert.equal(accountReads, accountReadsBeforeUnmount, "the late terminal response is ignored");
});

test("a slow cancel invalidates an in-flight poll synchronously", async () => {
  const stale = deferred<unknown>();
  const cancelRequest = deferred<unknown>();
  let accountReads = 0;
  const mounted = await mount(oauthComponentApi({
    startCpaOAuth: async () => ({ state: "flow-1", url: null, flow: "browser", revision: 1, processGeneration: 1 }),
    getCpaOAuthStatus: async () => stale.promise,
    cancelCpaOAuth: async () => { await cancelRequest.promise; return { revision: 1, processGeneration: 1 }; },
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  try {
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    await fireTimers(mounted.window);
    const accountReadsBeforeCancel = accountReads;
    const cancelClick = (button(mounted.root, "取消当前授权").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 1, "the flow panel stays visible while the cancel is pending");
    stale.resolve({ state: "flow-1", status: "ok" });
    await settle();
    assert.equal(accountReads, accountReadsBeforeCancel, "the stale terminal response is ignored during the cancel");
    assert.equal(mounted.window.__timers.size, 0, "the stale response does not re-arm polling");
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 1, "the pending cancel still owns the flow state");
    cancelRequest.resolve(undefined);
    await cancelClick;
    await settle();
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 0, "the flow panel is gone once the cancel settles");
    assert.equal(accountReads, accountReadsBeforeCancel, "a cancelled flow never refreshes accounts");
  } finally { mounted.app.unmount(); }
});

test("unmounting while OAuth start is pending never adopts the late session", async () => {
  const startRequest = deferred<unknown>();
  let cancels = 0;
  const mounted = await mount(oauthComponentApi({
    startCpaOAuth: async () => {
      await startRequest.promise;
      return { state: "flow-1", url: "https://example.invalid", flow: "browser", revision: 1, processGeneration: 1 };
    },
    cancelCpaOAuth: async () => { cancels += 1; return { revision: 1, processGeneration: 1 }; },
  }));
  void (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
  await settle();
  mounted.app.unmount();
  await settle();
  assert.equal(cancels, 0, "no flow was adopted yet, so there is nothing to cancel");
  startRequest.resolve(undefined);
  await settle();
  assert.equal(cancels, 1, "the late-started server session is released best-effort");
  assert.deepEqual(mounted.window.__opened, [], "no popup opens after unmount");
  assert.equal(mounted.window.__timers.size, 0, "no polling is scheduled after unmount");
});

function managedRunningApi(overrides: CpaApi): CpaApi {
  return oauthComponentApi({
    getCpaIntegration: async () => integration({ runtimeRunning: true }),
    getCpaRuntime: async () => runtime({ running: true }),
    ...overrides,
  });
}

function copiedCode(): { target: string; value: string } | undefined {
  return (globalThis as { __cpaCopied?: { target: string; value: string } }).__cpaCopied;
}

test("Codex browser and device buttons send the matching startCpaOAuth payload", async () => {
  const starts: unknown[] = [];
  const mounted = await mount(managedRunningApi({
    startCpaOAuth: async (input: unknown) => {
      starts.push(input);
      return { state: `flow-${starts.length}`, provider: "codex", url: null, flow: starts.length === 1 ? "browser" : "device", userCode: null, expiresIn: null, revision: 1, processGeneration: 1 };
    },
    getCpaOAuthStatus: async (state: unknown) => ({ state, status: "pending" }),
  }));
  try {
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(starts[0], { provider: "codex", method: "browser" }, "the browser button sends an explicit browser method");
    await (button(mounted.root, "取消当前授权").props.onClick as () => Promise<void>)();
    await settle();
    await (button(mounted.root, "Codex 设备码登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(starts[1], { provider: "codex", method: "device" }, "the device button sends the device method");
    assert.equal(starts.length, 2, "each click starts exactly one flow");
  } finally { mounted.app.unmount(); }
});

test("device sign-in shows the short code with copy, the auth URL, and no auto-open", async () => {
  const mounted = await mount(managedRunningApi({
    startCpaOAuth: async () => ({
      state: "flow-1", provider: "codex", flow: "device", userCode: "ABCD-1234",
      expiresIn: 900, url: "https://auth.openai.com/codex/device", revision: 1, processGeneration: 1,
      token: "secret-token-value",
    }),
    getCpaOAuthStatus: async (state: unknown) => ({ state, status: "pending" }),
  }));
  try {
    await (button(mounted.root, "Codex 设备码登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(mounted.window.__opened, [], "the device flow never auto-opens a popup");
    const page = text(mounted.root);
    assert.match(page, /ABCD-1234/, "the short code is prominent");
    const flowPanels = oauthStatusAlerts(mounted.root, "info");
    assert.equal(flowPanels.length, 1, "the device flow panel is shown");
    assert.match(text(flowPanels[0]), /15/, "the expiry derived from expiresIn is shown in minutes");
    assert.match(text(flowPanels[0]), /ChatGPT/, "the ChatGPT device-login prerequisite is explained");
    const openPage = button(mounted.root, "打开授权页面");
    assert.equal(openPage.props.href, "https://auth.openai.com/codex/device", "the auth URL stays one click away");
    assert.ok(button(mounted.root, "Codex 浏览器登录").props.disabled, "an active flow keeps the other start single-flight");
    const codeRow = elementsByClass(mounted.root, "cpa-device-code-row")[0];
    const copyCodeButton = codeRow.children.find((node) => node.type === "button");
    if (!copyCodeButton) assert.fail("the short code has a copy action");
    const copyLabelBefore = text(copyCodeButton);
    await (copyCodeButton.props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(copiedCode(), { target: "cpa-device-code", value: "ABCD-1234" }, "the copy action copies exactly the code");
    assert.notEqual(text(copyCodeButton), copyLabelBefore, "the copy is confirmed inline");
    assert.doesNotMatch(text(mounted.root), /secret-token-value/, "a token field in the response is never rendered");
  } finally { mounted.app.unmount(); }
});

test("a Codex browser failure offers the device alternative without switching automatically", async () => {
  const starts: unknown[] = [];
  const mounted = await mount(managedRunningApi({
    startCpaOAuth: async (input: unknown) => {
      starts.push(input);
      return { state: "flow-1", provider: "codex", url: null, flow: "browser", userCode: null, expiresIn: null, revision: 1, processGeneration: 1 };
    },
    getCpaOAuthStatus: async (state: unknown) => ({ state, status: "failed", error: "callback server unavailable" }),
  }));
  try {
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    await fireTimers(mounted.window);
    const failureAlerts = oauthStatusAlerts(mounted.root, "warning");
    assert.equal(failureAlerts.length, 1, "the failure is surfaced");
    assert.match(text(failureAlerts[0]), /callback server unavailable/, "the backend error detail is preserved");
    assert.ok(
      walkHostNodes(failureAlerts[0]).some((node) => node.type === "button"),
      "the failure alert offers the device alternative",
    );
    assert.equal(starts.length, 1, "no automatic switch to the device flow");
    await (button(mounted.root, "改用设备码登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(starts[1], { provider: "codex", method: "device" }, "the suggestion starts the device flow on demand");
  } finally { mounted.app.unmount(); }
});

test("an external CPA keeps device sign-in disabled with an explanation", async () => {
  let starts = 0;
  const mounted = await mount(oauthComponentApi({
    getCpaIntegration: async () => integration({ runtimeOwned: false, runtimeRunning: false }),
    getCpaRuntime: async () => runtime({ owned: false, running: false, supported: false }),
    startCpaOAuth: async () => { starts += 1; return { state: "flow-1", provider: "codex", url: null, flow: "browser", userCode: null, expiresIn: null, revision: 1, processGeneration: 1 }; },
  }));
  try {
    const oauthArea = elementsByClass(mounted.root, "oauth-providers")[0];
    const deviceExplanation = oauthArea.parent?.children.find(
      (node) => node.type === "p" && hasClass(node, "cpa-help"),
    );
    assert.ok(deviceExplanation, "the managed-runtime requirement is explained next to the sign-in buttons");
    const deviceButton = button(mounted.root, "Codex 设备码登录");
    assert.ok(deviceButton.props.disabled, "the device button is disabled for an external CPA");
    await (deviceButton.props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(starts, 0, "the guard never starts a device flow for an external CPA");
  } finally { mounted.app.unmount(); }
});

test("a Codex browser start failure offers the device alternative", async () => {
  const starts: unknown[] = [];
  let attempts = 0;
  const mounted = await mount(managedRunningApi({
    startCpaOAuth: async (input: unknown) => {
      starts.push(input);
      attempts += 1;
      if (attempts === 1) throw new Error("callback server listen failed");
      return { state: "flow-2", provider: "codex", flow: "device", userCode: "WXYZ-9876", expiresIn: 900, url: "https://auth.openai.com/codex/device", revision: 1, processGeneration: 1 };
    },
    getCpaOAuthStatus: async (state: unknown) => ({ state, status: "pending" }),
  }));
  try {
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    const failureAlerts = oauthStatusAlerts(mounted.root, "warning");
    assert.equal(failureAlerts.length, 1, "the start failure is surfaced");
    assert.match(text(failureAlerts[0]), /callback server listen failed/, "the backend error detail is preserved");
    assert.equal(starts.length, 1, "no automatic switch to the device flow");
    assert.equal(oauthStatusAlerts(mounted.root, "info").length, 0, "no flow is adopted from the failed start");
    await (button(mounted.root, "改用设备码登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(starts[1], { provider: "codex", method: "device" }, "the suggestion starts the device flow on demand");
    assert.match(text(mounted.root), /WXYZ-9876/, "the device flow proceeds with its code");
  } finally { mounted.app.unmount(); }
});

test("a non-Codex device flow keeps the generic instruction without ChatGPT copy", async () => {
  const mounted = await mount(oauthComponentApi({
    startCpaOAuth: async () => ({
      state: "flow-1", provider: "kimi", flow: "device", userCode: "KIMI-42",
      expiresIn: 600, url: "https://example.invalid/device", revision: 1, processGeneration: 1,
    }),
    getCpaOAuthStatus: async (state: unknown) => ({ state, status: "pending" }),
  }));
  try {
    await (button(mounted.root, "登录 Kimi").props.onClick as () => Promise<void>)();
    await settle();
    const page = text(mounted.root);
    assert.match(page, /KIMI-42/, "the short code is shown");
    const flowPanels = oauthStatusAlerts(mounted.root, "info");
    assert.equal(flowPanels.length, 1, "the device flow panel is shown");
    assert.ok(
      flowPanels[0].children.some((node) => node.type === "p"),
      "a generic device instruction is shown",
    );
    assert.doesNotMatch(page, /ChatGPT/, "no Codex-specific copy leaks into other providers");
    assert.match(text(flowPanels[0]), /10/, "the expiry derived from expiresIn is still shown");
    assert.equal(button(mounted.root, "打开授权页面").props.href, "https://example.invalid/device", "the auth URL is preserved");
  } finally { mounted.app.unmount(); }
});

test("CLI import lists dynamic availability with unsupported and missing reasons", async () => {
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
      { provider: "anthropic", source: "claude-cli", supported: true, available: false, reason: "未发现凭据文件" },
      { provider: "kimi", source: "kimi-cli", supported: false, available: false, reason: "导入支持仍在评估中" },
    ] }),
  }));
  try {
    await settle();
    const page = text(mounted.root);
    const importSections = elementsByClass(mounted.root, "cpa-cli-import");
    assert.equal(importSections.length, 1, "the CLI import section renders");
    assert.ok(
      importSections[0].children.some((node) => node.type === "p" && hasClass(node, "cpa-help")),
      "the one-time copy and shared-authorization trade-off is explained",
    );
    assert.equal(elementsByClass(mounted.root, "cpa-cli-import-tip").length, 1, "blocked sources collapse into one tip");
    assert.match(page, /未发现凭据文件/, "hover detail keeps the backend reason");
    assert.match(page, /导入支持仍在评估中/, "hover detail keeps the unsupported reason");
    assert.equal("secondary" in importButton(mounted.root, "Codex").props, true, "import uses the same secondary buttons as fresh login");
    assert.ok(!importButton(mounted.root, "Codex").props.disabled, "an available source can be imported");
    assert.ok(importButton(mounted.root, "Claude").props.disabled, "a missing source cannot be imported");
    assert.ok(importButton(mounted.root, "Kimi").props.disabled, "an unsupported source cannot be imported");
  } finally { mounted.app.unmount(); }
});

test("import sends only the provider payload and refreshes accounts on imported", async () => {
  const imports: unknown[] = [];
  let accountReads = 0;
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
    ] }),
    importCpaCliAccount: async (input: unknown) => {
      imports.push(input);
      return { provider: "codex", name: "ocg-cli-codex-a1b2c3.json", outcome: "imported", revision: 2, processGeneration: 1 };
    },
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  try {
    await settle();
    const accountReadsAfterLoad = accountReads;
    await (importButton(mounted.root, "Codex").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(imports[0], { provider: "codex" }, "the payload carries no secret, path, or source text");
    assert.equal(Object.keys(imports[0] as object).length, 1, "the payload is exactly the provider");
    assert.ok(accountReads > accountReadsAfterLoad, "a confirmed import refreshes the account list");
    const notices = oauthStatusAlerts(mounted.root, "success");
    assert.equal(notices.length, 1, "a success notice is shown");
    assert.match(text(notices[0]), /Codex/, "the notice names the provider label");
    assert.doesNotMatch(text(mounted.root), /ocg-cli-codex-a1b2c3\.json/, "the hashed implementation filename is never rendered");
    assert.ok(importButton(mounted.root, "Codex").props.disabled, "an imported source is greyed out");
  } finally { mounted.app.unmount(); }
});

test("alreadyImported refreshes accounts and explains no duplicate was created", async () => {
  let accountReads = 0;
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "anthropic", source: "claude-cli", supported: true, available: true, reason: null },
    ] }),
    importCpaCliAccount: async () => ({ provider: "anthropic", name: "ocg-cli-anthropic-f0e1d2.json", outcome: "alreadyImported", revision: 2, processGeneration: 1 }),
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  try {
    await settle();
    const accountReadsAfterLoad = accountReads;
    await (importButton(mounted.root, "Claude").props.onClick as () => Promise<void>)();
    await settle();
    assert.ok(accountReads > accountReadsAfterLoad, "an already-imported account still refreshes the list");
    const notices = oauthStatusAlerts(mounted.root, "success");
    assert.equal(notices.length, 1, "a success notice is shown");
    assert.match(text(notices[0]), /Claude/, "the notice names the provider label");
    assert.doesNotMatch(text(mounted.root), /ocg-cli-anthropic-f0e1d2\.json/, "the hashed implementation filename is never rendered");
    assert.ok(importButton(mounted.root, "Claude").props.disabled, "an already-imported source is greyed out");
  } finally { mounted.app.unmount(); }
});

test("an existing CLI-imported account greys that provider on load", async () => {
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
      { provider: "kimi", source: "kimi-cli", supported: true, available: true, reason: null },
    ] }),
    getCpaAccounts: async () => ({ accounts: [{
      authIndex: "1", disabled: false, email: null, label: "Codex", mutable: true,
      name: "ocg-cli-codex-a1b2c3.json", provider: "codex", quota: null, runtimeOnly: false,
      status: "ok", statusMessage: null, unavailable: false,
    }] }),
  }));
  try {
    await settle();
    assert.ok(importButton(mounted.root, "Codex").props.disabled, "a previously imported CLI account greys Import");
    assert.ok(!importButton(mounted.root, "Kimi").props.disabled, "a different available source stays importable");
  } finally { mounted.app.unmount(); }
});

test("empty CPA quota is omitted from the account row", async () => {
  const mounted = await mount(oauthComponentApi({
    getCpaAccounts: async () => ({ accounts: [{
      authIndex: "1", disabled: false, email: "a@b.com", label: "user", mutable: true,
      name: "codex-1", provider: "codex", quota: { signals: {} }, runtimeOnly: false,
      status: "active", statusMessage: null, unavailable: false,
    }] }),
  }));
  try {
    await settle();
    const page = text(mounted.root);
    assert.match(page, /user/);
    assert.ok(button(mounted.root, "重置配额"), "the account row renders its actions");
    assert.doesNotMatch(page, /signals/, "raw quota keys are never rendered");
    const row = elementsByClass(mounted.root, "cpa-account-row")[0];
    if (!row) assert.fail("the account row renders");
    assert.equal(elementsByClass(row, "cpa-muted").length, 1, "a vacuous quota renders no quota line");
  } finally { mounted.app.unmount(); }
});

test("unconfirmed import warns to refresh before retrying and does not refresh accounts", async () => {
  let accountReads = 0;
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
    ] }),
    importCpaCliAccount: async () => ({ provider: "codex", name: "codex-work", outcome: "unconfirmed", revision: 2, processGeneration: 1 }),
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  try {
    await settle();
    const accountReadsAfterLoad = accountReads;
    await (importButton(mounted.root, "Codex").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(accountReads, accountReadsAfterLoad, "an unconfirmed outcome never refreshes the list implicitly");
    assert.equal(oauthStatusAlerts(mounted.root, "warning").length, 1, "the refresh-before-retry warning is shown");
    assert.ok(!importButton(mounted.root, "Codex").props.disabled, "an unconfirmed import stays retryable");
  } finally { mounted.app.unmount(); }
});

test("CLI import and OAuth flows are mutually exclusive single-flight actions", async () => {
  const importRequest = deferred<unknown>();
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
    ] }),
    startCpaOAuth: async () => ({ state: "flow-1", provider: "codex", url: null, flow: "browser", userCode: null, expiresIn: null, revision: 1, processGeneration: 1 }),
    getCpaOAuthStatus: async (state: unknown) => ({ state, status: "pending" }),
    importCpaCliAccount: async () => {
      await importRequest.promise;
      return { provider: "codex", name: "codex-work", outcome: "imported", revision: 2, processGeneration: 1 };
    },
  }));
  try {
    await settle();
    await (button(mounted.root, "Codex 浏览器登录").props.onClick as () => Promise<void>)();
    await settle();
    assert.ok(importButton(mounted.root, "Codex").props.disabled, "an active OAuth flow disables import");
    await (button(mounted.root, "取消当前授权").props.onClick as () => Promise<void>)();
    await settle();
    const importClick = (importButton(mounted.root, "Codex").props.onClick as () => Promise<void>)();
    await settle();
    assert.ok(button(mounted.root, "Codex 浏览器登录").props.disabled, "an in-flight import disables OAuth starts");
    assert.ok(button(mounted.root, "Codex 设备码登录").props.disabled, "the device start is equally blocked");
    importRequest.resolve(undefined);
    await importClick;
    await settle();
    assert.ok(!button(mounted.root, "Codex 浏览器登录").props.disabled, "the OAuth start recovers after the import settles");
    assert.equal(oauthStatusAlerts(mounted.root, "success").length, 1, "the import success notice is shown");
  } finally { mounted.app.unmount(); }
});

test("CLI discovery failure keeps every fresh-login path working and allows manual re-detect", async () => {
  let discoveries = 0;
  const starts: unknown[] = [];
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => {
      discoveries += 1;
      if (discoveries === 1) throw new Error("discovery broke");
      return { sources: [{ provider: "codex", source: "codex-cli", supported: true, available: true, reason: null }] };
    },
    startCpaOAuth: async (input: unknown) => {
      starts.push(input);
      return { state: "flow-1", provider: "anthropic", url: null, flow: "browser", userCode: null, expiresIn: null, revision: 1, processGeneration: 1 };
    },
    getCpaOAuthStatus: async (state: unknown) => ({ state, status: "pending" }),
  }));
  try {
    await settle();
    assert.match(text(mounted.root), /discovery broke/, "the backend error detail is preserved");
    assert.equal(titledAlerts(mounted.root, "warning").length, 1, "the discovery failure is surfaced inline");
    await (button(mounted.root, "登录 Claude").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(starts[0], { provider: "anthropic", method: "browser" }, "fresh login still works after a discovery failure");
    await (button(mounted.root, "取消当前授权").props.onClick as () => Promise<void>)();
    await settle();
    await (button(mounted.root, "重新检测").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(discoveries, 2, "re-detect re-runs discovery only on demand");
    assert.ok(importButton(mounted.root, "Codex"), "the recovered discovery renders its import action");
  } finally { mounted.app.unmount(); }
});

test("unconfirmed import offers a manual account-list refresh without retrying the import", async () => {
  let accountReads = 0;
  const imports: unknown[] = [];
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
    ] }),
    importCpaCliAccount: async (input: unknown) => {
      imports.push(input);
      return { provider: "codex", name: "ocg-cli-codex-a1b2c3.json", outcome: "unconfirmed", revision: 2, processGeneration: 1 };
    },
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  try {
    await settle();
    await (importButton(mounted.root, "Codex").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(oauthStatusAlerts(mounted.root, "warning").length, 1, "the unconfirmed warning is shown");
    const accountReadsBeforeRefresh = accountReads;
    await (button(mounted.root, "刷新账号列表").props.onClick as () => Promise<void>)();
    await settle();
    assert.ok(accountReads > accountReadsBeforeRefresh, "the button refreshes the account list");
    assert.equal(imports.length, 1, "the refresh never retries the import");
  } finally { mounted.app.unmount(); }
});

test("a superseding page load ignores the older CLI discovery response", async () => {
  const staleDiscovery = deferred<unknown>();
  let discoveries = 0;
  const mounted = await mount(oauthComponentApi({
    getCpaCliImports: async () => {
      discoveries += 1;
      if (discoveries === 1) return staleDiscovery.promise as never;
      return { sources: [{ provider: "codex", source: "codex-cli", supported: true, available: true, reason: null }] };
    },
  }));
  try {
    await (button(mounted.root, "刷新").props.onClick as () => Promise<void>)();
    await settle();
    await settle();
    assert.ok(importButton(mounted.root, "Codex"), "the newer discovery renders");
    staleDiscovery.resolve({ sources: [{ provider: "xai", source: "late-marker-cli", supported: true, available: true, reason: null }] });
    await settle();
    assert.ok(!hasButton(mounted.root, "导入 xAI"), "the stale discovery response is ignored");
    assert.ok(importButton(mounted.root, "Codex"), "the newer discovery result stays");
  } finally { mounted.app.unmount(); }
});

test("a CLI import response arriving after disconnect is ignored without any undo attempt", async () => {
  const importRequest = deferred<unknown>();
  let accountReads = 0;
  let integrationReads = 0;
  let deletes = 0;
  const mounted = await mount(oauthComponentApi({
    getCpaIntegration: async () => {
      integrationReads += 1;
      return integrationReads === 1
        ? integration({ runtimeOwned: false, runtimeRunning: false })
        : integration({ configured: false, runtimeOwned: false, runtimeRunning: false });
    },
    getCpaRuntime: async () => runtime({ owned: false, running: false, supported: false }),
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
    ] }),
    importCpaCliAccount: async () => {
      await importRequest.promise;
      return { provider: "codex", name: "ocg-cli-codex-a1b2c3.json", outcome: "imported", revision: 2, processGeneration: 1 };
    },
    deleteCpaIntegration: async () => { deletes += 1; return { revision: 2, processGeneration: 1 }; },
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  try {
    await settle();
    const importClick = (importButton(mounted.root, "Codex").props.onClick as () => Promise<void>)();
    await settle();
    await (button(mounted.root, "断开并清除").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(deletes, 1, "the disconnect ran while the import was in flight");
    const accountReadsAfterDisconnect = accountReads;
    importRequest.resolve(undefined);
    await importClick;
    await settle();
    assert.equal(accountReads, accountReadsAfterDisconnect, "the late import response does not refresh accounts");
    assert.equal(oauthStatusAlerts(mounted.root, "success").length, 0, "no success notice is applied after disconnect");
    assert.doesNotMatch(text(mounted.root), /ocg-cli-codex-a1b2c3\.json/, "the late response never leaks the filename either");
  } finally { mounted.app.unmount(); }
});

test("CLI discovery and import responses after unmount are ignored", async () => {
  const staleDiscovery = deferred<unknown>();
  let discoveryReads = 0;
  const first = await mount(oauthComponentApi({
    getCpaCliImports: async () => {
      discoveryReads += 1;
      await staleDiscovery.promise;
      return { sources: [{ provider: "xai", source: "late-marker-cli", supported: true, available: true, reason: null }] };
    },
  }));
  first.app.unmount();
  staleDiscovery.resolve(undefined);
  await settle();
  assert.equal(discoveryReads, 1, "the late discovery response triggers no re-fetch after unmount");

  const importRequest = deferred<unknown>();
  let accountReads = 0;
  const second = await mount(oauthComponentApi({
    getCpaCliImports: async () => ({ sources: [
      { provider: "codex", source: "codex-cli", supported: true, available: true, reason: null },
    ] }),
    importCpaCliAccount: async () => {
      await importRequest.promise;
      return { provider: "codex", name: "ocg-cli-codex-a1b2c3.json", outcome: "imported", revision: 2, processGeneration: 1 };
    },
    getCpaAccounts: async () => { accountReads += 1; return { accounts: [] }; },
  }));
  await settle();
  void (importButton(second.root, "Codex").props.onClick as () => Promise<void>)();
  await settle();
  const accountReadsBeforeUnmount = accountReads;
  second.app.unmount();
  importRequest.resolve(undefined);
  await settle();
  assert.equal(accountReads, accountReadsBeforeUnmount, "the late import response does not refresh accounts after unmount");
});
