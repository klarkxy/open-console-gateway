import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { reactive, ssrContextKey, type App, type Component } from "vue";
import { BUILTIN_GOAT_ID } from "../domain/temporary-policy.ts";
import {
  createVueHostRenderer,
  deferred,
  installTestWindow,
  settle,
  walkHostNodes,
  type HostNode,
  type TestWindow,
} from "../test-helpers/vue-host-runtime.ts";

let buildDir: string;
let Section: Component;
const renderer = createVueHostRenderer();

function harnessPlugin() {
  const prefix = "\0temporary-policy-harness:";
  const modules: Record<string, string> = {
    i18n: `export const t = (key, values = {}) => key.replace(/\\{(\\w+)\\}/g, (_, name) => String(values[name] ?? ""));`,
    session: `export const useSessionStore = () => globalThis.__temporaryPolicySession;`,
    destinations: `export const useDestinationsStore = () => globalThis.__temporaryPolicyDestinations;`,
    policy: `export const useTemporaryPolicyStore = () => globalThis.__temporaryPolicyStore;`,
  };
  const sources: Record<string, string> = {
    "../i18n/index.ts": "i18n",
    "../stores/session.ts": "session",
    "../stores/destinations.ts": "destinations",
    "../stores/temporaryPolicy.ts": "policy",
  };
  return {
    name: "temporary-unavailability-harness",
    enforce: "pre" as const,
    resolveId(source: string) {
      const module = sources[source];
      return module ? `${prefix}${module}` : null;
    },
    load(id: string) {
      if (id.includes("/src/components/TemporaryUnavailabilitySection.vue?vue&type=style")) return "";
      return id.startsWith(prefix) ? modules[id.slice(prefix.length)] : null;
    },
  };
}

const sampleRule = {
  kind: "custom" as const,
  id: "custom.lab",
  destinationId: null,
  enabled: true,
  scope: "credential_model" as const,
  match: { statusCodes: [429] },
  backoff: { initialSeconds: 30, maxSeconds: 300 },
};

function config(overrides: Record<string, unknown> = {}) {
  return {
    revision: { revision: 3, processGeneration: 99, pricingRevision: "p" },
    rules: [sampleRule],
    builtins: [{
      id: BUILTIN_GOAT_ID,
      scope: "credential_model",
      backoff: { initialSeconds: 30, maxSeconds: 300 },
    }],
    ...overrides,
  };
}

function restrictions(rows: unknown[] = []) {
  return {
    revision: { revision: 3, processGeneration: 99, pricingRevision: "p" },
    restrictions: rows,
  };
}

function waitingRow() {
  return {
    id: "wait-1",
    ruleId: sampleRule.id,
    ruleGeneration: 1,
    source: "global",
    credentialId: "cred-1",
    destinationId: "dest-1",
    scope: "credential_model",
    upstreamModel: "minimax-m3",
    state: "waiting",
    nextProbeInSeconds: 12,
    probeInFlight: false,
  };
}

function action(root: HostNode, name: string): HostNode {
  const found = walkHostNodes(root).find((node) => node.props["data-action"] === name);
  if (!found) throw new Error(`missing data-action ${name}`);
  return found;
}

function attr(root: HostNode, name: string): unknown {
  return walkHostNodes(root).find((node) => name in node.props)?.props[name];
}

type PolicyStore = {
  configuration: ReturnType<typeof config> | null;
  restrictions: ReturnType<typeof restrictions> | null;
  restrictionsObservedAt: number | null;
  loaded: boolean;
  restrictionsLoaded: boolean;
  loading: boolean;
  restrictionsLoading: boolean;
  mutating: boolean;
  clearing: boolean;
  error: string | null;
  errorDetail: string;
  restrictionsError: string | null;
  restrictionsErrorDetail: string;
  loads: Array<{ retain: boolean }>;
  saves: unknown[];
  expectations: unknown[];
  clears: string[];
  probes: number;
  loadGate: ReturnType<typeof deferred<void>> | null;
  load(retain?: boolean): Promise<void>;
  loadRestrictions(retain?: boolean): Promise<void>;
  saveRules(rules: unknown, captured?: unknown): Promise<void>;
  clearRestriction(id: string): Promise<void>;
};

async function mount(options: {
  configuration?: ReturnType<typeof config> | null;
  restrictions?: ReturnType<typeof restrictions> | null;
  loadHeld?: boolean;
  failRestrictions?: boolean;
} = {}): Promise<{
  app: App;
  root: HostNode;
  store: PolicyStore;
  session: { authenticated: boolean };
  testWindow: TestWindow;
}> {
  const testWindow = installTestWindow({
    href: "http://127.0.0.1/dashboard/?view=settings",
    search: "?view=settings",
    pathname: "/dashboard/",
  });
  const session = reactive({ authenticated: true });
  const store = reactive<PolicyStore>({
    configuration: null,
    restrictions: null,
    restrictionsObservedAt: null,
    loaded: false,
    restrictionsLoaded: false,
    loading: false,
    restrictionsLoading: false,
    mutating: false,
    clearing: false,
    error: null,
    errorDetail: "",
    restrictionsError: null,
    restrictionsErrorDetail: "",
    loads: [],
    saves: [],
    expectations: [],
    clears: [],
    probes: 0,
    loadGate: options.loadHeld ? deferred<void>() : null,
    async load(retain = false) {
      this.loading = true;
      this.restrictionsLoading = true;
      this.loads.push({ retain });
      if (this.loadGate) await this.loadGate.promise;
      if (options.failRestrictions) {
        this.configuration = options.configuration === undefined ? config() : options.configuration;
        this.loaded = this.configuration !== null;
        this.restrictions = null;
        this.restrictionsLoaded = false;
        this.restrictionsError = "load_failed";
        this.loading = false;
        this.restrictionsLoading = false;
        return;
      }
      this.configuration = options.configuration === undefined ? config() : options.configuration;
      this.restrictions = options.restrictions === undefined ? restrictions() : options.restrictions;
      this.restrictionsObservedAt = 1_000;
      this.loaded = this.configuration !== null;
      this.restrictionsLoaded = this.restrictions !== null;
      this.loading = false;
      this.restrictionsLoading = false;
    },
    async loadRestrictions(retain = false) {
      this.loads.push({ retain });
      if (options.failRestrictions) {
        this.restrictionsError = "load_failed";
        return;
      }
      this.restrictions = options.restrictions === undefined ? restrictions() : options.restrictions;
      this.restrictionsObservedAt = 1_000;
      this.restrictionsLoaded = this.restrictions !== null;
    },
    async saveRules(rules: unknown, captured?: unknown) {
      this.expectations.push(captured);
      this.saves.push(rules);
      if (this.configuration) this.configuration = { ...this.configuration, rules: rules as typeof sampleRule[] };
    },
    async clearRestriction(id: string) {
      this.clears.push(id);
      this.restrictions = restrictions();
    },
  });
  const destinations = reactive({
    destinations: [{ id: "dest-1", name: "Lab HTTP" }],
    credentials: [{ id: "cred-1", name: "Office" }],
    loaded: true,
    loading: false,
    async load() {},
  });
  (globalThis as {
    __temporaryPolicyStore?: PolicyStore;
    __temporaryPolicySession?: typeof session;
    __temporaryPolicyDestinations?: typeof destinations;
  }).__temporaryPolicyStore = store;
  (globalThis as { __temporaryPolicySession?: typeof session }).__temporaryPolicySession = session;
  (globalThis as { __temporaryPolicyDestinations?: typeof destinations }).__temporaryPolicyDestinations = destinations;

  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Section);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await settle(20);
  if (!options.loadHeld) {
    for (let index = 0; index < 40 && (store.loading || store.restrictionsLoading); index += 1) {
      await settle(4);
    }
  }
  return { app, root, store, session, testWindow };
}

before(async () => {
  const artifactsDir = path.join(process.cwd(), ".artifacts");
  await mkdir(artifactsDir, { recursive: true });
  buildDir = await mkdtemp(path.join(artifactsDir, "temporary-unavailability-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(), vue()],
    build: {
      emptyOutDir: true,
      lib: {
        entry: path.resolve("src/components/TemporaryUnavailabilitySection.vue"),
        fileName: () => "temporary-unavailability.mjs",
        formats: ["es"],
      },
      outDir: buildDir,
      rollupOptions: { external: ["vue"] },
    },
  });
  Section = (await import(pathToFileURL(path.join(buildDir, "temporary-unavailability.mjs")).href)).default;
});

after(async () => {
  await rm(buildDir, { force: true, recursive: true });
});

test("empty restriction tables expose no_local_waits and never a health marker", async () => {
  const mounted = await mount({ restrictions: restrictions() });
  try {
    assert.equal(attr(mounted.root, "data-empty-code"), "no_local_waits");
    assert.equal(
      walkHostNodes(mounted.root).some((node) => "data-health" in node.props || node.props["data-empty-code"] === "healthy"),
      false,
    );
  } finally {
    mounted.app.unmount();
  }
});

test("an initial restrictions GET failure does not claim no_local_waits", async () => {
  const mounted = await mount({ failRestrictions: true });
  try {
    assert.equal(attr(mounted.root, "data-restrictions-loaded"), "false");
    assert.equal(attr(mounted.root, "data-restrictions-error-code"), "load_failed");
    assert.equal(attr(mounted.root, "data-empty-code"), undefined);
    assert.equal(attr(mounted.root, "data-restriction-count"), undefined);
    assert.equal(action(mounted.root, "refresh-restrictions").type, "button");
  } finally {
    mounted.app.unmount();
  }
});

test("a successful empty restrictions snapshot may show no_local_waits", async () => {
  const mounted = await mount({ restrictions: restrictions([]) });
  try {
    assert.equal(attr(mounted.root, "data-restrictions-loaded"), "true");
    assert.equal(attr(mounted.root, "data-empty-code"), "no_local_waits");
    assert.equal(attr(mounted.root, "data-restrictions-error-code"), undefined);
  } finally {
    mounted.app.unmount();
  }
});

test("a failed restrictions revalidation keeps the previous snapshot", async () => {
  const mounted = await mount({
    restrictions: restrictions([waitingRow()]),
  });
  try {
    assert.equal(attr(mounted.root, "data-restriction-count"), 1);
    mounted.store.restrictionsError = "load_failed";
    mounted.store.restrictionsLoading = false;
    await settle();
    assert.equal(attr(mounted.root, "data-restrictions-loaded"), "true");
    assert.equal(attr(mounted.root, "data-restriction-count"), 1);
    assert.equal(attr(mounted.root, "data-empty-code"), undefined);
    assert.equal(attr(mounted.root, "data-restrictions-error-code"), "load_failed");
  } finally {
    mounted.app.unmount();
  }
});

test("a failed revalidation of an empty snapshot keeps no_local_waits", async () => {
  const mounted = await mount({ restrictions: restrictions([]) });
  try {
    assert.equal(attr(mounted.root, "data-empty-code"), "no_local_waits");
    mounted.store.restrictionsError = "load_failed";
    await settle();
    assert.equal(attr(mounted.root, "data-empty-code"), "no_local_waits");
    assert.equal(attr(mounted.root, "data-restrictions-error-code"), "load_failed");
    assert.equal(attr(mounted.root, "data-restriction-count"), undefined);
  } finally {
    mounted.app.unmount();
  }
});

test("clear-local-wait records the restriction id and does not send a probe", async () => {
  const mounted = await mount({
    restrictions: restrictions([waitingRow()]),
  });
  try {
    await (action(mounted.root, "clear-local-wait").props.onClick as () => Promise<void>)();
    await settle();
    assert.deepEqual(mounted.store.clears, ["wait-1"]);
    assert.equal(mounted.store.probes, 0);
  } finally {
    mounted.app.unmount();
  }
});

test("invalid custom drafts show issues without hiding a valid stored rule", async () => {
  const mounted = await mount();
  try {
    assert.equal(attr(mounted.root, "data-custom-count"), 1);
    const form = action(mounted.root, "custom-rule-form");
    await (form.props.onSubmit as (event: { preventDefault(): void }) => Promise<void>)({
      preventDefault() {},
    });
    await settle();
    assert.equal(attr(mounted.root, "data-issue-count") !== undefined, true);
    assert.equal(attr(mounted.root, "data-custom-count"), 1);
  } finally {
    mounted.app.unmount();
  }
});

test("CAS conflict stays visible with the last successful rules", async () => {
  const mounted = await mount();
  try {
    mounted.store.error = "conflict";
    await settle();
    assert.equal(attr(mounted.root, "data-error-code"), "conflict");
    assert.equal(attr(mounted.root, "data-custom-count"), 1);
    assert.equal(attr(mounted.root, "data-loaded"), "true");
  } finally {
    mounted.app.unmount();
  }
});

test("revalidation keeps rendered configuration while a later load is outstanding", async () => {
  const mounted = await mount({ loadHeld: true });
  try {
    assert.equal(attr(mounted.root, "data-loaded"), "false");
    mounted.store.loadGate?.resolve();
    await settle(20);
    assert.equal(attr(mounted.root, "data-custom-count"), 1);
    mounted.store.loadGate = deferred<void>();
    const pending = mounted.store.load(true);
    mounted.store.loading = true;
    await settle();
    assert.equal(attr(mounted.root, "data-custom-count"), 1);
    assert.equal(attr(mounted.root, "data-loaded"), "true");
    mounted.store.loadGate.resolve();
    await pending;
  } finally {
    mounted.app.unmount();
  }
});

test("the local wait interval is created on mount and removed on unmount", async () => {
  const mounted = await mount();
  try {
    assert.equal(
      [...mounted.testWindow.__timers.values()].some((timer) => timer.kind === "interval"),
      true,
    );
  } finally {
    mounted.app.unmount();
  }
  assert.equal(mounted.testWindow.__timers.size, 0);
});

test("logout stops the local wait ticker without leaving an interval", async () => {
  const mounted = await mount();
  try {
    assert.equal(mounted.testWindow.__timers.size > 0, true);
    mounted.session.authenticated = false;
    await settle();
    assert.equal(mounted.testWindow.__timers.size, 0);
  } finally {
    mounted.app.unmount();
  }
});

test("builtin disable saves an override and does not call a probe helper", async () => {
  const mounted = await mount();
  try {
    const box = action(mounted.root, "toggle-builtin");
    await (box.props.onChange as (event: { target: { checked: boolean } }) => Promise<void>)({
      target: { checked: false },
    });
    await settle();
    assert.equal(mounted.store.saves.length, 1);
    assert.equal(mounted.store.probes, 0);
    const saved = mounted.store.saves[0] as Array<{ kind: string; enabled: boolean }>;
    assert.equal(saved.some((rule) => rule.kind === "builtin_override" && rule.enabled === false), true);
  } finally {
    mounted.app.unmount();
  }
});


test("an open rule editor retains its original revision across revalidation", async () => {
  const mounted = await mount();
  try {
    (action(mounted.root, "edit-custom-rule").props.onClick as () => void)();
    await settle();
    mounted.store.configuration = config({ revision: { revision: 8, processGeneration: 99 } });
    await settle();
    await (action(mounted.root, "custom-rule-form").props.onSubmit as (event: unknown) => Promise<void>)({ preventDefault() {} });
    assert.deepEqual(mounted.store.expectations, [{ expectedRevision: 3, processGeneration: 99 }]);
  } finally {
    mounted.app.unmount();
  }
});
