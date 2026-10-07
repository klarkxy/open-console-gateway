import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { getActivePinia } from "pinia";
import { defineComponent, h, ssrContextKey, type App, type Component } from "vue";
import { setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { dropAllSnapshots } from "../stores/persistence.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSessionStore } from "../stores/session.ts";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

const MODEL = "model-luna";
const READ = "F13_READ";

let buildDir = "";
let Editor: Component;
const renderer = createVueHostRenderer();
const storage = memoryStorage();

type Recorded = { url: string; method: string };
type Release = { reject: (error: Error) => void; resolve: () => void };
type Gate = { holdGets: boolean; failGets: boolean; pending: Release[] };
type MessageRecord = { type: string; args: unknown[] };

function memoryStorage(): Storage {
  const data = new Map<string, string>();
  return {
    get length() { return data.size; },
    clear: () => data.clear(),
    getItem: (key) => data.get(key) ?? null,
    key: (index) => [...data.keys()][index] ?? null,
    removeItem: (key) => { data.delete(key); },
    setItem: (key, value) => { data.set(key, String(value)); },
  };
}

function walkSources(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) walkSources(full, out);
    else if (name.endsWith(".vue") || name.endsWith(".ts")) out.push(full);
  }
  return out;
}

function importedNames(specifier: string): string[] {
  const names = new Set<string>();
  const pattern = /import\s+(type\s+)?\{([\s\S]*?)\}\s+from\s+["']([^"']+)["']/g;
  for (const file of walkSources(path.resolve("src"))) {
    const source = readFileSync(file, "utf8");
    for (const match of source.matchAll(pattern)) {
      if (match[1] || match[3] !== specifier) continue;
      for (const part of match[2]!.split(",")) {
        const piece = part.trim();
        if (!piece || piece.startsWith("type ")) continue;
        const original = piece.split(/\s+as\s+/)[0]?.trim();
        if (original) names.add(original);
      }
    }
  }
  return [...names];
}

function naiveSource(names: string[]): string {
  const special: Record<string, string> = {
    NAlert: `export const NAlert = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { role: "alert", type: attrs.type, class: attrs.class }, [attrs.title ?? "", ...Object.values(slots).flatMap((slot) => slot?.() ?? [])]);
    } });`,
    NButton: `export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("button", attrs, slots.default?.());
    } });`,
    NModal: `export const NModal = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => attrs.show === false ? h("div") : h("div", { role: "dialog", class: attrs.class }, [...(slots.default?.() ?? []), ...(slots.footer?.() ?? [])]);
    } });`,
    useMessage: `export function useMessage() {
      const record = (type) => (...args) => { (globalThis.__ocgMessages ??= []).push({ type, args }); };
      return { error: record("error"), success: record("success"), warning: record("warning"), info: record("info") };
    }`,
  };
  const body = names.map((name) => {
    if (special[name]) return special[name];
    if (name.startsWith("use")) return `export function ${name}() { return { value: null }; }`;
    if (name.endsWith("Theme")) return `export const ${name} = {};`;
    return `export const ${name} = pass;`;
  });
  return `
    import { defineComponent, h } from "vue";
    const pass = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", attrs, Object.values(slots).flatMap((slot) => slot?.() ?? []));
    } });
    ${body.join("\n")}
  `;
}

function harnessPlugin(naive: string) {
  return {
    name: "ocg-editor-host",
    enforce: "pre" as const,
    resolveId(source: string) {
      if (source === "naive-ui") return "\0ocg-naive";
      if (source === "@vicons/antd") return "\0ocg-icons";
      return null;
    },
    load(id: string) {
      if (id.includes("type=style")) return "";
      if (id === "\0ocg-naive") return naive;
      if (id === "\0ocg-icons") return `import { defineComponent, h } from "vue"; const icon = defineComponent(() => () => h("i"));`;
      return null;
    },
  };
}

function evidence() {
  return {
    protocol: "chat_completions",
    available: true,
    enabled: true,
    source: "static",
    verifiedAt: null,
    observedAt: null,
    lastProbeResult: null,
    lastProbeAt: null,
    lastProbeError: null,
    override: "auto",
  };
}

function contractsBody(): object {
  return {
    revision: 12,
    processGeneration: 42,
    pricingRevision: "p1",
    customEndpoints: [],
    providers: [{
      scopeKind: "provider",
      scopeId: "opencode",
      providerId: "opencode",
      staticProtocolSnapshotDate: null,
      accounts: [],
      catalog: { source: "static", sourceUrl: "", refreshedAt: null, models: [MODEL], refreshSupported: false },
      models: [{
        alias: MODEL,
        modelId: MODEL,
        preferredProtocol: "chat_completions",
        protocols: { chat_completions: evidence(), responses: null, messages: null },
        routable: true,
        disabledReasons: [],
      }],
      pricing: { availability: "not_applicable" },
      usage: { availability: "unavailable" },
      card: { fetchZenModels: false, discoverModels: false, protocolProbe: true, catalogRefresh: false },
      catalogRoutable: true,
      productionInference: true,
      disabledReasons: [],
      revision: 12,
    }],
  };
}

function cardsBody(): object {
  return {
    cards: [],
    credentials: [],
    destinations: [{
      accountControls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
      adapter: "opencode_go",
      authScheme: "bearer",
      baseUrl: "https://lab.example/v1",
      brandFamily: "OpenCode",
      capabilities: {
        billingTierRequired: false,
        discoverableModels: false,
        externalIntegration: false,
        identityHeaders: false,
        managedSignup: false,
        observer: false,
        officialBalanceProbe: [],
        redirectPolicy: "no_follow",
        testable: true,
      },
      catalog: [{
        enabled: true,
        preferred: "chat_completions",
        protocols: ["chat_completions"],
        publicModel: MODEL,
        upstreamModel: MODEL,
        upstreamOverride: null,
      }],
      enabled: true,
      id: "dest-opencode",
      legacy: { kind: "builtin", id: "opencode" },
      maxCredentials: 1,
      name: "OpenCode",
      observerCredentialId: null,
      plan: null,
      protocols: ["chat_completions"],
      protocolRoutes: [],
    }],
    revision: { revision: 12, processGeneration: 42, pricingRevision: "p1" },
  };
}

function catalogBody(): object {
  return {
    entries: [{
      providerId: "opencode",
      origin: "builtin",
      editable: false,
      deletable: false,
      offering: "api",
      displayName: "OpenCode",
      displayFamily: "OpenCode",
      credentialKind: "api_key",
      quotaScope: "key",
      singleton: false,
      creationAvailability: "unavailable",
      creationUnavailableReason: null,
      verificationPolicy: "not_required",
      verificationRuntimeAvailability: "not_applicable",
      routable: true,
      managedRegistration: false,
      pricingAvailability: "not_applicable",
      usageAvailability: "unavailable",
      manualUsageCalibration: false,
      quotaUnit: "tokens",
      modelSource: "static",
      keyPrefix: null,
      authSchemes: ["bearer"],
      upstreamProtocols: ["chat_completions"],
      formFields: [],
      modelAliases: [],
    }],
    revision: 12,
    processGeneration: 42,
    pricingRevision: "p1",
  };
}

function connectionBody(): object {
  return {
    revision: { revision: 12, processGeneration: 42, pricingRevision: "p1" },
    connections: [],
  };
}

function editorScope(): object {
  return {
    key: "provider:opencode",
    scope_kind: "provider",
    scope_id: "opencode",
    provider_id: "opencode",
    static_protocol_snapshot_date: null,
    label: "OpenCode",
    accounts: [],
    catalog: { source: "static", source_url: "", refreshed_at: null, models: [MODEL], refresh_supported: false },
    models: [{
      alias: MODEL,
      model_id: MODEL,
      preferred_protocol: "chat_completions",
      protocols: {
        chat_completions: {
          protocol: "chat_completions",
          available: true,
          enabled: true,
          source: "static",
          verified_at: null,
          observed_at: null,
          last_probe_result: null,
          last_probe_at: null,
          last_probe_error: null,
          override: "auto",
        },
      },
      routable: true,
      disabled_reasons: [],
    }],
    pricing: { availability: "not_applicable" },
    usage: { availability: "unavailable" },
    card: { fetch_zen_models: false, discover_models: false, protocol_probe: true, catalog_refresh: false },
    catalog_routable: true,
    production_inference: true,
    disabled_reasons: [],
    revision: 12,
  };
}

function pathnameOf(url: string): string {
  const marker = "/dashboard/api/v4";
  const start = url.indexOf(marker);
  const pathName = start >= 0 ? url.slice(start + marker.length) : url;
  return pathName.split("?")[0] ?? pathName;
}

function bodyFor(pathname: string, method: string): object {
  if (pathname === "/auth/status" && method === "GET") {
    return { authenticated: true, initialized: true, local: true, revision: 12, processGeneration: 42 };
  }
  if (pathname === "/routing/cards" && method === "GET") return cardsBody();
  if (pathname === "/provider-contracts" && method === "GET") return contractsBody();
  if (pathname === "/provider-contracts/provider/opencode/catalog/model" && method === "PUT") return contractsBody();
  if (pathname === "/connections" && method === "GET") return connectionBody();
  if (pathname === "/providers" && method === "GET") return catalogBody();
  throw new Error(`unexpected editor request ${method} ${pathname}`);
}

function installDashboard(requests: Recorded[], gate: Gate): void {
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: async (input: string, init: RequestInit = {}) => {
      const url = String(input);
      const method = init.method ?? "GET";
      requests.push({ url, method });
      const pathname = pathnameOf(url);
      if (method === "GET" && gate.holdGets) {
        await new Promise<void>((resolve, reject) => {
          gate.pending.push({ resolve, reject });
        });
      }
      if (method === "GET" && gate.failGets) throw new Error(READ);
      return new Response(JSON.stringify(bodyFor(pathname, method)), {
        headers: { "Content-Type": "application/json" },
      });
    },
  });
}

function prepareWindow(): void {
  const view = installTestWindow({ pathname: "/dashboard/providers", href: "http://127.0.0.1/dashboard/providers" });
  Object.assign(view, { dispatchEvent: () => true, localStorage: storage });
  globalThis.localStorage = storage;
}

function messages(): MessageRecord[] {
  return (globalThis as { __ocgMessages?: MessageRecord[] }).__ocgMessages ?? [];
}

function dialogs(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props.role === "dialog");
}

function saveButton(root: HostNode): HostNode {
  const dialog = dialogs(root)[0];
  if (!dialog) throw new Error(`editor dialog is closed; visible=${text(root).slice(0, 300)}; messages=${JSON.stringify(messages())}`);
  const button = walkHostNodes(dialog).find((node) => node.type === "button" && node.props.type === "primary");
  if (!button) throw new Error("editor save button is missing");
  return button;
}

function puts(requests: Recorded[]): number {
  return requests.filter((request) => request.method === "PUT" && pathnameOf(request.url).endsWith("/catalog/model")).length;
}

function releasePending(gate: Gate, error?: Error): void {
  const pending = gate.pending.splice(0);
  for (const item of pending) {
    if (error) item.reject(error);
    else item.resolve();
  }
}

type EditorExpose = {
  openEditor(modelId: string | null): void;
};

function assertEditorExpose(value: unknown): asserts value is EditorExpose {
  if (typeof value !== "object" || value === null || !("openEditor" in value) || typeof value.openEditor !== "function") {
    throw new Error("editor expose is missing");
  }
}

async function openEditor(gate: Gate, deferRevalidation = false): Promise<{ app: App; root: HostNode; requests: Recorded[]; busy: { current: boolean | null }; receipts: unknown[] }> {
  dropAllSnapshots();
  storage.clear();
  prepareWindow();
  (globalThis as { __ocgMessages?: MessageRecord[] }).__ocgMessages = [];
  setupControlPlane(12, 42);
  const requests: Recorded[] = [];
  installDashboard(requests, gate);
  const pinia = getActivePinia();
  if (!pinia) throw new Error("pinia should be active");
  const session = useSessionStore();
  const destinations = useDestinationsStore();
  const providers = useProvidersStore();
  await session.loadStatus();
  await destinations.load();
  await providers.loadContracts();
  const busy = { current: null as boolean | null };
  const receipts: unknown[] = [];
  let exposed: unknown = null;
  const scope = editorScope();
  const Shell = defineComponent({
    setup() {
      return () => h(Editor, {
        scope, deferRevalidation, onCommitted: (receipt: unknown) => { receipts.push(receipt); },
        "onUpdate:busy": (value: boolean) => { busy.current = value; },
        ref: (value: unknown) => { exposed = value; },
      });
    },
  });
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Shell);
  app.use(pinia);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle();
  assertEditorExpose(exposed);
  exposed.openEditor(MODEL);
  await settle();
  if (dialogs(root).length === 0) {
    throw new Error(`editor did not open; messages=${JSON.stringify(messages())}; requests=${requests.map((request) => `${request.method} ${pathnameOf(request.url)}`).join(",")}`);
  }
  return { app, root, requests, busy, receipts };
}

before(async () => {
  prepareWindow();
  const scratch = path.join(process.cwd(), ".artifacts", "frontend-logic-repair", "provider-tests");
  await mkdir(scratch, { recursive: true });
  buildDir = await mkdtemp(path.join(scratch, "build-editor-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(naiveSource(importedNames("naive-ui"))), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: { entry: path.resolve("src/components/ProviderModelEditor.vue"), fileName: () => "editor.mjs", formats: ["es"] },
      outDir: buildDir,
      rollupOptions: {
        external: ["vue", "pinia", "vue-router"],
        output: { inlineDynamicImports: true },
      },
    },
    esbuild: { target: "esnext" },
  });
  Editor = (await import(pathToFileURL(path.join(buildDir, "editor.mjs")).href)).default;
});

after(async () => {
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

describe("builtin model editor save receipt", { concurrency: false }, () => {
  test("a page-owned editor emits its confirmed receipt without starting legacy inventory revalidation", async () => {
    const gate: Gate = { holdGets: false, failGets: false, pending: [] };
    const mounted = await openEditor(gate, true);
    try {
      const before = mounted.requests.length;
      const save = saveButton(mounted.root).props.onClick as () => Promise<void>; await save(); await settle();
      assert.equal(dialogs(mounted.root).length, 0); assert.equal(mounted.busy.current, false);
      assert.equal(mounted.receipts.length, 1);
      const receipt = mounted.receipts[0] as { kind: string; contracts: { providers: { scope_id: string }[] } };
      assert.equal(receipt.kind, "provider"); assert.equal(receipt.contracts.providers[0]?.scope_id, "opencode");
      assert.equal(mounted.requests.slice(before).filter(request => request.method === "GET").length, 0);
      assert.equal(puts(mounted.requests), 1);
    } finally { mounted.app.unmount(); }
  });
  test("a confirmed builtin model edit closes when the write is acknowledged while follow-up reads are still pending", async () => {
    const gate: Gate = { holdGets: false, failGets: false, pending: [] };
    const mounted = await openEditor(gate);
    try {
      const before = mounted.requests.length;
      gate.holdGets = true;
      const saving = saveButton(mounted.root).props.onClick as () => Promise<void>;
      await saving();
      const followed = mounted.requests.slice(before).filter((request) => request.method === "GET");
      assert.equal(dialogs(mounted.root).length, 0);
      assert.equal(mounted.busy.current, false);
      assert.ok(messages().some((message) => message.type === "success"));
      assert.equal(messages().some((message) => message.type === "error"), false);
      assert.equal(puts(mounted.requests), 1);
      assert.ok(gate.pending.length > 0, `follow-up reads were not still pending: ${followed.map((request) => pathnameOf(request.url)).join(",")}`);
      assert.ok(followed.some((request) => pathnameOf(request.url) === "/routing/cards"));
      releasePending(gate, new Error(READ));
      gate.holdGets = false;
      await settle();
      assert.equal(dialogs(mounted.root).length, 0);
      assert.equal(mounted.busy.current, false);
      assert.equal(messages().some((message) => message.type === "error"), false);
      assert.equal(puts(mounted.requests), 1);
    } finally {
      releasePending(gate, new Error(READ));
      mounted.app.unmount();
    }
  });

  test("an immediate follow-up read failure does not reopen the editor or send a second write", async () => {
    const gate: Gate = { holdGets: false, failGets: false, pending: [] };
    const mounted = await openEditor(gate);
    try {
      gate.failGets = true;
      const saving = saveButton(mounted.root).props.onClick as () => Promise<void>;
      await saving();
      await settle();
      assert.equal(dialogs(mounted.root).length, 0);
      assert.equal(mounted.busy.current, false);
      assert.ok(messages().some((message) => message.type === "success"));
      assert.equal(messages().some((message) => message.type === "error"), false);
      assert.equal(puts(mounted.requests), 1);
    } finally {
      releasePending(gate, new Error(READ));
      mounted.app.unmount();
    }
  });
});
