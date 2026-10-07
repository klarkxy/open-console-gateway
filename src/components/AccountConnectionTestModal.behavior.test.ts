import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { before, after, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { defineComponent, h, reactive, shallowRef, ssrContextKey, type Component } from "vue";
import { createVueHostRenderer, settle, walkHostNodes, type HostNode, deferred } from "../test-helpers/vue-host-runtime.ts";

let buildDir: string;
let Modal: Component;
const renderer = createVueHostRenderer();

function plugin() {
  const prefix = "\0account-test-harness:";
  const modules: Record<string, string> = {
    naive: `import { defineComponent,h } from 'vue';
      const stub = name => defineComponent({inheritAttrs:false,setup(_, {attrs,slots}){return () => h(name,attrs,[slots.trigger?.(),slots.default?.()]);}});
      export const NAlert=stub('alert'), NButton=stub('button'), NEmpty=stub('empty'), NIcon=stub('icon'), NInput=stub('input'), NModal=stub('modal'), NPagination=stub('pagination'), NPopconfirm=stub('confirm'), NSpin=stub('spin'), NTag=stub('tag'), NTooltip=stub('tooltip');`,
    store: `export const useAccountPageStore = () => globalThis.__accountTestStore;`,
    api: `export const dashboardApi = {testAccountModel: (...args) => globalThis.__accountTestCall(...args)};`,
    i18n: `export const t = key => key;`,
    close: `export const useLocalizedModalCloseLabel = () => {};`,
    icon: `import {defineComponent,h} from 'vue';export const ApiOutlined=defineComponent({setup:()=>()=>h('icon')});`,
    protocol: `export const protocolDisplayName = value => value;`,
    errors: `export const dashboardErrorDetail = value => String(value);`,
  };
  const imports: Record<string, string> = {
    "naive-ui": "naive", "@vicons/antd": "icon", "../stores/accountPage.ts": "store",
    "../api/dashboard.ts": "api", "../i18n/index.ts": "i18n", "../utils/modal-close-label.ts": "close",
    "../domain/provider-contracts.ts": "protocol", "../utils/errors.ts": "errors",
  };
  return {
    name: "account-test-harness", enforce: "pre" as const,
    resolveId(source: string, importer?: string) {
      if (importer?.replaceAll("\\", "/").includes("/src/components/AccountConnectionTestModal.vue")) {
        return imports[source] ? prefix + imports[source] : null;
      }
      return null;
    },
    load(id: string) {
      if (id.includes("AccountConnectionTestModal.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

before(async () => {
  await mkdir(".artifacts", { recursive: true });
  buildDir = await mkdtemp(path.resolve(".artifacts/account-test-component-"));
  await build({ configFile: false, logLevel: "silent", resolve: { preserveSymlinks: true }, plugins: [plugin(), vue()], build: {
    target: "esnext", outDir: buildDir, lib: { entry: path.resolve("src/components/AccountConnectionTestModal.vue"), fileName: () => "modal.mjs", formats: ["es"] },
    rollupOptions: { external: ["vue"] },
  } });
  Modal = (await import(pathToFileURL(path.join(buildDir, "modal.mjs")).href)).default;
}, { timeout: 180_000 });
after(async () => { if (buildDir) await rm(buildDir, { recursive: true, force: true }); });

function mount(store: object, call: (...args: unknown[]) => Promise<unknown>) {
  Object.assign(globalThis, { __accountTestStore: store, __accountTestCall: call });
  const account = shallowRef({ id: "a", name: "A" });
  const show = shallowRef(true);
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(defineComponent({ setup: () => () => h(Modal, { account: account.value, show: show.value, catalog: null }) }));
  app.provide(ssrContextKey, {});
  app.mount(root);
  return { app, root, account, show };
}
const nodes = (root: HostNode, type: string) => walkHostNodes(root).filter(node => node.type === type);
const choice = (id: string) => ({ modelId: id, alias: id, protocol: "messages" });

test("first load shows a spinner and consumes only the selected server candidates", async () => {
  const read = deferred<void>(); const calls: string[] = [];
  const store = reactive({ details: new Map(), detailLoading: {} as Record<string, boolean>, detailErrors: {},
    async loadDetail(id: string) { calls.push(id); store.detailLoading[id] = true; await read.promise; store.details.set(id, { operations: { testModels: [choice("from-server")] } }); store.detailLoading[id] = false; },
  });
  const mounted = mount(store, async () => ({}));
  assert.equal(nodes(mounted.root, "empty").length, 0);
  assert.equal(nodes(mounted.root, "spin")[0]?.props.show, true);
  read.resolve(); await settle();
  assert.deepEqual(calls, ["a"]);
  assert.equal(nodes(mounted.root, "tbody")[0]?.children.filter(node => node.type === "tr").length, 1);
  assert.equal(nodes(mounted.root, "code").length, 0);
  // The store owns logout/session clearing; the modal observes it immediately.
  store.details.clear(); await settle();
  assert.equal(nodes(mounted.root, "tbody")[0]?.children.filter(node => node.type === "tr").length, 0);
  mounted.app.unmount();
});

test("switching account retains server ordering and ignores an old test result", async () => {
  const request = deferred<unknown>(); const tested: unknown[][] = [];
  const store = reactive({ details: new Map([
    ["a", { operations: { testModels: [choice("z"), choice("a")] } }],
    ["b", { operations: { testModels: [choice("b")] } }],
  ]), detailLoading: {}, detailErrors: {}, async loadDetail() {} });
  const mounted = mount(store, async (...args) => { tested.push(args); return request.promise; });
  await settle();
  const rows = nodes(mounted.root, "tbody")[0]!.children.filter(node => node.type === "tr");
  assert.equal(rows.length, 2);
  const testButton = walkHostNodes(rows[0]!).find(node => node.type === "button")!;
  (testButton.props.onClick as () => void)(); await settle();
  assert.deepEqual(tested, [["a", "z"]]);
  mounted.account.value = { id: "b", name: "B" }; await settle();
  request.resolve({ success: true, protocol: "messages", durationMs: 1, httpStatus: 200 }); await settle();
  assert.equal(nodes(mounted.root, "tbody")[0]!.children.filter(node => node.type === "tr").length, 1);
  assert.equal(nodes(mounted.root, "tag").some(node => node.props.type === "success"), false);
  mounted.app.unmount();
});
