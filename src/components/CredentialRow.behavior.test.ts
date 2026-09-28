import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { computed, defineComponent, h, shallowRef, ssrContextKey, type Component } from "vue";
import { createVueHostRenderer, settle, type HostNode } from "../test-helpers/vue-host-runtime.ts";

let directory: string;
let CredentialRow: Component;
interface RenderRecord { id: string; surface: string; usage: unknown; loading: unknown; error: unknown; limits: unknown }
const records: RenderRecord[] = [];

before(async () => {
  (globalThis as { __credentialRowRenders?: unknown }).__credentialRowRenders = records;
  const artifacts = path.join(process.cwd(), ".artifacts");
  await mkdir(artifacts, { recursive: true });
  directory = await mkdtemp(path.join(artifacts, "credential-row-test-"));
  const prefix = "\0credential-row-test:";
  await build({ configFile: false, logLevel: "silent", plugins: [{
    name: "credential-row-harness", enforce: "pre",
    resolveId(source, importer) {
      if (source === "naive-ui") return prefix + "naive";
      if (source === "@vicons/antd") return prefix + "icons";
      if (importer?.replaceAll("\\", "/").includes("/src/components/CredentialRow.vue")) {
        if (source.endsWith("useAccountRemoval.ts")) return prefix + "removal";
        if (source.endsWith("/billing.ts") && source.includes("stores")) return prefix + "billing";
        if (source.endsWith("CredentialBody.vue")) return prefix + "body";
        if (source.endsWith("CredentialActions.vue")) return prefix + "actions";
        if (source.endsWith("CredentialTags.vue")) return prefix + "tags";
      }
      return null;
    },
    load(id) {
      if (!id.startsWith(prefix)) return null;
      const kind = id.slice(prefix.length);
      if (kind === "icons") return "export const HolderOutlined = {};";
      if (kind === "billing") return 'import { computed } from "vue"; export const useBillingStore = () => ({ slotFor: () => computed(() => undefined) });';
      if (kind === "removal") return 'import { ref } from "vue"; export const useAccountRemoval = () => ({ deleting: ref({}), confirmDelete() {} });';
      if (kind === "naive") return 'import { defineComponent,h } from "vue"; const stub=defineComponent({setup:(_,ctx)=>()=>h("div",ctx.attrs,ctx.slots.default?.())});export const NTag=stub,NButton=stub,NIcon=stub;';
      if (kind === "tags") return 'import { defineComponent,h } from "vue"; export default defineComponent({setup:()=>()=>h("span")});';
      return 'import { defineComponent,h } from "vue"; export default defineComponent({props:["account","usage","providerUsage","usageLoading","usageLoadError","usageRefreshLoading","limits"],setup:(props)=>()=>{globalThis.__credentialRowRenders.push({id:props.account.id,surface:'+JSON.stringify(kind)+',usage:props.usage,loading:props.usageLoading,error:props.usageLoadError,limits:props.limits});return h("section");}});';
    },
  }, vue()], build: { target: "esnext", outDir: directory, lib: { entry: path.resolve("src/components/CredentialRow.vue"), fileName: () => "row.mjs", formats: ["es"] }, rollupOptions: { external: ["vue"] } } });
  CredentialRow = (await import(pathToFileURL(path.join(directory,"row.mjs")).href)).default;
});
after(async () => { await rm(directory, { recursive: true, force: true }); });

test("row-local usage references update the affected row without re-rendering its parent or siblings", async () => {
  const aUsage = shallowRef({ window_5h: 10 });
  const bUsage = shallowRef({ window_5h: 20 });
  const aLoading = shallowRef(false);
  const aError = shallowRef<string | null>(null);
  const aLimits = shallowRef([{ key: "window_5h", limit: 100 }]);
  const bindings = (id: string, usage: unknown, loading: unknown, error: unknown, limits: unknown) => ({
    credential: { id, name: id, legacy_account_id: id, enabled: true, quota_recovery: null, cooldown_until: null },
    destination: { id: "destination", enabled: true, legacy: { kind: "builtin" } },
    account: { id, name: id, enabled: true, setup_step: "pending_key" },
    catalog: null, usage, providerUsage: null, limits, edits: undefined, now: 0,
    usageLoading: loading, usageLoadError: error, usageRefreshLoading: false,
    purchaseDateSaving: false, menuOptions: [],
  });
  const a = bindings("a", computed(() => aUsage.value), aLoading, aError, aLimits);
  const b = bindings("b", computed(() => bUsage.value), false, null, []);
  let parentRenders = 0;
  const wrapper = defineComponent({ setup: () => () => {
    parentRenders += 1;
    return h("main", [h(CredentialRow, a), h(CredentialRow, b)]);
  } });
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = createVueHostRenderer().createApp(wrapper);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  const parentBefore = parentRenders;
  const siblingBefore = records.filter(row => row.id === "b").length;
  aLoading.value = true;
  await settle();
  aUsage.value = { window_5h: 55 };
  aLoading.value = false;
  aError.value = "offline";
  aLimits.value = [{ key: "window_5h", limit: 200 }];
  await settle();
  assert.equal(parentRenders, parentBefore);
  assert.equal(records.filter(row => row.id === "b").length, siblingBefore);
  const latest = records.filter(row => row.id === "a" && row.surface === "actions").at(-1)!;
  assert.deepEqual(latest.usage, { window_5h: 55 });
  assert.equal(latest.loading, false);
  assert.equal(latest.error, "offline");
  assert.deepEqual(latest.limits, [{ key: "window_5h", limit: 200 }]);
  app.unmount();
});
