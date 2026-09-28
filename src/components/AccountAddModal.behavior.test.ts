import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { defineComponent, h, ref, ssrContextKey, type App, type Component } from "vue";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

let buildDir: string;
let AccountAddModal: Component;

function harnessPlugin() {
  const prefix = "\0aa-modal-harness:";
  const passThrough = `
    import { defineComponent, h } from "vue";
    export const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
    } });
  `;
  const modules: Record<string, string> = {
    naive: `
      ${passThrough}
      export const NModal = defineComponent({ inheritAttrs: false, props: ["show"], setup(props, { attrs, slots }) {
        return () => props.show ? h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? [])) : null;
      } });
      export const NAlert = pass; export const NButton = pass; export const NIcon = pass;
      export const NInput = pass; export const NRadioButton = pass; export const NRadioGroup = pass;
      export const NSelect = pass; export const NSpin = pass; export const NTag = pass; export const NTooltip = pass;
      export const useMessage = () => globalThis.__aamMessage;
    `,
    icons: `
      import { defineComponent, h } from "vue";
      const icon = defineComponent(() => () => h("i"));
      export const KeyOutlined = icon;
      export const CloudOutlined = icon;
      export const ApiOutlined = icon;
      export const DatabaseOutlined = icon;
      export const SwapOutlined = icon;
    `,
    router: `
      export const useRouter = () => globalThis.__aamRouter;
    `,
    i18n: `
      import { ref } from "vue";
      export const locale = ref("zh-CN");
      export const t = (key, values = {}) => String(key).replace(/\\{(\\w+)\\}/g, (_, name) => String(values[name] ?? ""));
    `,
    closeLabel: `
      export const useLocalizedModalCloseLabel = () => {};
    `,
    errors: `
      export const dashboardErrorDetail = (error) => error instanceof Error ? error.message : String(error);
    `,
    providers: `
      export const providerApi = new Proxy({}, { get: (_, key) => (...args) => globalThis.__aamApi.providers[key](...args) });
    `,
    stub: `
      import { defineComponent, h } from "vue";
      export default defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
      } });
    `,
  };
  const sources: Record<string, string> = {
    "naive-ui": "naive",
    "@vicons/antd": "icons",
    "vue-router": "router",
    "../i18n/index.ts": "i18n",
    "../utils/modal-close-label.ts": "closeLabel",
    "../utils/errors.ts": "errors",
    "../api/providers.ts": "providers",
    "./AccountFormModal.vue": "stub",
    "./DynamicProviderModal.vue": "stub",
    "./PlatformAccountFormModal.vue": "stub",
    "./ProviderBrandMark.vue": "stub",
  };
  return {
    name: "aa-modal-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui" || source === "@vicons/antd" || source === "vue-router") {
        return `${prefix}${sources[source]}`;
      }
      const importerPath = importer?.replaceAll("\\", "/") ?? "";
      if (!importerPath.includes("/src/components/AccountAddModal.vue")) return null;
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/components/AccountAddModal.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

const renderer = createVueHostRenderer();

interface RouterCall {
  name?: string;
  query?: Record<string, string>;
}

interface Mounted {
  app: App;
  root: HostNode;
  show: { value: boolean };
  pushCalls: RouterCall[];
  replaceCalls: RouterCall[];
  presetCommittedEvents: Array<Record<string, unknown>>;
  presetSavedEvents: string[];
  updateShowEvents: boolean[];
  messageCounts: { success: number; warning: number; error: number };
}

async function mount(): Promise<Mounted> {
  installTestWindow();
  const pushCalls: RouterCall[] = [];
  const replaceCalls: RouterCall[] = [];
  const messageCounts = { success: 0, warning: 0, error: 0 };
  (globalThis as { __aamRouter?: unknown }).__aamRouter = {
    push: (target: RouterCall) => { pushCalls.push(target); return Promise.resolve(); },
    replace: (target: RouterCall) => { replaceCalls.push(target); return Promise.resolve(); },
  };
  (globalThis as { __aamMessage?: unknown }).__aamMessage = {
    success: () => { messageCounts.success += 1; },
    warning: () => { messageCounts.warning += 1; },
    error: () => { messageCounts.error += 1; },
  };
  (globalThis as { __aamApi?: unknown }).__aamApi = {
    providers: {
      getProviderDefinition: async () => { throw new Error("no dynamic catalog entries in this harness"); },
    },
  };
  const show = ref(true);
  const presetCommittedEvents: Mounted["presetCommittedEvents"] = [];
  const presetSavedEvents: string[] = [];
  const updateShowEvents: boolean[] = [];
  const root: HostNode = { children: [], props: {}, type: "root" };
  const wrapper = defineComponent({
    setup: () => () => h(AccountAddModal, {
      show: show.value,
      catalog: null,
      catalogLoading: false,
      connections: [],
      managedAvailable: false,
      managedReason: "",
      inviteMissing: false,
      createBusy: false,
      platformBusy: false,
      initialOptionId: "manual",
      "onUpdate:show": (value: boolean) => { updateShowEvents.push(value); show.value = value; },
      onPresetCommitted: (result: Record<string, unknown>) => { presetCommittedEvents.push(result); },
      onPresetSaved: (providerId: string) => { presetSavedEvents.push(providerId); },
    }),
  });
  const app = renderer.createApp(wrapper);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  return {
    app,
    root,
    show,
    pushCalls,
    replaceCalls,
    presetCommittedEvents,
    presetSavedEvents,
    updateShowEvents,
    messageCounts,
  };
}

function embeddedDynamicForm(root: HostNode): HostNode {
  const node = walkHostNodes(root).find(
    (entry) => typeof entry.props.onCommitted === "function",
  );
  assert.ok(node, "embedded dynamic-provider form should render");
  return node;
}

function emitCommitted(root: HostNode, result: Record<string, unknown>): void {
  (embeddedDynamicForm(root).props.onCommitted as (value: Record<string, unknown>) => void)(result);
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "aa-modal-component-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/components/AccountAddModal.vue"),
        fileName: () => "aa-modal.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue"] },
    },
  });
  AccountAddModal = (await import(pathToFileURL(path.join(buildDir, "aa-modal.mjs")).href)).default;
});

after(async () => { await rm(buildDir, { force: true, recursive: true }); });

test("a draft commit forwards the receipt and routes to Providers to continue setup", async () => {
  const mounted = await mount();
  const receipt = {
    connectionId: "conn-draft",
    credentialId: null,
    accountId: null,
    replayed: false,
    mode: "draft",
  };
  emitCommitted(mounted.root, receipt);
  await settle();

  // The receipt is forwarded verbatim; the chooser does not close itself or
  // re-read anything — the host owns closing and projection refresh.
  assert.equal(mounted.presetCommittedEvents.length, 1);
  assert.deepEqual(mounted.presetCommittedEvents[0], receipt);
  assert.equal(mounted.updateShowEvents.length, 0);
  assert.equal(mounted.presetSavedEvents.length, 0);
  assert.equal(mounted.messageCounts.success, 1);

  // Draft commits route to the receipt connection on Providers.
  assert.equal(mounted.pushCalls.length, 1);
  assert.equal(mounted.pushCalls[0]!.name, "providers");
  assert.equal(mounted.pushCalls[0]!.query?.connection, "conn-draft");
  mounted.app.unmount();
});

test("a completed commit forwards the receipt without navigating or closing", async () => {
  const mounted = await mount();
  const receipt = {
    connectionId: "conn-full",
    credentialId: "cred-1",
    accountId: "acc-1",
    replayed: false,
    mode: "complete",
  };
  emitCommitted(mounted.root, receipt);
  await settle();

  assert.equal(mounted.presetCommittedEvents.length, 1);
  assert.deepEqual(mounted.presetCommittedEvents[0], receipt);
  // Return navigation and closing are the host's decision (it knows the
  // origin); the chooser stays put and never navigates on a completed add.
  assert.equal(mounted.pushCalls.length, 0);
  assert.equal(mounted.updateShowEvents.length, 0);
  assert.equal(mounted.messageCounts.success, 0);
  assert.equal(mounted.messageCounts.warning, 0);
  mounted.app.unmount();
});
