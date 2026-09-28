import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { defineComponent, h, ref, ssrContextKey, type App, type Component } from "vue";
import {
  button,
  createVueHostRenderer,
  deferred,
  fireTimers,
  installTestWindow,
  settle,
  walkHostNodes,
  type HostNode,
  type TestWindow,
} from "../test-helpers/vue-host-runtime.ts";

type Gate<T = unknown> = { promise: Promise<T>; resolve: (value: T) => void; reject: (error: unknown) => void };

let buildDir: string;
let DynamicProviderModal: Component;

function harnessPlugin() {
  const prefix = "\0dp-modal-harness:";
  const passThrough = `
    import { defineComponent, h } from "vue";
    export const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
    } });
  `;
  const modules: Record<string, string> = {
    naive: `
      ${passThrough}
      export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("button", attrs, slots.default?.());
      } });
      export const NAlert = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, [attrs.title, ...Object.values(slots).flatMap((slot) => slot?.() ?? [])]);
      } });
      export const NPopconfirm = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
        return () => h("div", attrs, [...(slots.trigger?.() ?? []), ...(slots.default?.() ?? [])]);
      } });
      export const NCheckbox = pass; export const NForm = pass; export const NFormItem = pass;
      export const NIcon = pass; export const NInput = pass; export const NSelect = pass; export const NSpace = pass;
    `,
    icons: `
      import { defineComponent, h } from "vue";
      const icon = defineComponent(() => () => h("i"));
      export const DownOutlined = icon;
      export const RightOutlined = icon;
    `,
    connections: `
      export const connectionsApi = new Proxy({}, { get: (_, key) => (...args) => globalThis.__dpApi.connections[key](...args) });
    `,
    identities: `
      export const identitiesApi = new Proxy({}, { get: (_, key) => (...args) => globalThis.__dpApi.identities[key](...args) });
    `,
    dashboardV3: `
      export class DashboardRequestError extends Error {}
    `,
    providers: `
      export const providerApi = new Proxy({}, { get: (_, key) => (...args) => globalThis.__dpApi.providers[key](...args) });
      export const isRevisionConflict = () => false;
    `,
    controlPlane: `
      export const useControlPlaneStore = () => ({
        hasTokens: () => true,
        refresh: async () => ({ expectedRevision: 1, processGeneration: 1 }),
        expectation: () => ({ expectedRevision: 1, processGeneration: 1 }),
        runMutation: async (run) => run({ expectedRevision: 1, processGeneration: 1 }),
        runLocalMutation: async (target, run) => run({ expectedRevision: 1, processGeneration: 1 }),
      });
    `,
    i18n: `
      import { ref } from "vue";
      export const locale = ref("zh-CN");
      export const t = (key, values = {}) => String(key).replace(/\\{(\\w+)\\}/g, (_, name) => String(values[name] ?? ""));
    `,
    errors: `
      export const dashboardErrorDetail = (error) => error instanceof Error ? error.message : String(error);
    `,
    formSurface: `
      import { defineComponent, h } from "vue";
      export default defineComponent({
        props: ["show", "title", "embedded", "modalClass", "modalStyle", "closeOnEsc"],
        emits: ["update:show"],
        setup(props, { slots }) {
          return () => h("div", { class: "form-surface-stub" }, [
            ...(slots.default?.() ?? []),
            ...(slots.footer?.() ?? []),
          ]);
        },
      });
    `,
  };
  const sources: Record<string, string> = {
    "naive-ui": "naive",
    "@vicons/antd": "icons",
    "../api/connections.ts": "connections",
    "../api/identities.ts": "identities",
    "../api/dashboard-v3.ts": "dashboardV3",
    "../api/providers.ts": "providers",
    "../stores/controlPlane.ts": "controlPlane",
    "../i18n/index.ts": "i18n",
    "../utils/errors.ts": "errors",
    "./FormSurface.vue": "formSurface",
  };
  return {
    name: "dp-modal-harness",
    enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (source === "naive-ui" || source === "@vicons/antd") {
        return `${prefix}${sources[source]}`;
      }
      const importerPath = importer?.replaceAll("\\", "/") ?? "";
      if (!importerPath.includes("/src/components/DynamicProviderModal.vue")) return null;
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/components/DynamicProviderModal.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

const renderer = createVueHostRenderer();

interface Mounted {
  app: App;
  root: HostNode;
  show: { value: boolean };
  provider: { value: Record<string, unknown> | null };
  busyEvents: boolean[];
  committedEvents: Array<Record<string, unknown>>;
  savedEvents: string[];
  showUpdates: boolean[];
  snapshotGates: Array<Gate<object>>;
  discoveryGates: Array<Gate<{ models: string[]; truncated: boolean }>>;
  snapshotSignals: AbortSignal[];
  discoverySignals: AbortSignal[];
  commitGates: Array<Gate<object>>;
  listCalls: number[];
  testWindow: TestWindow;
}

async function mount(): Promise<Mounted> {
  const testWindow = installTestWindow();
  const snapshotGates: Mounted["snapshotGates"] = [];
  const discoveryGates: Mounted["discoveryGates"] = [];
  const snapshotSignals: Mounted["snapshotSignals"] = [];
  const discoverySignals: Mounted["discoverySignals"] = [];
  const commitGates: Mounted["commitGates"] = [];
  const listCalls: number[] = [];
  (globalThis as { __dpApi?: unknown }).__dpApi = {
    connections: {
      listSnapshot: (signal?: AbortSignal) => {
        if (signal) snapshotSignals.push(signal);
        const gate = deferred<object>();
        snapshotGates.push(gate);
        return gate.promise;
      },
      commitOnboarding: () => {
        const gate = deferred<object>();
        commitGates.push(gate);
        return gate.promise;
      },
      // A hung legacy readback must never gate a confirmed commit.
      list: () => {
        listCalls.push(1);
        return new Promise(() => {});
      },
    },
    identities: {
      listSnapshot: async () => ({ identities: [], expectation: { expectedRevision: 1, processGeneration: 1 } }),
    },
    providers: {
      discoverProviderDefinitionModels: (_input: unknown, signal?: AbortSignal) => {
        if (signal) discoverySignals.push(signal);
        const gate = deferred<{ models: string[]; truncated: boolean }>();
        discoveryGates.push(gate);
        return gate.promise;
      },
      testProviderDefinition: async () => ({ ok: true, error: "" }),
    },
  };
  const show = ref(true);
  const provider = ref<Record<string, unknown> | null>(null);
  const busyEvents: boolean[] = [];
  const committedEvents: Mounted["committedEvents"] = [];
  const savedEvents: string[] = [];
  const showUpdates: boolean[] = [];
  const root: HostNode = { children: [], props: {}, type: "root" };
  const wrapper = defineComponent({
    setup: () => () => h(DynamicProviderModal, {
      show: show.value,
      provider: provider.value,
      context: "provider",
      "onUpdate:show": (value: boolean) => { showUpdates.push(value); show.value = value; },
      onCommitted: (result: Record<string, unknown>) => { committedEvents.push(result); },
      onSaved: (providerId: string) => { savedEvents.push(providerId); },
      onBusyChange: (value: boolean) => { busyEvents.push(value); },
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
    provider,
    busyEvents,
    committedEvents,
    savedEvents,
    showUpdates,
    snapshotGates,
    discoveryGates,
    snapshotSignals,
    discoverySignals,
    commitGates,
    listCalls,
    testWindow,
  };
}

function discoverButton(root: HostNode): HostNode {
  return button(root, "获取模型");
}

/** The draft defaults to keyed auth; switching to no-auth unlocks discovery. */
async function selectNoAuth(root: HostNode): Promise<void> {
  const select = walkHostNodes(root).find(
    (node) => node.type === "div" && node.props["aria-label"] === "鉴权方式",
  );
  assert.ok(select, "auth select should render");
  (select.props["onUpdate:value"] as (value: string) => void)("none");
  await settle();
}

async function resolveSnapshot(mounted: Mounted, index: number): Promise<void> {
  mounted.snapshotGates[index]!.resolve({
    connections: [],
    expectation: { expectedRevision: 1, processGeneration: 1 },
  });
  await settle();
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "dp-modal-component-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/components/DynamicProviderModal.vue"),
        fileName: () => "dp-modal.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue"] },
    },
  });
  DynamicProviderModal = (await import(pathToFileURL(path.join(buildDir, "dp-modal.mjs")).href)).default;
});

after(async () => { await rm(buildDir, { force: true, recursive: true }); });

test("closing during discovery is allowed and the late read cannot touch the reopened form", async () => {
  const mounted = await mount();
  assert.equal(mounted.snapshotGates.length, 1);
  await resolveSnapshot(mounted, 0);

  await selectNoAuth(mounted.root);
  const firstDiscover = discoverButton(mounted.root);
  assert.notEqual(firstDiscover.props.disabled, true);
  void (firstDiscover.props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(mounted.discoveryGates.length, 1);
  assert.equal(discoverButton(mounted.root).props.loading, true);
  // A read in flight never signals the host lock (writes only).
  assert.equal(mounted.busyEvents.includes(true), false);

  // Close is available mid-discovery: the cancel control is enabled and the
  // host can simply drop `show`.
  const cancel = button(mounted.root, "取消");
  assert.notEqual(cancel.props.disabled, true);
  (cancel.props.onClick as () => void)();
  await settle();
  assert.equal(mounted.show.value, false);

  // Reopen: a fresh form generation starts its own snapshot and discovery.
  mounted.show.value = true;
  await settle();
  assert.equal(mounted.snapshotGates.length, 2);
  await resolveSnapshot(mounted, 1);
  await selectNoAuth(mounted.root);
  void (discoverButton(mounted.root).props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(mounted.discoveryGates.length, 2);
  assert.equal(discoverButton(mounted.root).props.loading, true);

  // The abandoned first discovery resolves late: its result is ignored and
  // its finally cannot clear the new form's in-flight state.
  mounted.discoveryGates[0]!.resolve({ models: ["stale-a", "stale-b"], truncated: false });
  await settle();
  assert.equal(walkHostNodes(mounted.root).some(node => node.props["aria-label"] === "选择要导入的模型"), false);
  assert.equal(discoverButton(mounted.root).props.loading, true);

  mounted.discoveryGates[1]!.resolve({ models: ["fresh-model"], truncated: false });
  await settle();
  const imported = walkHostNodes(mounted.root).find(node => node.props["aria-label"] === "选择要导入的模型");
  assert.deepEqual((imported?.props.options as Array<{ value: string }>).map(option => option.value), ["fresh-model"]);
  assert.equal(discoverButton(mounted.root).props.loading, false);
  mounted.app.unmount();
});

test("an abandoned snapshot read cannot release the reopened form's read wait", async () => {
  const mounted = await mount();
  assert.equal(mounted.snapshotGates.length, 1);
  await selectNoAuth(mounted.root);
  // The initial snapshot read is pending; close is still allowed.
  const cancel = button(mounted.root, "取消");
  assert.notEqual(cancel.props.disabled, true);
  (cancel.props.onClick as () => void)();
  await settle();
  assert.equal(mounted.show.value, false);

  mounted.show.value = true;
  await settle();
  assert.equal(mounted.snapshotGates.length, 2);
  await selectNoAuth(mounted.root);
  // The reopened form is still waiting on its own snapshot, so the discovery
  // action stays unavailable...
  assert.equal(discoverButton(mounted.root).props.disabled, true);
  // ...and the abandoned first snapshot cannot release that wait.
  mounted.snapshotGates[0]!.resolve({ connections: [], expectation: { expectedRevision: 1, processGeneration: 1 } });
  await settle();
  assert.equal(discoverButton(mounted.root).props.disabled, true);

  await resolveSnapshot(mounted, 1);
  assert.notEqual(discoverButton(mounted.root).props.disabled, true);
  mounted.app.unmount();
});

function fieldAriaLabel(node: HostNode): unknown {
  const inputProps = (node.props["input-props"] ?? node.props.inputProps) as Record<string, unknown> | undefined;
  return node.props["aria-label"] ?? inputProps?.["aria-label"];
}

function setField(root: HostNode, ariaLabel: string, value: string): void {
  const node = walkHostNodes(root).find(
    (entry) => fieldAriaLabel(entry) === ariaLabel && typeof entry.props["onUpdate:value"] === "function",
  );
  assert.ok(node, `field ${ariaLabel} should render`);
  (node.props["onUpdate:value"] as (next: string) => void)(value);
}

function fieldValue(root: HostNode, ariaLabel: string): unknown {
  const node = walkHostNodes(root).find(
    (entry) => fieldAriaLabel(entry) === ariaLabel && "value" in entry.props,
  );
  return node?.props.value;
}

test("a confirmed commit emits its receipt once and closes without any follow-up read", async () => {
  const mounted = await mount();
  assert.equal(mounted.snapshotGates.length, 1);
  await resolveSnapshot(mounted, 0);
  await selectNoAuth(mounted.root);
  setField(mounted.root, "名称", "主号");
  setField(mounted.root, "API 地址", "https://api.example.com");
  setField(mounted.root, "对外模型名", "public-a");
  setField(mounted.root, "上游模型 ID", "upstream-a");
  await settle();

  void (button(mounted.root, "完成设置").props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(mounted.commitGates.length, 1);
  // The in-flight write holds the host lock.
  assert.equal(mounted.busyEvents.at(-1), true);

  mounted.commitGates[0]!.resolve({
    connection_id: "conn-1",
    credential_id: "cred-1",
    account_id: "acc-1",
    replayed: false,
    target_ids: [],
  });
  await settle();

  // The receipt is canonical and emitted exactly once.
  assert.equal(mounted.committedEvents.length, 1);
  assert.deepEqual(mounted.committedEvents[0], {
    connectionId: "conn-1",
    credentialId: "cred-1",
    accountId: "acc-1",
    replayed: false,
    mode: "complete",
  });
  // Closed exactly once, write lock released, and no readback of any kind
  // (list / snapshot) followed the confirmed commit: the hung legacy list was
  // never even called.
  assert.deepEqual(mounted.showUpdates, [false]);
  assert.equal(mounted.show.value, false);
  assert.equal(mounted.busyEvents.at(-1), false);
  assert.equal(mounted.listCalls.length, 0);
  assert.equal(mounted.snapshotGates.length, 1);
  assert.equal(mounted.savedEvents.length, 0);
  mounted.app.unmount();
});

test("a snapshot read that times out aborts the request and releases the form for retry", async () => {
  const mounted = await mount();
  assert.equal(mounted.snapshotGates.length, 1);
  await selectNoAuth(mounted.root);
  // While the snapshot read is pending the fields stay locked...
  assert.equal(discoverButton(mounted.root).props.disabled, true);

  // ...but the wait is bounded: the timer aborts the request and releases the
  // form even though the read never answers.
  await fireTimers(mounted.testWindow);
  await settle();
  assert.equal(mounted.snapshotSignals[0]!.aborted, true);
  assert.equal(discoverButton(mounted.root).props.disabled, false);

  // Retry issues a fresh bounded read and recovers cleanly.
  void (button(mounted.root, "重试").props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(mounted.snapshotGates.length, 2);
  await resolveSnapshot(mounted, 1);
  const alerts = walkHostNodes(mounted.root).filter((node) => node.props.role === "alert");
  assert.equal(alerts.length, 0);
  mounted.app.unmount();
});

test("a context switch without a hide aborts the in-flight read and ignores its late result", async () => {
  const mounted = await mount();
  assert.equal(mounted.snapshotGates.length, 1);
  await resolveSnapshot(mounted, 0);
  await selectNoAuth(mounted.root);
  void (discoverButton(mounted.root).props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(mounted.discoveryGates.length, 1);
  assert.equal(discoverButton(mounted.root).props.loading, true);

  // The host swaps the edit target while the modal stays open: the old
  // context's read is aborted exactly like a close would abort it.
  mounted.provider.value = {
    id: "prov-9",
    name: "switched-target",
    origin: "custom",
    offering: "api",
    editable: true,
    deletable: true,
    endpoint_url: "https://api.switched.example.com",
    upstream_protocol: "chat_completions",
    auth_kind: "none",
    models: [{ public_model: "switched-model", upstream_model: "upstream-switched", upstream_override: null }],
    preset_id: null,
    created_at: "2026-09-28T00:00:00Z",
    revision: 3,
    process_generation: 1,
  };
  await settle();
  assert.equal(mounted.discoverySignals[0]!.aborted, true);
  assert.equal(fieldValue(mounted.root, "名称"), "switched-target");
  assert.notEqual(discoverButton(mounted.root).props.loading, true);

  // The abandoned read resolves late: its result and finally cannot touch the
  // new context (no discovery import affordance appears).
  mounted.discoveryGates[0]!.resolve({ models: ["stale-model-x"], truncated: false });
  await settle();
  const importSelect = walkHostNodes(mounted.root).find(
    (node) => node.props["aria-label"] === "选择要导入的模型",
  );
  assert.equal(importSelect, undefined);
  mounted.app.unmount();
});


test("a submit abandoned during snapshot loading never commits the next form", async () => {
  const mounted = await mount();
  mounted.snapshotGates[0]!.reject(new Error("offline"));
  await settle();
  await selectNoAuth(mounted.root);
  setField(mounted.root, "名称", "old create");
  setField(mounted.root, "API 地址", "https://api.example.com");
  setField(mounted.root, "对外模型名", "public-a");
  setField(mounted.root, "上游模型 ID", "upstream-a");
  await settle();
  const submitting = (button(mounted.root, "完成设置").props.onClick as () => Promise<void>)();
  await settle();
  assert.equal(mounted.snapshotGates.length, 2);
  mounted.provider.value = {
    id: "prov-9",
    name: "switched-target",
    origin: "custom",
    offering: "api",
    editable: true,
    deletable: true,
    endpoint_url: "https://api.switched.example.com",
    upstream_protocol: "chat_completions",
    auth_kind: "none",
    models: [{ public_model: "switched-model", upstream_model: "upstream-switched", upstream_override: null }],
    preset_id: null,
    created_at: "2026-09-28T00:00:00Z",
    revision: 3,
    process_generation: 1,
  };

  await settle();
  assert.equal(mounted.snapshotSignals[1]!.aborted, true);
  mounted.snapshotGates[1]!.reject(new DOMException("Aborted", "AbortError"));
  await submitting;
  await settle();
  assert.equal(mounted.commitGates.length, 0);
  assert.equal(fieldValue(mounted.root, "名称"), "switched-target");
  assert.equal(mounted.committedEvents.length, 0);
  mounted.app.unmount();
});
