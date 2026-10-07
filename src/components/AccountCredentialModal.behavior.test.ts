import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { before, after, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { defineComponent, h, reactive, ssrContextKey, type Component } from "vue";
import { createVueHostRenderer, settle, walkHostNodes, type HostNode } from "../test-helpers/vue-host-runtime.ts";

let buildDir: string;
let BindingModal: Component;
let CreateModal: Component;
const renderer = createVueHostRenderer();

function plugin() {
  const prefix = "\0account-credential-harness:";
  const modules: Record<string, string> = {
    naive: `import { defineComponent,h } from 'vue';
      const stub = name => defineComponent({inheritAttrs:false,setup(_, {attrs,slots,expose}){expose({focus(){}});return () => h(name,attrs,[slots.default?.(),slots.footer?.()]);}});
      export const NAlert=stub('alert'), NButton=stub('button'), NCheckbox=stub('checkbox'), NForm=stub('form'), NFormItem=stub('field'), NInput=stub('input'), NRadioButton=stub('radio'), NRadioGroup=stub('radio-group'), NSelect=stub('select'), NSpace=stub('space'), NSwitch=stub('switch');`,
    surface: `import {defineComponent,h} from 'vue';export default defineComponent({inheritAttrs:false,setup(_, {attrs,slots}){return () => h('surface',attrs,[slots.default?.(),slots.footer?.()]);}});`,
    credits: `import {defineComponent,h} from 'vue';export default defineComponent({setup:()=>()=>h('credits')});`,
    i18n: `export const t=key=>key;`,
  };
  return {
    name: "account-credential-harness", enforce: "pre" as const,
    resolveId(source: string) {
      const module = source === "naive-ui" ? "naive" : source === "./FormSurface.vue" ? "surface"
        : source === "./CreditSetupFields.vue" ? "credits" : source.endsWith("/i18n/index.ts") ? "i18n" : null;
      return module ? prefix + module : null;
    },
    load(id: string) {
      if (/Modal.vue\?vue&type=style/.test(id)) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

before(async () => {
  await mkdir(".artifacts", { recursive: true });
  buildDir = await mkdtemp(path.resolve(".artifacts/account-credential-component-"));
  await build({ configFile: false, logLevel: "silent", resolve: { preserveSymlinks: true }, plugins: [plugin(), vue()], build: {
    target: "esnext", outDir: buildDir, lib: { entry: {
      binding: path.resolve("src/components/AccountCredentialModal.vue"), create: path.resolve("src/components/IdentityCredentialCreateModal.vue"),
    }, fileName: (_format, name) => `${name}.mjs`, formats: ["es"] }, rollupOptions: { external: ["vue"] },
  } });
  BindingModal = (await import(pathToFileURL(path.join(buildDir, "binding.mjs")).href)).default;
  CreateModal = (await import(pathToFileURL(path.join(buildDir, "create.mjs")).href)).default;
}, { timeout: 180_000 });
after(async () => { if (buildDir) await rm(buildDir, { recursive: true, force: true }); });

function mount(component: Component, props: Record<string, unknown>) {
  const state = reactive({ show: false, busy: false, unsupportedReason: null, ...props });
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(defineComponent({ setup: () => () => h(component, state) }));
  app.provide(ssrContextKey, {}); app.mount(root); state.show = true;
  return { app, root, state };
}
const nodes = (root: HostNode, type: string) => walkHostNodes(root).filter(node => node.type === type);
const update = (node: HostNode, field: string, value: unknown) => (node.props[`onUpdate:${field}`] as (v: unknown) => void)(value);
const submit = (root: HostNode) => (nodes(root, "button").find(node => node.props.type === "primary")!.props.onClick as () => void)();

test("binding editor consumes server grant selection and sends grants only after explicit draft change", async () => {
  const emitted: unknown[] = [];
  const endpoint = { id: "endpoint", wire_protocol: "chat_completions", locked: false, url: "https://moved.example/v1" };
  const mounted = mount(BindingModal, {
    mode: "binding", binding: { id: "binding", enabled: true, model_scope: { kind: "all" }, allowed_endpoint_ids: ["endpoint"], allowed_origins: ["https://old.example"] },
    connection: { endpoints: [endpoint] }, grantedEndpointIds: [], staleEndpointIds: ["removed"], staleOrigins: ["https://old.example"],
    onSaveBinding: (value: unknown) => emitted.push(value),
  });
  await settle();
  assert.equal(nodes(mounted.root, "checkbox")[0]!.props.checked, false);
  submit(mounted.root); await settle();
  assert.deepEqual(emitted[0], { enabled: true, modelScope: { kind: "all" } });
  update(nodes(mounted.root, "checkbox")[0]!, "checked", true); await settle(); submit(mounted.root);
  assert.deepEqual(emitted[1], { enabled: true, modelScope: { kind: "all" }, allowedEndpointIds: ["endpoint"], allowedOrigins: ["https://moved.example"] });
  mounted.app.unmount();
});

test("create editor displays server choices and quota sharing remains an explicit draft decision", async () => {
  const emitted: unknown[] = [];
  const mounted = mount(CreateModal, {
    defaultConnectionId: "a", connections: [{ id: "a", name: "A" }, { id: "b", name: "B" }],
    shareTargets: [{ id: "same-identity", label: "Sibling" }], onCreate: (value: unknown) => emitted.push(value),
  });
  await settle();
  assert.deepEqual(nodes(mounted.root, "select")[0]!.props.options, [{ value: "a", label: "A" }, { value: "b", label: "B" }]);
  update(nodes(mounted.root, "input").find(node => node.props.type === "password")!, "value", "test-key"); await settle();
  submit(mounted.root); await settle();
  assert.deepEqual((emitted[0] as { quotaSharing: unknown }).quotaSharing, { kind: "independent" });
  update(nodes(mounted.root, "radio-group")[0]!, "value", "shared"); await settle();
  update(nodes(mounted.root, "select")[1]!, "value", "same-identity"); await settle(); submit(mounted.root);
  assert.deepEqual((emitted[1] as { quotaSharing: unknown }).quotaSharing, { kind: "shared", credentialId: "same-identity" });
  mounted.app.unmount();
});
