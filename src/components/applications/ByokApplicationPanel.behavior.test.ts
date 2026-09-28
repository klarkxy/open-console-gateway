import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { ssrContextKey, type App, type Component } from "vue";
import { createPinia, type Pinia } from "pinia";
import type { ByokApplicationView } from "../../api/byok-applications.ts";
import {
  createVueHostRenderer,
  deferred,
  installTestWindow,
  settle,
  walkHostNodes,
  type HostNode,
} from "../../test-helpers/vue-host-runtime.ts";

type ByokApi = {
  inspect: (client: string, targetPath?: string) => Promise<ByokApplicationView>;
  configure: (client: string, input: Record<string, unknown>, expectation: unknown) => Promise<ByokApplicationView>;
  remove: (client: string, input: Record<string, unknown>, expectation: unknown) => Promise<ByokApplicationView>;
  recover: (client: string, input: Record<string, unknown>, expectation: unknown) => Promise<ByokApplicationView>;
};

type ConnectionMock = {
  info: { primary_key: string; sub_keys: Array<{ id: string; name: string; enabled: boolean }> } | null;
  loadCount: number;
  reloadCount: number;
  reloadError: Error | null;
  sessionEpoch: number;
  reloadWait: Promise<ConnectionMock["info"]> | null;
  currentSession: () => number;
  load: () => Promise<ConnectionMock["info"]>;
  reloadAfterMutation: (expectedSession?: number) => Promise<ConnectionMock["info"]>;
};

let buildDir: string;
let ByokApplicationPanel: Component;
const renderer = createVueHostRenderer();

function byokHarnessPlugin() {
  const prefix = "\0byok-panel-harness:";
  const modules: Record<string, string> = {
    naive: `
      import { defineComponent, h } from "vue";
      const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
      } });
      export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("button", attrs, slots.default?.());
      } });
      export const NAlert = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, [attrs.title, ...Object.values(slots).flatMap((slot) => slot?.() ?? [])]);
      } });
      export const NModal = defineComponent({
        inheritAttrs: false,
        props: { show: { type: Boolean, default: false }, title: String },
        setup(props, { attrs, slots }) {
          return () => props.show
            ? h("div", { ...attrs, role: "dialog", title: props.title }, [slots.default?.(), slots.footer?.()])
            : null;
        },
      });
      export const NSelect = pass;
      export const NInput = defineComponent({ inheritAttrs: false, props: { value: String }, setup(props, { attrs }) {
        return () => h("input", { ...attrs, value: props.value });
      } });
      export const NCheckbox = defineComponent({ inheritAttrs: false, props: { checked: Boolean }, setup(props, { attrs, slots }) {
        return () => h("label", { ...attrs, "data-checked": props.checked }, slots.default?.());
      } });
      export const NSpin = pass;
      export const NTag = pass;
      export const useMessage = () => {
        const record = (type) => (...args) => { (globalThis.__byokMessages ??= []).push({ type, args }); };
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
      globalThis.__ByokDashboardRequestError = DashboardRequestError;
      export function isRevisionConflict(error) {
        return Boolean(error && error.status === 409 && error.code === "revisionConflict");
      }
    `,
    dashboardV3: `export const PRIMARY_KEY_ID = "00000000-0000-0000-0000-000000000001";`,
    byokApi: `
      export const byokApplicationsApi = new Proxy({}, { get: (_, key) => (...args) => globalThis.__byokApi[key](...args) });
    `,
    connection: `export const useConnectionStore = () => globalThis.__byokConnectionStore;`,
    session: `export const useSessionStore = () => ({ authenticated: true });`,
    store: `
      export const useControlPlaneStore = () => ({
        hasTokens: () => true,
        refresh: async () => ({ expectedRevision: 1, processGeneration: 1 }),
        runMutation: async (run, expectation) => run(expectation ?? { expectedRevision: 1, processGeneration: 1 }),
      });
    `,
    i18n: `export const t = (key, values = {}) => key.replace(/\\{(\\w+)\\}/g, (_, name) => String(values[name] ?? ""));`,
    errors: `export const dashboardErrorDetail = (error) => error instanceof Error ? error.message : String(error);`,
    modal: `export const useLocalizedModalCloseLabel = () => {};`,
  };
  const byPath = new Map([
    ["src/api/dashboard.ts", "dashboard"],
    ["src/api/dashboard-v3.ts", "dashboardV3"],
    ["src/api/byok-applications.ts", "byokApi"],
    ["src/stores/connection.ts", "connection"],
    ["src/stores/controlPlane.ts", "store"],
    ["src/stores/session.ts", "session"],
    ["src/i18n/index.ts", "i18n"],
    ["src/utils/errors.ts", "errors"],
    ["src/utils/modal-close-label.ts", "modal"],
  ]);
  const root = process.cwd().replaceAll("\\", "/");
  return {
    name: "byok-panel-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      if (!importer || !source.startsWith(".")) return null;
      const cleanImporter = importer.split("?")[0]!.replaceAll("\\", "/");
      const absolute = path.posix.normalize(path.posix.join(path.posix.dirname(cleanImporter), source));
      const module = byPath.get(path.posix.relative(root, absolute));
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

function byokView(overrides: Record<string, unknown> = {}): ByokApplicationView {
  return {
    client: "codex",
    status: "ready",
    detected: true,
    configPath: "C:\\Users\\author\\.codex\\config.toml",
    discoverySource: "default",
    targetPaths: ["C:\\Users\\author\\.codex\\config.toml"],
    configureSupported: true,
    removeSupported: false,
    recoverySupported: false,
    requiresClosedClient: true,
    activationRequired: false,
    fingerprint: "fp-1",
    configuredModelIds: [],
    defaultModelId: null,
    backupPath: null,
    detail: null,
    gatewayV1Url: "http://127.0.0.1:8317/v1",
    revision: { revision: 7, processGeneration: 3, pricingRevision: "p" },
    ...overrides,
  } as ByokApplicationView;
}

function requestError(message: string, status: number): Error {
  const Ctor = (globalThis as unknown as {
    __ByokDashboardRequestError: new (message: string, status: number) => Error;
  }).__ByokDashboardRequestError;
  return new Ctor(message, status);
}

function findByClass(root: HostNode, className: string): HostNode | undefined {
  return walkHostNodes(root).find((node) => node.props.class === className);
}

function dialogOf(root: HostNode): HostNode | undefined {
  return walkHostNodes(root).find((node) => node.props.role === "dialog");
}

function confirmButton(dialog: HostNode): HostNode {
  const found = walkHostNodes(dialog).find((node) => node.type === "button" && node.props.type === "primary");
  if (!found) throw new Error("confirm button should render");
  return found;
}

function closedClientCheckbox(dialog: HostNode): HostNode {
  const found = walkHostNodes(dialog).find((node) => node.type === "label" && "data-checked" in node.props);
  if (!found) throw new Error("closed-client acknowledgement should render");
  return found;
}

function refreshButton(root: HostNode): HostNode {
  const toolbar = findByClass(root, "byok-toolbar");
  const found = toolbar && walkHostNodes(toolbar).find((node) => node.type === "button");
  if (!found) throw new Error("refresh button should render");
  return found;
}

async function click(node: HostNode): Promise<void> {
  await (node.props.onClick as () => Promise<void>)();
  await settle();
}

function createConnectionMock(): ConnectionMock {
  return {
    info: null,
    loadCount: 0,
    reloadCount: 0,
    reloadError: null,
    sessionEpoch: 0,
    reloadWait: null,
    currentSession() {
      return this.sessionEpoch;
    },
    async load() {
      this.loadCount += 1;
      return this.info;
    },
    async reloadAfterMutation(expectedSession?: number) {
      if (expectedSession !== undefined && expectedSession !== this.sessionEpoch) return this.info;
      this.reloadCount += 1;
      if (this.reloadError) throw this.reloadError;
      if (this.reloadWait) return this.reloadWait;
      return this.info;
    },
  };
}

interface Mounted {
  app: App;
  root: HostNode;
  api: ByokApi;
  calls: Array<{ method: string; args: unknown[] }>;
  pinia: Pinia;
  connection: ConnectionMock;
}

async function mount(options: {
  client?: string;
  inspect: () => Promise<ByokApplicationView>;
  api?: Partial<ByokApi>;
  pinia?: Pinia;
  connection?: ConnectionMock;
}): Promise<Mounted> {
  installTestWindow({
    href: "http://127.0.0.1/dashboard/?view=applications&app=codex",
    search: "?view=applications&app=codex",
  });
  const calls: Mounted["calls"] = [];
  const record = <K extends keyof ByokApi>(method: K, fallback: ByokApi[K]): ByokApi[K] =>
    ((...args: unknown[]) => {
      calls.push({ method, args });
      const impl = (options.api?.[method] ?? fallback) as (...rest: unknown[]) => Promise<ByokApplicationView>;
      return impl(...args);
    }) as ByokApi[K];
  const api: ByokApi = {
    inspect: record("inspect", options.inspect),
    configure: record("configure", async () => { throw new Error("configure not stubbed"); }),
    remove: record("remove", async () => { throw new Error("remove not stubbed"); }),
    recover: record("recover", async () => { throw new Error("recover not stubbed"); }),
  };
  (globalThis as { __byokApi?: ByokApi }).__byokApi = api;
  (globalThis as unknown as { __byokMessages?: Array<{ type: string }> }).__byokMessages = [];
  const connection = options.connection ?? createConnectionMock();
  (globalThis as { __byokConnectionStore?: ConnectionMock }).__byokConnectionStore = connection;
  const pinia = options.pinia ?? createPinia();
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(ByokApplicationPanel, { client: options.client ?? "codex" });
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.use(pinia);
  app.mount(root);
  await settle();
  return { app, root, api, calls, pinia, connection };
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "byok-panel-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [byokHarnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      lib: {
        entry: path.resolve("src/components/applications/ByokApplicationPanel.vue"),
        fileName: () => "byok-panel.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue", "pinia"] },
    },
  });
  ByokApplicationPanel = (await import(pathToFileURL(path.join(buildDir, "byok-panel.mjs")).href)).default;
});

after(async () => {
  await rm(buildDir, { force: true, recursive: true });
});

test("first inspect resolves out of the loading state and renders the status region", async () => {
  const pending = deferred<ByokApplicationView>();
  const mounted = await mount({ inspect: () => pending.promise });
  try {
    assert.ok(findByClass(mounted.root, "byok-state"), "loading region should render while pending");
    pending.resolve(byokView());
    await settle();
    assert.equal(findByClass(mounted.root, "byok-state"), undefined);
    assert.ok(findByClass(mounted.root, "byok-actions"), "actions should render after the first load");
    assert.ok(findByClass(mounted.root, "byok-target"), "target path editor should render");
  } finally {
    mounted.app.unmount();
  }
});

test("configure sends target, fingerprint and closed-client ack without Key, models or picker preload", async () => {
  const mounted = await mount({
    inspect: async () => byokView(),
    api: { configure: async () => byokView({ status: "configured", configuredModelIds: ["ocg/model-a"], activationRequired: true }) },
  });
  try {
    const actions = findByClass(mounted.root, "byok-actions")!;
    const configureButton = walkHostNodes(actions).find((node) => node.type === "button" && node.props.type === "primary")!;
    await click(configureButton);
    const dialog = dialogOf(mounted.root)!;
    assert.ok(dialog, "configure dialog should open");
    assert.equal(mounted.connection.loadCount, 0, "opening configure must not load plaintext Keys");
    assert.equal(walkHostNodes(dialog).some((node) => node.props.class === "byok-model-select"), false);
    assert.equal(walkHostNodes(dialog).some((node) => node.props.class === "byok-key-group"), false);

    assert.equal(confirmButton(dialog).props.disabled, true, "closed-client acknowledgement gates confirm");
    const checkbox = closedClientCheckbox(dialogOf(mounted.root)!);
    (checkbox.props["onUpdate:checked"] as (value: boolean) => void)(true);
    await settle();
    assert.equal(confirmButton(dialogOf(mounted.root)!).props.disabled, false);

    await click(confirmButton(dialogOf(mounted.root)!));
    const call = mounted.calls.find((entry) => entry.method === "configure")!;
    const input = call.args[1] as Record<string, unknown>;
    assert.equal(Object.prototype.hasOwnProperty.call(input, "keyId"), false);
    assert.equal(Object.prototype.hasOwnProperty.call(input, "models"), false);
    assert.equal(Object.prototype.hasOwnProperty.call(input, "defaultModelId"), false);
    assert.equal(input.clientClosed, true);
    assert.equal(input.expectedFingerprint, "fp-1");
    assert.equal(input.targetPath, null);
    assert.equal(mounted.connection.reloadCount, 1);
    assert.equal(dialogOf(mounted.root), undefined, "dialog closes after save");
    const messages = (globalThis as unknown as { __byokMessages: Array<{ type: string }> }).__byokMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
  } finally {
    mounted.app.unmount();
  }
});

test("configure success is kept when connection reload fails", async () => {
  const mounted = await mount({
    inspect: async () => byokView(),
    api: { configure: async () => byokView({ status: "configured", configuredModelIds: ["ocg/model-a"] }) },
  });
  try {
    mounted.connection.reloadError = new Error("reload failed");
    const actions = findByClass(mounted.root, "byok-actions")!;
    await click(walkHostNodes(actions).find((node) => node.type === "button" && node.props.type === "primary")!);
    (closedClientCheckbox(dialogOf(mounted.root)!).props["onUpdate:checked"] as (value: boolean) => void)(true);
    await settle();
    await click(confirmButton(dialogOf(mounted.root)!));
    assert.equal(dialogOf(mounted.root), undefined);
    const messages = (globalThis as unknown as { __byokMessages: Array<{ type: string }> }).__byokMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
    assert.equal(mounted.connection.reloadCount, 1);
  } finally {
    mounted.app.unmount();
  }
});

test("configure success closes before a slow connection reload finishes", async () => {
  const pendingReload = deferred<ConnectionMock["info"]>();
  const mounted = await mount({
    inspect: async () => byokView(),
    api: { configure: async () => byokView({ status: "configured", configuredModelIds: ["ocg/model-a"] }) },
  });
  try {
    mounted.connection.reloadWait = pendingReload.promise;
    const actions = findByClass(mounted.root, "byok-actions")!;
    await click(walkHostNodes(actions).find((node) => node.type === "button" && node.props.type === "primary")!);
    (closedClientCheckbox(dialogOf(mounted.root)!).props["onUpdate:checked"] as (value: boolean) => void)(true);
    await settle();
    await click(confirmButton(dialogOf(mounted.root)!));
    assert.equal(dialogOf(mounted.root), undefined, "dialog closes without waiting for connection reload");
    const messages = (globalThis as unknown as { __byokMessages: Array<{ type: string }> }).__byokMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
    assert.equal(mounted.connection.reloadCount, 1);
    pendingReload.resolve(mounted.connection.info);
    await settle();
  } finally {
    mounted.app.unmount();
  }
});

test("logout during configure does not start a connection reload after the receipt", async () => {
  const pendingConfigure = deferred<ByokApplicationView>();
  const mounted = await mount({
    inspect: async () => byokView(),
    api: { configure: () => pendingConfigure.promise },
  });
  try {
    const actions = findByClass(mounted.root, "byok-actions")!;
    await click(walkHostNodes(actions).find((node) => node.type === "button" && node.props.type === "primary")!);
    (closedClientCheckbox(dialogOf(mounted.root)!).props["onUpdate:checked"] as (value: boolean) => void)(true);
    await settle();
    const confirm = confirmButton(dialogOf(mounted.root)!).props.onClick as () => Promise<void>;
    const pendingSave = confirm();
    await settle();
    mounted.connection.sessionEpoch += 1;
    pendingConfigure.resolve(byokView({ status: "configured", configuredModelIds: ["ocg/model-a"] }));
    await pendingSave;
    await settle();
    assert.equal(dialogOf(mounted.root), undefined, "successful save still closes");
    const messages = (globalThis as unknown as { __byokMessages: Array<{ type: string }> }).__byokMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
    assert.equal(mounted.connection.reloadCount, 0, "logout must skip the delayed plaintext reload");
  } finally {
    mounted.app.unmount();
  }
});

test("failed configure still refreshes the connection store", async () => {
  const mounted = await mount({
    inspect: async () => byokView(),
    api: { configure: async () => { throw new Error("write failed"); } },
  });
  try {
    const actions = findByClass(mounted.root, "byok-actions")!;
    await click(walkHostNodes(actions).find((node) => node.type === "button" && node.props.type === "primary")!);
    (closedClientCheckbox(dialogOf(mounted.root)!).props["onUpdate:checked"] as (value: boolean) => void)(true);
    await settle();
    await click(confirmButton(dialogOf(mounted.root)!));
    assert.ok(dialogOf(mounted.root), "dialog stays open on a non-conflict failure");
    assert.equal(mounted.connection.reloadCount, 1);
  } finally {
    mounted.app.unmount();
  }
});

test("remove works without a current Key or model and sends no keyId", async () => {
  const mounted = await mount({
    inspect: async () => byokView({
      status: "configured",
      configuredModelIds: ["ocg/gone-model"],
      removeSupported: true,
    }),
    api: { remove: async () => byokView({ status: "ready", fingerprint: "fp-2" }) },
  });
  try {
    const actions = findByClass(mounted.root, "byok-actions")!;
    const buttons = walkHostNodes(actions).filter((node) => node.type === "button");
    assert.equal(buttons.length, 2, "configure and remove actions render");
    await click(buttons[1]!);
    const dialog = dialogOf(mounted.root)!;
    assert.ok(dialog, "remove dialog should open");
    (closedClientCheckbox(dialog).props["onUpdate:checked"] as (value: boolean) => void)(true);
    await settle();
    await click(confirmButton(dialogOf(mounted.root)!));
    const call = mounted.calls.find((entry) => entry.method === "remove")!;
    const input = call.args[1] as Record<string, unknown>;
    assert.equal(Object.prototype.hasOwnProperty.call(input, "keyId"), false);
    assert.equal(input.expectedFingerprint, "fp-1");
    const messages = (globalThis as unknown as { __byokMessages: Array<{ type: string }> }).__byokMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
  } finally {
    mounted.app.unmount();
  }
});

test("a 409 configure closes the dialog, surfaces an error and reloads the status", async () => {
  let inspections = 0;
  const mounted = await mount({
    inspect: async () => {
      inspections += 1;
      return byokView({ fingerprint: inspections === 1 ? "fp-1" : "fp-2" });
    },
    api: { configure: async () => { throw requestError("revision moved", 409); } },
  });
  try {
    const actions = findByClass(mounted.root, "byok-actions")!;
    await click(walkHostNodes(actions).find((node) => node.type === "button" && node.props.type === "primary")!);
    (closedClientCheckbox(dialogOf(mounted.root)!).props["onUpdate:checked"] as (value: boolean) => void)(true);
    await settle();
    await click(confirmButton(dialogOf(mounted.root)!));
    assert.equal(dialogOf(mounted.root), undefined, "dialog closes on conflict");
    assert.equal(inspections, 2, "status reloads after the conflict");
    const errors = walkHostNodes(mounted.root).filter(
      (node) => node.props.type === "error" && typeof node.props.title === "string",
    );
    assert.equal(errors.length, 1);
  } finally {
    mounted.app.unmount();
  }
});

test("revisit renders cached inspection without a second fetch; Refresh inspects again", async () => {
  let inspections = 0;
  const inspect = async () => {
    inspections += 1;
    return byokView({ fingerprint: `fp-${inspections}` });
  };
  const first = await mount({ inspect });
  assert.equal(inspections, 1);
  first.app.unmount();
  const second = await mount({ inspect, pinia: first.pinia, connection: first.connection });
  try {
    assert.equal(inspections, 1, "cached revisit must not inspect again");
    assert.ok(findByClass(second.root, "byok-actions"));
    await click(refreshButton(second.root));
    assert.equal(inspections, 2, "explicit Refresh inspects again");
  } finally {
    second.app.unmount();
  }
});

test("remount while inspect is pending joins the in-flight request", async () => {
  const pending = deferred<ByokApplicationView>();
  let inspections = 0;
  const inspect = () => {
    inspections += 1;
    return pending.promise;
  };
  const first = await mount({ inspect });
  assert.equal(inspections, 1);
  assert.ok(findByClass(first.root, "byok-state"));
  first.app.unmount();
  const second = await mount({ inspect, pinia: first.pinia, connection: first.connection });
  try {
    assert.equal(inspections, 1, "pending remount must not start a second inspect");
    pending.resolve(byokView());
    await settle();
    assert.ok(findByClass(second.root, "byok-actions"));
  } finally {
    second.app.unmount();
  }
});
