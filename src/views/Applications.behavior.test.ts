import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { reactive, ssrContextKey, type App, type Component } from "vue";
import type { DshApplication } from "../api/generated/dashboard-v4.ts";
import {
  button,
  createVueHostRenderer,
  deferred,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

type DshApi = {
  getDshApplication: () => Promise<DshApplication>;
  installDshApplication: (
    input: { keyId: string; expectedFingerprint: string },
    expectation: { expectedRevision: number; processGeneration: number },
  ) => Promise<DshApplication>;
};

type ConnectionState = {
  info: { primary_key: string; sub_keys: Array<{ id: string; name: string; enabled: boolean; value: string }> } | null;
  load: () => Promise<ConnectionState["info"]>;
};

let buildDir: string;
let Applications: Component;
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
      export const NRadio = defineComponent({ inheritAttrs: false, props: { value: [String, Number, Boolean] }, setup(props, { attrs, slots }) {
        return () => h("label", attrs, slots.default?.());
      } });
      export const NRadioGroup = pass;
      export const NSpin = pass;
      export const NTabPane = pass;
      export const NTabs = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, [slots.suffix?.(), slots.default?.()]);
      } });
      export const NTag = pass;
      export const useMessage = () => {
        const record = (type) => (...args) => { (globalThis.__dshMessages ??= []).push({ type, args }); };
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
      globalThis.__DshDashboardRequestError = DashboardRequestError;
      export function isRevisionConflict(error) {
        return Boolean(error && error.status === 409 && error.code === "revisionConflict");
      }
    `,
    dashboardV3: `export const PRIMARY_KEY_ID = "00000000-0000-0000-0000-000000000001";`,
    api: `
      export const dashboardV4 = new Proxy({}, { get: (_, key) => (...args) => globalThis.__dshComponentApi[key](...args) });
    `,
    connection: `export const useConnectionStore = () => globalThis.__dshConnectionStore;`,
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
  const sources: Record<string, string> = {
    "naive-ui": "naive",
    "../api/dashboard.ts": "dashboard",
    "../api/dashboard-v3.ts": "dashboardV3",
    "../api/dashboard-v4.ts": "api",
    "../stores/connection.ts": "connection",
    "../stores/controlPlane.ts": "store",
    "../i18n/index.ts": "i18n",
    "../utils/errors.ts": "errors",
    "../utils/modal-close-label.ts": "modal",
  };
  return {
    name: "dsh-applications-harness",
    enforce: "pre" as const,
    resolveId(source: string) {
      if (source === "naive-ui") return `${prefix}naive`;
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/views/Applications.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

function dshApp(overrides: Partial<DshApplication> = {}): DshApplication {
  return {
    status: "ready",
    detected: true,
    installed: false,
    installSupported: true,
    activationRequired: false,
    version: "0.1.5-rc.2",
    detail: "Ready to install the OCG provider into the DSH web profile",
    targetPaths: ["C:\\\\ocg\\\\applications\\\\dsh"],
    fingerprint: "fp-1",
    revision: { revision: 7, processGeneration: 3, pricingRevision: "p" },
    ...overrides,
  };
}

function enabledConnection(): ConnectionState["info"] {
  return {
    primary_key: "ocg-primary-secret",
    sub_keys: [],
  };
}

function requestError(message: string, status: number): Error {
  const Ctor = (globalThis as unknown as {
    __DshDashboardRequestError: new (message: string, status: number) => Error;
  }).__DshDashboardRequestError;
  return new Ctor(message, status);
}

async function mount(options: {
  api: Partial<DshApi> & Pick<DshApi, "getDshApplication">;
  connection?: ConnectionState;
}): Promise<{ app: App; root: HostNode; connection: ConnectionState }> {
  installTestWindow({
    href: "http://127.0.0.1/dashboard/?view=applications",
    search: "?view=applications",
  });
  const connection = reactive(options.connection ?? {
    info: enabledConnection(),
    async load() {
      return this.info;
    },
  }) as ConnectionState;
  (globalThis as unknown as { __dshConnectionStore?: ConnectionState }).__dshConnectionStore = connection;
  (globalThis as unknown as { __dshMessages?: Array<{ type: string; args: unknown[] }> }).__dshMessages = [];
  const api: DshApi = {
    installDshApplication: async () => {
      throw new Error("installDshApplication not stubbed");
    },
    ...options.api,
  };
  (globalThis as { __dshComponentApi?: DshApi }).__dshComponentApi = api;
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Applications);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  return { app, root, connection };
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
        entry: path.resolve("src/views/Applications.vue"),
        fileName: () => "applications.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue"] },
    },
  });
  Applications = (await import(pathToFileURL(path.join(buildDir, "applications.mjs")).href)).default;
});

after(async () => {
  await rm(buildDir, { force: true, recursive: true });
});

test("opening install without an enabled Key shows an error and keeps the dialog closed", async () => {
  const mounted = await mount({
    api: { getDshApplication: async () => dshApp() },
    connection: {
      info: { primary_key: "", sub_keys: [] },
      async load() {
        return this.info;
      },
    },
  });
  try {
    await (button(mounted.root, "安装 DSH").props.onClick as () => Promise<void>)();
    await settle();
    assert.match(text(mounted.root), /没有可用于 DSH 的已启用 Key。/);
    assert.equal(
      walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"),
      false,
    );
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
    await (button(mounted.root, "安装 DSH").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), true);
    await (button(mounted.root, "确认安装").props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(walkHostNodes(mounted.root).some((node) => node.props.role === "dialog"), false);
    assert.match(text(mounted.root), /DSH 状态已变化，已刷新当前状态。/);
    assert.match(text(mounted.root), /DSH has a same-name package that is not an OCG-managed source/);
    assert.equal(loads, 2);
    assert.equal(button(mounted.root, "安装 DSH").props.disabled, true);
  } finally {
    mounted.app.unmount();
  }
});

test("confirm stays disabled without a usable Key and does not install", async () => {
  let installs = 0;
  const mounted = await mount({
    api: {
      getDshApplication: async () => dshApp(),
      installDshApplication: async () => {
        installs += 1;
        return dshApp({ status: "installed", installed: true });
      },
    },
  });
  try {
    await (button(mounted.root, "安装 DSH").props.onClick as () => Promise<void>)();
    await settle();
    // openInstall requires a Key to show Confirm; drop every usable Key after.
    mounted.connection.info = {
      primary_key: "",
      sub_keys: [{ id: "sub-disabled", name: "off", enabled: false, value: "secret" }],
    };
    await settle();
    const confirm = button(mounted.root, "确认安装");
    assert.equal(confirm.props.disabled, true);
    await (confirm.props.onClick as () => Promise<void>)();
    await settle();
    assert.equal(installs, 0);
  } finally {
    mounted.app.unmount();
  }
});

test("a repeated click while installing is ignored when a Key is selected", async () => {
  const pending = deferred<DshApplication>();
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
    await (button(mounted.root, "安装 DSH").props.onClick as () => Promise<void>)();
    await settle();
    const confirm = button(mounted.root, "确认安装");
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
    assert.equal(button(conflict.root, "安装 DSH").props.disabled, true);
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
    assert.equal(button(incompatible.root, "安装 DSH").props.disabled, true);
    assert.match(text(incompatible.root), /DSH 0\.1\.4 is not a supported 0\.1\.5-rc\.1 or 0\.1\.5-rc\.2 build/);
  } finally {
    incompatible.app.unmount();
  }

  const ready = await mount({
    api: { getDshApplication: async () => dshApp() },
  });
  try {
    assert.equal(button(ready.root, "安装 DSH").props.disabled, false);
    assert.doesNotMatch(text(ready.root), /Ready to install the OCG provider into the DSH web profile/);
  } finally {
    ready.app.unmount();
  }
});
