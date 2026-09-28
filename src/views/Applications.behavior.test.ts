import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { ssrContextKey, type App, type Component } from "vue";
import { createPinia, type Pinia } from "pinia";
import { createMemoryHistory, createRouter, type Router } from "vue-router";
import type { ByokApplicationView } from "../api/byok-applications.ts";
import type { DshApplicationView } from "../api/dashboard-v4.ts";
import {
  createVueHostRenderer,
  deferred,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

type DshApi = {
  getDshApplication: (profilePath?: string, runtimeUrl?: string) => Promise<DshApplicationView>;
  installDshApplication: (
    input: { keyId?: string; profilePath?: string; runtimeUrl?: string | null; expectedFingerprint: string },
    expectation: { expectedRevision: number; processGeneration: number },
  ) => Promise<DshApplicationView>;
  uninstallDshApplication: (
    input: { profilePath?: string; runtimeUrl?: string | null; expectedFingerprint: string },
    expectation: { expectedRevision: number; processGeneration: number },
  ) => Promise<DshApplicationView>;
};

type ByokApi = {
  inspect: (client: string, targetPath?: string) => Promise<ByokApplicationView>;
  configure: (client: string, input: Record<string, unknown>, expectation: unknown) => Promise<ByokApplicationView>;
  remove: (client: string, input: Record<string, unknown>, expectation: unknown) => Promise<ByokApplicationView>;
  recover: (client: string, input: Record<string, unknown>, expectation: unknown) => Promise<ByokApplicationView>;
};

type ConnectionState = {
  info: { primary_key: string; sub_keys: Array<{ id: string; name: string; enabled: boolean; value: string }> } | null;
  loadCount: number;
  reloadCount: number;
  reloadError: Error | null;
  sessionEpoch: number;
  reloadWait: Promise<ConnectionState["info"]> | null;
  currentSession: () => number;
  load: () => Promise<ConnectionState["info"]>;
  reloadAfterMutation: (expectedSession?: number) => Promise<ConnectionState["info"]>;
};

let buildDir: string;
let Applications: Component;
let DshPanel: Component;
const renderer = createVueHostRenderer();

function applicationsHarnessPlugin() {
  const prefix = "\0dsh-applications-harness:";
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
        const record = (type) => (...args) => { (globalThis.__dshMessages ??= []).push({ type, args }); };
        return { error: record("error"), success: record("success"), warning: record("warning") };
      };
    `,
    reka: `
      import { computed, defineComponent, h, inject, provide } from "vue";
      const key = Symbol("applications-tabs");
      export const TabsRoot = defineComponent({
        props: { modelValue: String, activationMode: { type: String, default: "automatic" } },
        emits: ["update:modelValue"],
        setup(props, { slots, emit }) {
          provide(key, {
            current: computed(() => props.modelValue),
            select: (value) => emit("update:modelValue", value),
          });
          return () => h("div", { "data-activation-mode": props.activationMode }, slots.default?.());
        },
      });
      export const TabsList = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", { ...attrs, role: "tablist" }, slots.default?.());
      } });
      export const TabsTrigger = defineComponent({ inheritAttrs: false, props: { value: String }, setup(props, { attrs }) {
        const ctx = inject(key);
        return () => h("button", {
          ...attrs,
          role: "tab",
          "data-state": ctx.current.value === props.value ? "active" : "inactive",
          tabindex: ctx.current.value === props.value ? 0 : -1,
          onClick: () => ctx.select(props.value),
        });
      } });
      export const TabsContent = defineComponent({ props: { value: String }, setup(props, { slots }) {
        const ctx = inject(key);
        return () => ctx.current.value === props.value ? h("div", { role: "tabpanel" }, slots.default?.()) : null;
      } });
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
      globalThis.__DshDashboardRequestError = DashboardRequestError;
      export function isRevisionConflict(error) {
        return Boolean(error && error.status === 409 && error.code === "revisionConflict");
      }
    `,
    dashboardV3: `export const PRIMARY_KEY_ID = "00000000-0000-0000-0000-000000000001";`,
    api: `
      export const dashboardV4 = new Proxy({}, { get: (_, key) => (...args) => globalThis.__dshComponentApi[key](...args) });
    `,
    byokApi: `
      export const byokApplicationsApi = new Proxy({}, { get: (_, key) => (...args) => globalThis.__byokApi[key](...args) });
    `,
    connection: `export const useConnectionStore = () => globalThis.__dshConnectionStore;`,
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
    ["src/api/dashboard-v4.ts", "api"],
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
    name: "dsh-applications-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      if (source === "reka-ui") return `${prefix}reka`;
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

function dshApp(overrides: Partial<DshApplicationView> = {}): DshApplicationView {
  return {
    selectedProfilePath: "C:\\Users\\author\\.dsh\\profiles\\web",
    status: "ready",
    detected: true,
    installed: false,
    installSupported: true,
    activationRequired: false,
    version: "0.1.5-rc.2",
    detail: "Ready to install the OCG provider into the DSH web profile",
    targetPaths: ["C:\\\\ocg\\\\applications\\\\dsh"],
    discoveredProfiles: [],
    fingerprint: "fp-1",
    revision: { revision: 7, processGeneration: 3, pricingRevision: "p" },
    runtimeUrl: "http://127.0.0.1:3080",
    uninstallSupported: false,
    enabled: false,
    application: null,
    ...overrides,
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
    __DshDashboardRequestError: new (message: string, status: number) => Error;
  }).__DshDashboardRequestError;
  return new Ctor(message, status);
}

function installActionButton(root: HostNode): HostNode {
  const actions = walkHostNodes(root).find((node) => node.props.class === "dsh-actions");
  const found = actions && walkHostNodes(actions).find((node) => node.type === "button");
  if (!found) throw new Error("install action button should render");
  return found;
}

function installConfirmButton(root: HostNode): HostNode {
  const dialog = walkHostNodes(root).find((node) => node.props.role === "dialog");
  const found = dialog
    && walkHostNodes(dialog).find((node) => node.type === "button" && node.props.type === "primary");
  if (!found) throw new Error("install confirm button should render");
  return found;
}

function errorAlertCount(root: HostNode): number {
  return walkHostNodes(root).filter(
    (node) => node.props.type === "error" && typeof node.props.title === "string",
  ).length;
}

function defaultByokApi(): ByokApi {
  return {
    inspect: async (client) => byokView({ client }),
    configure: async () => { throw new Error("configure not stubbed"); },
    remove: async () => { throw new Error("remove not stubbed"); },
    recover: async () => { throw new Error("recover not stubbed"); },
  };
}

function createConnectionState(): ConnectionState {
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

function runtimeInput(root: HostNode): HostNode {
  const found = walkHostNodes(root).find((node) => node.props.class === "dsh-runtime-input");
  if (!found) throw new Error("runtime URL input should render");
  return found;
}

async function mount(options: {
  shell?: boolean;
  api: Partial<DshApi> & Pick<DshApi, "getDshApplication">;
  byokApi?: Partial<ByokApi>;
  connection?: ConnectionState;
  pinia?: Pinia;
}): Promise<{ app: App; root: HostNode; connection: ConnectionState; router: Router; pinia: Pinia }> {
  installTestWindow({
    href: "http://127.0.0.1/dashboard/?view=applications",
    search: "?view=applications",
  });
  const connection = options.connection ?? createConnectionState();
  (globalThis as unknown as { __dshConnectionStore?: ConnectionState }).__dshConnectionStore = connection;
  (globalThis as unknown as { __dshMessages?: Array<{ type: string; args: unknown[] }> }).__dshMessages = [];
  const api: DshApi = {
    installDshApplication: async () => {
      throw new Error("installDshApplication not stubbed");
    },
    uninstallDshApplication: async () => {
      throw new Error("uninstallDshApplication not stubbed");
    },
    ...options.api,
  };
  (globalThis as { __dshComponentApi?: DshApi }).__dshComponentApi = api;
  const byokApi: ByokApi = { ...defaultByokApi(), ...options.byokApi };
  (globalThis as { __byokApi?: ByokApi }).__byokApi = byokApi;
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(options.shell ? Applications : DshPanel);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: "/:pathMatch(.*)*", name: "applications", component: { render: () => null } }],
  });
  await router.push("/applications");
  app.use(router);
  const pinia = options.pinia ?? createPinia();
  app.use(pinia);
  app.mount(root);
  await settle();
  return { app, root, connection, router, pinia };
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "dsh-applications-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [applicationsHarnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      lib: {
        entry: {
          applications: path.resolve("src/views/Applications.vue"),
          "dsh-panel": path.resolve("src/components/applications/DshApplicationPanel.vue"),
        },
        fileName: (_format, name) => `${name}.mjs`,
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue", "vue-router", "pinia"] },
    },
  });
  Applications = (await import(pathToFileURL(path.join(buildDir, "applications.mjs")).href)).default;
  DshPanel = (await import(pathToFileURL(path.join(buildDir, "dsh-panel.mjs")).href)).default;
});

after(async () => {
  await rm(buildDir, { force: true, recursive: true });
});

test("opening install does not load plaintext Keys and omits keyId", async () => {
  let installed: Record<string, unknown> | undefined;
  const mounted = await mount({
    api: {
      getDshApplication: async () => dshApp(),
      installDshApplication: async (input) => {
        installed = input as Record<string, unknown>;
        return dshApp({ status: "installed", installed: true });
      },
    },
  });
  try {
    await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), true);
    assert.equal(mounted.connection.loadCount, 0);
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.class === "dsh-key-group"), false);
    await (installConfirmButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(Object.prototype.hasOwnProperty.call(installed ?? {}, "keyId"), false);
    assert.equal(mounted.connection.reloadCount, 1);
  } finally {
    mounted.app.unmount();
  }
});

test("selecting a detected profile inspects and installs that exact target", async () => {
  const editorPath = "C:\\Users\\author\\.dsh-editor\\profiles\\dsh-editor";
  const inspected: Array<{ path?: string; runtimeUrl?: string }> = [];
  let installedTarget: string | undefined;
  let installedFingerprint: string | undefined;
  let installedRuntime: string | undefined | null;
  const discoveredProfiles = [
    { home: "C:\\Users\\author\\.dsh", name: "web", path: "C:\\Users\\author\\.dsh\\profiles\\web" },
    { home: "C:\\Users\\author\\.dsh-editor", name: "dsh-editor", path: editorPath },
  ];
  const mounted = await mount({
    api: {
      getDshApplication: async (path, runtimeUrl) => {
        inspected.push({ path, runtimeUrl });
        return dshApp({
          selectedProfilePath: path ?? discoveredProfiles[0]!.path,
          targetPaths: [path ?? discoveredProfiles[0]!.path],
          fingerprint: path ? "editor-fingerprint" : "web-fingerprint",
          runtimeUrl: runtimeUrl ?? (path ? null : "http://127.0.0.1:3080"),
          discoveredProfiles,
        });
      },
      installDshApplication: async (input) => {
        installedTarget = input.profilePath;
        installedFingerprint = input.expectedFingerprint;
        installedRuntime = input.runtimeUrl;
        return dshApp({ selectedProfilePath: editorPath, status: "installed", installed: true, discoveredProfiles });
      },
    },
  });
  try {
    const select = walkHostNodes(mounted.root).find((node) => node.props.class === "dsh-profile-select");
    assert.ok(select);
    assert.equal((select.props.options as Array<{ value: string }>).length, 2);
    (select.props["onUpdate:value"] as (value: string) => void)(editorPath);
    await settle();
    assert.deepEqual(inspected.map((item) => item.path), [undefined, editorPath]);
    assert.equal(installActionButton(mounted.root).props.disabled, false);
    await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    await (installConfirmButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(installedTarget, editorPath);
    assert.equal(installedFingerprint, "editor-fingerprint");
    assert.equal(installedRuntime, undefined);
  } finally {
    mounted.app.unmount();
  }
});

test("a 409 install closes the dialog, explains the change, and refreshes status", async () => {
  let loads = 0;
  const mounted = await mount({
    api: {
      getDshApplication: async () => {
        loads += 1;
        return loads === 1
          ? dshApp()
          : dshApp({
            status: "conflict",
            installSupported: false,
            fingerprint: null,
            detail: "DSH has a same-name package that is not an OCG-managed source",
          });
      },
      installDshApplication: async () => {
        throw requestError("revision moved", 409);
      },
    },
  });
  try {
    await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), true);
    await (installConfirmButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), false);
    assert.equal(errorAlertCount(mounted.root), 1);
    assert.match(text(mounted.root), /DSH has a same-name package that is not an OCG-managed source/);
    assert.equal(loads, 2);
    assert.equal(installActionButton(mounted.root).props.disabled, true);
  } finally {
    mounted.app.unmount();
  }
});

test("a repeated click while installing is ignored", async () => {
  const pending = deferred<DshApplicationView>();
  let installs = 0;
  const mounted = await mount({
    api: {
      getDshApplication: async () => dshApp(),
      installDshApplication: async () => {
        installs += 1;
        return pending.promise;
      },
    },
  });
  try {
    await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    const confirm = installConfirmButton(mounted.root);
    assert.equal(confirm.props.disabled, false);
    const first = (confirm.props.onClick as () => Promise<void>)();
    await settle();
    await (confirm.props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(installs, 1);
    pending.resolve(dshApp({ status: "installed", installed: true, activationRequired: true, detail: null }));
    await first;
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), false);
  } finally {
    mounted.app.unmount();
  }
});

test("blocked conflict and incompatible states keep the action disabled and show host detail", async () => {
  const conflict = await mount({
    api: {
      getDshApplication: async () => dshApp({
        status: "conflict",
        installSupported: false,
        fingerprint: null,
        detail: "DSH has only part of the OCG plugin registration",
      }),
    },
  });
  try {
    assert.equal(installActionButton(conflict.root).props.disabled, true);
    assert.match(text(conflict.root), /DSH has only part of the OCG plugin registration/);
    assert.doesNotMatch(text(conflict.root), /Ready to install the OCG provider/);
  } finally {
    conflict.app.unmount();
  }

  const incompatible = await mount({
    api: {
      getDshApplication: async () => dshApp({
        status: "incompatible",
        installSupported: false,
        detail: "DSH 0.1.4 is not a supported 0.1.5-rc.1 or 0.1.5-rc.2 build",
      }),
    },
  });
  try {
    assert.equal(installActionButton(incompatible.root).props.disabled, true);
    assert.match(text(incompatible.root), /DSH 0\.1\.4 is not a supported 0.1.5-rc.1 or 0.1.5-rc.2 build/);
  } finally {
    incompatible.app.unmount();
  }

  const ready = await mount({
    api: { getDshApplication: async () => dshApp() },
  });
  try {
    assert.equal(installActionButton(ready.root).props.disabled, false);
    assert.doesNotMatch(text(ready.root), /Ready to install the OCG provider into the DSH web profile/);
  } finally {
    ready.app.unmount();
  }
});

test("uninstall confirm targets the displayed runtime URL and does not send a Key", async () => {
  let uninstallInput: { profilePath?: string; runtimeUrl?: string | null; expectedFingerprint: string } | undefined;
  const mounted = await mount({
    api: {
      getDshApplication: async () => dshApp({
        status: "installed",
        installed: true,
        uninstallSupported: true,
        runtimeUrl: "http://127.0.0.1:19387",
      }),
      uninstallDshApplication: async (input) => {
        uninstallInput = input;
        return dshApp({ status: "ready", installed: false, uninstallSupported: false });
      },
    },
  });
  try {
    const actions = walkHostNodes(mounted.root).find((node) => node.props.class === "dsh-actions");
    const buttons = actions ? walkHostNodes(actions).filter((node) => node.type === "button") : [];
    assert.equal(buttons.length, 2);
    await (buttons[1]!.props.onClick as () => Promise<void>)();
    await settle();
    const dialog = walkHostNodes(mounted.root).find((node) => node.props.role === "dialog");
    assert.ok(dialog);
    const confirm = walkHostNodes(dialog).find((node) => node.type === "button" && node.props.type === "primary");
    assert.ok(confirm);
    await (confirm!.props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(uninstallInput?.expectedFingerprint, "fp-1");
    assert.equal(uninstallInput?.runtimeUrl, "http://127.0.0.1:19387");
    assert.equal(Object.prototype.hasOwnProperty.call(uninstallInput ?? {}, "keyId"), false);
  } finally {
    mounted.app.unmount();
  }
});

test("HTTP failures, pending restarts and unconfirmed results never show a success toast", async () => {
  for (const application of ["failed", "restart-required", null] as const) {
    const mounted = await mount({ api: {
      getDshApplication: async () => dshApp(),
      installDshApplication: async () => dshApp({ installed: true, application }),
    } });
    try {
      await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
      await settle();
      await (installConfirmButton(mounted.root).props.onClick as () => Promise<void>)();
      await settle();
      const messages = (globalThis as unknown as { __dshMessages: Array<{ type: string }> }).__dshMessages;
      assert.equal(messages.some((entry) => entry.type === "success"), false);
      assert.equal(messages.some((entry) => entry.type === "warning"), true);
    } finally { mounted.app.unmount(); }
  }
});

test("install success is kept when connection reload fails", async () => {
  const mounted = await mount({
    api: {
      getDshApplication: async () => dshApp(),
      installDshApplication: async () => dshApp({ status: "installed", installed: true, enabled: true, application: "applied" }),
    },
  });
  try {
    mounted.connection.reloadError = new Error("reload failed");
    await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    await (installConfirmButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    const messages = (globalThis as unknown as { __dshMessages: Array<{ type: string }> }).__dshMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
    assert.equal(mounted.connection.reloadCount, 1);
  } finally {
    mounted.app.unmount();
  }
});

test("install success closes before a slow connection reload finishes", async () => {
  const pendingReload = deferred<ConnectionState["info"]>();
  const mounted = await mount({
    api: {
      getDshApplication: async () => dshApp(),
      installDshApplication: async () => dshApp({ status: "installed", installed: true, enabled: true, application: "applied" }),
    },
  });
  try {
    mounted.connection.reloadWait = pendingReload.promise;
    await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    await (installConfirmButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), false);
    const messages = (globalThis as unknown as { __dshMessages: Array<{ type: string }> }).__dshMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
    assert.equal(mounted.connection.reloadCount, 1);
    pendingReload.resolve(mounted.connection.info);
    await settle();
  } finally {
    mounted.app.unmount();
  }
});

test("logout during install does not start a connection reload after the receipt", async () => {
  const pendingInstall = deferred<DshApplicationView>();
  const mounted = await mount({
    api: {
      getDshApplication: async () => dshApp(),
      installDshApplication: () => pendingInstall.promise,
    },
  });
  try {
    await (installActionButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    const pendingSave = (installConfirmButton(mounted.root).props.onClick as () => Promise<void>)();
    await settle();
    mounted.connection.sessionEpoch += 1;
    pendingInstall.resolve(dshApp({ status: "installed", installed: true, enabled: true, application: "applied" }));
    await pendingSave;
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), false);
    const messages = (globalThis as unknown as { __dshMessages: Array<{ type: string }> }).__dshMessages;
    assert.equal(messages.some((entry) => entry.type === "success"), true);
    assert.equal(mounted.connection.reloadCount, 0);
  } finally {
    mounted.app.unmount();
  }
});

test("DSH remount restores profile and runtime drafts from the cached app", async () => {
  const customPath = "C:\\Users\\author\\.dsh-editor\\profiles\\dsh-editor";
  const customUrl = "http://127.0.0.1:9999";
  const discoveredProfiles = [
    { home: "C:\\Users\\author\\.dsh", name: "web", path: "C:\\Users\\author\\.dsh\\profiles\\web" },
    { home: "C:\\Users\\author\\.dsh-editor", name: "dsh-editor", path: customPath },
  ];
  let loads = 0;
  const getDshApplication = async (path?: string, runtimeUrl?: string) => {
    loads += 1;
    return dshApp({
      selectedProfilePath: path ?? discoveredProfiles[0]!.path,
      fingerprint: path ? "editor-fingerprint" : "web-fingerprint",
      runtimeUrl: runtimeUrl ?? (path ? customUrl : "http://127.0.0.1:3080"),
      discoveredProfiles,
    });
  };
  const first = await mount({ api: { getDshApplication } });
  try {
    const select = walkHostNodes(first.root).find((node) => node.props.class === "dsh-profile-select");
    assert.ok(select);
    (select!.props["onUpdate:value"] as (value: string) => void)(customPath);
    await settle();
    assert.equal(loads, 2);
    assert.equal(runtimeInput(first.root).props.value, customUrl);
  } finally {
    first.app.unmount();
  }
  const second = await mount({
    api: { getDshApplication },
    pinia: first.pinia,
    connection: first.connection,
  });
  try {
    assert.equal(loads, 2, "cached remount must not inspect again");
    const select = walkHostNodes(second.root).find((node) => node.props.class === "dsh-profile-select");
    assert.equal(select?.props.value, customPath);
    assert.equal(runtimeInput(second.root).props.value, customUrl);
  } finally {
    second.app.unmount();
  }
});

test("unchanged runtime URL blur does not inspect; a changed target still does", async () => {
  let loads = 0;
  const mounted = await mount({
    api: {
      getDshApplication: async (_path, runtimeUrl) => {
        loads += 1;
        return dshApp({ runtimeUrl: runtimeUrl ?? "http://127.0.0.1:3080" });
      },
    },
  });
  try {
    assert.equal(loads, 1);
    const input = runtimeInput(mounted.root);
    assert.notEqual(input.props.tabindex, "-1");
    await (input.props.onBlur as () => void)();
    await settle();
    assert.equal(loads, 1, "unchanged blur must not inspect");
    (input.props["onUpdate:value"] as (value: string) => void)("  http://127.0.0.1:3080  ");
    await settle();
    await (runtimeInput(mounted.root).props.onBlur as () => void)();
    await settle();
    assert.equal(loads, 1, "whitespace-only blur must not inspect");
    (runtimeInput(mounted.root).props["onUpdate:value"] as (value: string) => void)("http://127.0.0.1:9999");
    await settle();
    await (runtimeInput(mounted.root).props.onBlur as () => void)();
    await settle();
    assert.equal(loads, 2, "changed runtime URL still inspects");
    const toolbar = walkHostNodes(mounted.root).find((node) => node.props.class === "dsh-toolbar");
    const refresh = toolbar && walkHostNodes(toolbar).find((node) => node.type === "button");
    assert.ok(refresh);
    await (refresh!.props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(loads, 3, "explicit Refresh still inspects");
  } finally {
    mounted.app.unmount();
  }
});

test("all five tabs switch with pending inspects and explicit Refresh still works", async () => {
  const dshPending = deferred<DshApplicationView>();
  let dshLoads = 0;
  const byokInspects: string[] = [];
  const byokPending = new Map<string, ReturnType<typeof deferred<ByokApplicationView>>>();
  const mounted = await mount({
    shell: true,
    api: {
      getDshApplication: async () => {
        dshLoads += 1;
        if (dshLoads === 1) return dshPending.promise;
        return dshApp({ fingerprint: `fp-${dshLoads}` });
      },
    },
    byokApi: {
      inspect: async (client) => {
        byokInspects.push(client);
        const gate = byokPending.get(client) ?? deferred<ByokApplicationView>();
        byokPending.set(client, gate);
        return gate.promise;
      },
    },
  });
  try {
    const tabs = () => walkHostNodes(mounted.root).filter((node) => node.props.role === "tab");
    assert.equal(tabs().length, 5);
    assert.equal(dshLoads, 1);

    for (const index of [1, 2, 3, 4]) {
      (tabs()[index]!.props.onClick as () => void)();
      await settle();
    }
    assert.deepEqual(byokInspects, ["codex", "kimi", "minimax", "zcode"]);

    (tabs()[0]!.props.onClick as () => void)();
    await settle();
    assert.equal(dshLoads, 1, "return to DSH while pending must not start a second inspect");
    (tabs()[1]!.props.onClick as () => void)();
    await settle();
    assert.equal(byokInspects.filter((id) => id === "codex").length, 1);

    dshPending.resolve(dshApp());
    for (const client of ["codex", "kimi", "minimax", "zcode"]) {
      byokPending.get(client)?.resolve(byokView({ client }));
    }
    await settle();

    (tabs()[0]!.props.onClick as () => void)();
    await settle(40);
    assert.equal(dshLoads, 1, "cached DSH revisit must not inspect again");
    const toolbar = walkHostNodes(mounted.root).find((node) => node.props.class === "dsh-toolbar");
    const refresh = toolbar && walkHostNodes(toolbar).find((node) => node.type === "button");
    assert.ok(refresh);
    await (refresh!.props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(dshLoads, 2, "explicit Refresh inspects again");
  } finally {
    mounted.app.unmount();
  }
});

test("client tabs expose tab semantics and switching syncs the app query", async () => {
  const mounted = await mount({ shell: true, api: { getDshApplication: async () => dshApp() } });
  try {
    const tabs = () => walkHostNodes(mounted.root).filter((node) => node.props.role === "tab");
    assert.equal(tabs().length, 5);
    assert.equal(
      walkHostNodes(mounted.root).find((node) => "data-activation-mode" in node.props)?.props["data-activation-mode"],
      "manual",
    );
    assert.equal(tabs()[0]!.props["data-state"], "active");
    assert.equal(tabs()[0]!.props.tabindex, 0);
    assert.equal(tabs()[1]!.props["data-state"], "inactive");
    assert.equal(tabs()[1]!.props.tabindex, -1);
    (tabs()[1]!.props.onClick as () => void)();
    await settle(40);
    assert.equal(mounted.router.currentRoute.value.query.app, "codex");
    assert.equal(tabs()[1]!.props["data-state"], "active");
    assert.equal(tabs()[1]!.props.tabindex, 0);
    const panels = walkHostNodes(mounted.root).filter((node) => node.props.role === "tabpanel");
    assert.equal(panels.length, 1);
  } finally {
    mounted.app.unmount();
  }
});
