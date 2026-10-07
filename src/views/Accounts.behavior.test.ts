import assert from "node:assert/strict";
import { before, test } from "node:test";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createPinia, setActivePinia } from "pinia";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { createMemoryHistory, createRouter } from "vue-router";
import { defineComponent, h, provide, inject, ssrContextKey, type App } from "vue";
import type { Connection } from "../api/connections.ts";
import type { Identity } from "../api/identities.ts";
import type { AccountDetail } from "../api/pages.ts";
import type { PlatformAccount, PlatformAccountsView, PlatformLink } from "../api/platform-accounts.ts";
import { accountInferenceEndpointUrl } from "../domain/upstream-balance.ts";
import {
  createVueHostRenderer,
  deferred,
  installTestWindow,
  settle,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

const INVITE = "https://opencode.ai/go?ref=68XPB6NP8V";
const renderer = createVueHostRenderer();
const radioKey = Symbol("ocg-radio");

type PageKind = "ready" | "google" | "verify";
type AsyncFn = (...args: unknown[]) => unknown;
type PageMessage = { type: string };
type TextInputHandler = (value: string) => void;

interface AccountViewHelpers {
  identityForCard(id: string): Identity | null;
  connectionsForAccount(id: string): Connection[];
  platformLinkForAccount(id: string): PlatformLink | null;
  platformParentForAccount(id: string): PlatformAccount | null;
}

interface OcgUiRegistry {
  component(name: string): unknown;
  namespace(): unknown;
}

function assertTextInput(value: unknown, detail: string): asserts value is TextInputHandler {
  if (typeof value !== "function") throw new Error(detail);
}

function readPurchaseDate(update: unknown): string {
  if (typeof update !== "object" || update === null || !("purchase_date" in update)) return "";
  const value = update.purchase_date;
  return value == null ? "" : String(value);
}

let Accounts: new () => unknown;
let useAccountPageStore: () => { loadDetail: (id: string) => Promise<AccountDetail>; details: ReadonlyMap<string, AccountDetail> };
let useProvidersStore: () => { loadConnections: () => Promise<Connection[]> };
let useIdentitiesStore: () => { loadPresented: () => Promise<Identity[]> };
let acceptPlatformView: (view: PlatformAccountsView) => void;
let useSessionStore: () => { dropSession: () => void };
let useAccountsStore: () => { accounts: Array<{ id: string; name: string; purchase_date: string | null }>; loaded: boolean };
let useDestinationsStore: () => { destinations: unknown[]; loaded: boolean };
let usePlatformAccountsStore: () => { load: () => Promise<unknown> };
let useSettingsStore: () => { loadPresented: () => Promise<unknown> };
let useControlPlaneStore: () => { sync: (receipt: { revision: number; processGeneration: number }) => void };
let dropAllSnapshots: () => void;
let DashboardConflictError: new (message: string, revision: number | null, generation: number | null) => Error;
let accountReads = 0;
let pageReads = 0;
let accountDetails = 0;
let billingCalls = 0;
const pendingAccountReads: Array<ReturnType<typeof deferred<unknown>>> = [];
const renderErrors: string[] = [];
const floating: Promise<unknown>[] = [];
const handlers: Record<string, AsyncFn> = {};

function installLocalStorage(): void {
  const backing = new Map<string, string>();
  const storage: Storage = {
    get length() { return backing.size; },
    clear: () => backing.clear(),
    getItem: (key) => backing.get(key) ?? null,
    key: (index) => [...backing.keys()][index] ?? null,
    removeItem: (key) => { backing.delete(key); },
    setItem: (key, value) => { backing.set(key, String(value)); },
  };
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storage });
}

function installDocument(): void {
  const document = {
    visibilityState: "visible",
    hidden: false,
    documentElement: { lang: "", style: {} },
    body: { style: {} },
    addEventListener() {},
    removeEventListener() {},
    head: { appendChild() {}, removeChild() {} },
    getElementById() { return null; },
    querySelector() { return null; },
    querySelectorAll() { return []; },
    createElement() {
      return { setAttribute() {}, click() {}, style: {}, appendChild() {} };
    },
  };
  Object.defineProperty(globalThis, "document", { configurable: true, value: document });
}

function pageMessages(): PageMessage[] {
  const host = globalThis as { __ocgPageMessages?: PageMessage[] };
  host.__ocgPageMessages ??= [];
  return host.__ocgPageMessages;
}

function installUiKit(): void {
  const pass = defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("div", attrs, slots.default?.());
    },
  });
  const motion = new Proxy(pass, { get: () => pass });
  const known = new Map<string, unknown>();
  const remember = (name: string, value: unknown) => {
    known.set(name, value);
    return value;
  };
  const generic = (name: string) => defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("div", { ...attrs, "data-ui": name }, ["default", "trigger", "header", "footer", "icon", "extra", "action", "title"]
        .flatMap((slot) => slots[slot]?.() ?? []));
    },
  });
  const NButton = defineComponent({
    inheritAttrs: false,
    props: ["disabled", "loading", "type"],
    setup(props, { attrs, slots }) {
      return () => h("button", {
        ...attrs,
        type: "button",
        class: attrs.class,
        disabled: props.disabled || props.loading ? true : undefined,
        "data-loading": props.loading ? "true" : "false",
        "data-variant": props.type ?? "",
        onClick: () => {
          if (props.disabled || props.loading) return undefined;
          const click = attrs.onClick as (() => unknown) | undefined;
          return typeof click === "function" ? click() : undefined;
        },
      }, slots.default?.());
    },
  });
  const NSwitch = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled"],
    emits: ["update:value"],
    setup(props, { attrs, emit }) {
      return () => h("button", {
        ...attrs,
        type: "button",
        role: "switch",
        class: attrs.class,
        "aria-checked": props.value ? "true" : "false",
        disabled: props.disabled ? true : undefined,
        onClick: () => {
          if (props.disabled) return;
          emit("update:value", !props.value);
        },
      });
    },
  });
  const NDropdown = defineComponent({
    inheritAttrs: false,
    props: ["options"],
    emits: ["select"],
    setup(props, { attrs, slots, emit }) {
      return () => h("div", { ...attrs, class: attrs.class }, [
        slots.default?.(),
        ...((props.options as Array<{ key?: unknown; disabled?: boolean }> | undefined) ?? []).map((option) => h("button", {
          type: "button",
          "data-menu-key": String(option.key),
          disabled: option.disabled ? true : undefined,
          onClick: () => {
            if (!option.disabled) emit("select", option.key);
          },
        })),
      ]);
    },
  });
  const NModal = defineComponent({
    inheritAttrs: false,
    props: ["show"],
    setup(props, { attrs, slots }) {
      return () => props.show === false ? null : h("div", { role: "dialog", class: attrs.class }, [
        slots.header?.(),
        slots.default?.(),
        slots.footer?.(),
      ]);
    },
  });
  const NPopover = defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots }) {
      return () => h("div", { ...attrs, class: attrs.class }, [slots.trigger?.(), slots.default?.()]);
    },
  });
  const NInput = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled"],
    emits: ["update:value"],
    setup(props, { attrs, emit }) {
      return () => h("input", {
        ...attrs,
        class: attrs.class,
        value: props.value ?? "",
        disabled: props.disabled ? true : undefined,
        onInput: (payload: unknown) => {
          const value = typeof payload === "string"
            ? payload
            : (payload as { target?: { value?: string } } | null)?.target?.value ?? "";
          emit("update:value", value);
        },
      });
    },
  });
  const NCheckbox = defineComponent({
    inheritAttrs: false,
    props: ["checked", "disabled"],
    emits: ["update:checked"],
    setup(props, { attrs, emit }) {
      return () => h("button", {
        ...attrs,
        type: "button",
        role: "checkbox",
        class: attrs.class,
        "aria-checked": props.checked ? "true" : "false",
        disabled: props.disabled ? true : undefined,
        onClick: () => {
          if (!props.disabled) emit("update:checked", !props.checked);
        },
      });
    },
  });
  const NRadioGroup = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled"],
    emits: ["update:value"],
    setup(props, { attrs, slots, emit }) {
      provide(radioKey, {
        value: () => props.value,
        select: (value: unknown) => emit("update:value", value),
      });
      return () => h("div", { ...attrs, role: "radiogroup", class: attrs.class }, slots.default?.());
    },
  });
  const NRadioButton = defineComponent({
    inheritAttrs: false,
    props: ["value", "disabled"],
    setup(props, { attrs, slots }) {
      const group = inject<{ value: () => unknown; select: (value: unknown) => void } | null>(radioKey, null);
      return () => h("button", {
        ...attrs,
        type: "button",
        class: attrs.class,
        "data-choice": String(props.value),
        "aria-pressed": group?.value() === props.value ? "true" : "false",
        disabled: props.disabled ? true : undefined,
        onClick: () => {
          if (!props.disabled) group?.select(props.value);
        },
      }, slots.default?.());
    },
  });
  const NForm = defineComponent({
    inheritAttrs: false,
    setup(_props, { attrs, slots, expose }) {
      expose({ validate: async () => undefined });
      return () => h("form", {
        ...attrs,
        class: attrs.class,
        onSubmit: (event: { preventDefault?: () => void }) => {
          event?.preventDefault?.();
          const submit = attrs.onSubmit as ((event: unknown) => unknown) | undefined;
          return typeof submit === "function" ? submit(event) : undefined;
        },
      }, slots.default?.());
    },
  });
  const useMessage = () => {
    const record = (type: string) => () => { pageMessages().push({ type }); };
    return { success: record("success"), warning: record("warning"), error: record("error"), info: record("info") };
  };
  const useDialog = () => ({
    warning: (options: { onPositiveClick?: () => unknown }) => {
      try {
        const result = options?.onPositiveClick?.();
        if (result && typeof (result as Promise<unknown>).then === "function") {
          void (result as Promise<unknown>).catch(() => undefined);
        }
      } catch {
        // The page owns the thrown mutation; the dialog stub only starts it.
      }
    },
  });
  const specials: Record<string, unknown> = {
    NButton, NSwitch, NDropdown, NModal, NPopover, NTooltip: NPopover, NInput, NCheckbox,
    NRadioGroup, NRadioButton, NRadio: NRadioButton, NForm, useMessage, useDialog,
    motion, MotionConfig: pass, darkTheme: {}, lightTheme: {}, useOsTheme: () => null,
  };
  const registry: OcgUiRegistry = {
    component(name: string) {
      if (known.has(name)) return known.get(name);
      return remember(name, specials[name] ?? generic(name));
    },
    namespace() {
      return new Proxy({}, { get: (_target, key) => registry.component(String(key)) });
    },
  };
  Object.assign(globalThis, { __ocgUi: registry });
}

function uiKind(source: string): string | null {
  if (source === "naive-ui" || (source.startsWith("naive-ui/") && !source.includes("/locales/"))) return "naive";
  if (source === "@vicons/antd" || source.startsWith("@vicons/antd/")) return "icon";
  if (source === "motion-v") return "motion";
  return null;
}

function uiBindings(clause: string): string | null {
  const trimmed = clause.trim();
  if (!trimmed) return "";
  const star = trimmed.match(/^\*\s+as\s+([A-Za-z_$][\w$]*)$/);
  if (star) return `const ${star[1]} = globalThis.__ocgUi.namespace();`;
  let rest = trimmed;
  const lines: string[] = [];
  const fallback = rest.match(/^([A-Za-z_$][\w$]*)\s*(?:,\s*)?/);
  if (fallback && !rest.startsWith("{") && !rest.startsWith("type") && !rest.startsWith("*")) {
    lines.push(`const ${fallback[1]} = globalThis.__ocgUi.component("default");`);
    rest = rest.slice(fallback[0].length).trim();
  }
  if (!rest) return lines.join("\n");
  if (!rest.startsWith("{")) return null;
  const body = rest.slice(1, rest.lastIndexOf("}"));
  for (const part of body.split(",")) {
    const piece = part.trim();
    if (!piece) continue;
    const typed = piece.match(/^(type\s+)?([A-Za-z_$][\w$]*)(?:\s+as\s+([A-Za-z_$][\w$]*))?$/);
    if (!typed) return null;
    if (typed[1]) continue;
    lines.push(`const ${typed[3] ?? typed[2]} = globalThis.__ocgUi.component(${JSON.stringify(typed[2])});`);
  }
  return lines.join("\n");
}

function uiStubPlugin() {
  const pattern = /import\s+(type\s+)?([\s\S]*?)\s+from\s+["']([^"']+)["']/g;
  return {
    name: "ocg-account-ui-stub",
    enforce: "pre" as const,
    transform(code: string, id: string) {
      const normalized = id.replaceAll("\\", "/");
      if (!normalized.includes("/src/") || normalized.includes("/node_modules/")) return null;
      if (normalized.endsWith("/src/test-helpers/accounts-entry.ts")) code += '\nexport { pagesApi } from "../api/pages.ts";\nexport { useAccountPageStore } from "../stores/accountPage.ts";\nexport { useSettingsStore } from "../stores/settings.ts";\nexport { useControlPlaneStore } from "../stores/controlPlane.ts";\nexport { usePlatformAccountsStore } from "../stores/platformAccounts.ts";';
      let changed = normalized.endsWith("/src/test-helpers/accounts-entry.ts");
      if (normalized.endsWith("/src/views/Accounts.vue")) {
        code = code.replace("</script>", "\ndefineExpose({ identityForCard, connectionsForAccount, platformLinkForAccount, platformParentForAccount });\n</script>");
        changed = true;
      }
      if (normalized.endsWith("/src/test-helpers/accounts-entry.ts")) code += '\nexport { useProvidersStore } from "../stores/providers.ts";\nexport { useIdentitiesStore } from "../stores/identities.ts";';
      const next = code.replace(pattern, (full, typeOnly: string | undefined, clause: string, source: string) => {
        if (!uiKind(source)) return full;
        changed = true;
        if (typeOnly) return "";
        return uiBindings(clause) ?? full;
      });
      return changed ? next : null;
    },
    load(id: string) {
      const normalized = id.replaceAll("\\", "/");
      if (normalized.includes("type=style") || normalized.endsWith(".css") || normalized.includes(".css?")) return "export default {}";
      return null;
    },
  };
}

function classTokens(value: unknown): string[] {
  if (typeof value === "string") return value.split(/\s+/).filter(Boolean);
  if (Array.isArray(value)) return value.flatMap(classTokens);
  if (value && typeof value === "object") {
    return Object.entries(value as Record<string, unknown>).filter(([, on]) => Boolean(on)).map(([key]) => key);
  }
  return [];
}

function hasClass(node: HostNode, className: string): boolean {
  return classTokens(node.props.class).includes(className);
}

function byClass(root: HostNode, className: string): HostNode[] {
  return walkHostNodes(root).filter((node) => hasClass(node, className));
}

function buttonsUnder(node: HostNode): HostNode[] {
  return walkHostNodes(node).filter((child) => child.type === "button");
}

function outline(root: HostNode): string {
  return walkHostNodes(root).slice(0, 60).map((node) => {
    const classes = classTokens(node.props.class).join(".");
    const mark = node.props["data-menu-key"] ?? node.props.id ?? node.props.role ?? "";
    return `${node.type}${classes ? `.${classes}` : ""}${mark ? `#${mark}` : ""}`;
  }).join(" | ");
}

function fire(node: HostNode): void {
  const click = node.props.onClick;
  if (typeof click !== "function") throw new Error("fixture: control has no click handler");
  const result = click();
  if (result && typeof (result as Promise<unknown>).then === "function") {
    floating.push((result as Promise<unknown>).catch((error: unknown) => {
      renderErrors.push(error instanceof Error ? error.message : String(error));
    }));
  }
}

async function waitFor(label: string, predicate: () => boolean, root?: HostNode): Promise<void> {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    if (predicate()) return;
    await settle(4);
  }
  throw new Error(`fixture: timed out waiting for ${label}${root ? `\n${outline(root)}\n${renderErrors.join("\n")}` : ""}`);
}

function account(overrides: Record<string, unknown> = {}) {
  return {
    id: "acc-1",
    name: "Synthetic",
    username: "",
    password: "",
    key: "",
    enabled: true,
    account_type: "key",
    setup_step: "ready",
    provider_id: "opencode",
    credential_kind: "api_key",
    quota_scope: "key",
    purchase_date: "2020-01-01",
    expires_on: "2020-02-01",
    cooldown_until: "2099-01-01T00:00:00Z",
    cooldown_generic_until: null,
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: null,
    last_error: null,
    auth_error: null,
    notes: "",
    usage_sync_last_success_at: null,
    usage_sync_next_allowed_at: null,
    verification_status: "not_required",
    connection_verified_at: null,
    verification_error: null,
    plan_routable: true,
    custom_config: null,
    model_capabilities: [],
    created_at: "2020-01-01T00:00:00Z",
    updated_at: "2020-01-01T00:00:00Z",
    ...overrides,
  };
}

function scenarioAccount(kind: PageKind) {
  if (kind === "google") {
    return account({ account_type: "managed", setup_step: "google_account", purchase_date: "", expires_on: "", cooldown_until: null });
  }
  if (kind === "verify") {
    return account({ account_type: "managed", setup_step: "key_verification", purchase_date: "", expires_on: "", cooldown_until: null });
  }
  return account();
}

const catalogEntry = {
  provider_id: "opencode",
  origin: "builtin",
  editable: false,
  deletable: false,
  offering: "plan",
  display_name: "OpenCode Go",
  display_family: "OpenCode",
  credential_kind: "api_key",
  quota_scope: "key",
  singleton: false,
  creation_availability: "available",
  creation_unavailable_reason: null,
  verification_policy: "required",
  verification_runtime_availability: "available",
  routable: true,
  managed_registration: true,
  usage_availability: "available",
  manual_usage_calibration: false,
  quota_unit: "tokens",
  model_source: "opencode_get_models",
  key_prefix: null,
  auth_schemes: ["bearer"],
  upstream_protocols: ["chat_completions"],
  form_fields: [
    { id: "name", kind: "text", required: true, immutable_after_create: false },
    { id: "key", kind: "secret", required: true, immutable_after_create: false },
    { id: "purchase_date", kind: "date", required: false, immutable_after_create: false },
  ],
  model_aliases: [],
};

function settings() {
  return {
    revision: 1,
    process_generation: 1,
    gateway_port: 9042,
    gateway_port_from_env: false,
    proxy_mode: "auto",
    proxy_url: "",
    proxy_list_direction: "whitelist",
    proxy_list_models: [],
    proxy_supported_models: [],
    opencode_invite_url: INVITE,
    client_root_url: "https://client.example.test",
    client_root_url_from_env: false,
    auto_start: false,
    auto_start_supported: false,
    show_dock_icon: false,
    dock_visibility_supported: false,
    connect_timeout_secs: 10,
    non_stream_timeout_secs: 60,
    stream_idle_timeout_secs: 300,
    routing_mode: "strict-priority",
    conversation_sticky: true,
  };
}

function projection(row = scenarioAccount("ready")) {
  const destination = {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: "opencode", browserProfile: true },
    adapter: "http",
    legacy: { kind: "builtin", id: "dest-1" },
    auth_scheme: "bearer",
    base_url: null,
    brand_family: null,
    capabilities: {
      billing_tier_required: false,
      discoverable_models: false,
      external_integration: false,
      identity_headers: false,
      managed_signup: true,
      observer: false,
      official_balance_probe: [],
      redirect_policy: "no_follow",
      testable: true,
    },
    catalog: [],
    enabled: true,
    id: "dest-1",
    max_credentials: null,
    name: "OpenCode",
    observer_credential_id: null,
    plan: { expiry_cadence: "monthly", manual_calibration: false, usage_source: "none", windows: [{ kind: "month" }] },
    protocols: ["chat_completions"],
  };
  const credential = {
    auth_state: "unknown",
    cooldowns: { five_hour_until: null, free_until: null, generic_until: null, month_until: null, week_until: null },
    destination_id: "dest-1",
    enabled: true,
    grants: { allowed_endpoint_ids: [], allowed_origins: [] },
    has_secret: true,
    id: "cred-acc-1",
    last_error: null,
    legacy_account_id: row.id,
    name: row.name,
    notes: null,
    onboarding_task: null,
    purchase_date: row.purchase_date,
    quota_pool_id: null,
    quota_recovery: null,
    routing_rank: 0,
    scope: { kind: "all" },
  };
  return {
    cards: [{ id: "card-1", destination_id: "dest-1", credential_ids: [credential.id] }],
    destinations: [destination],
    credentials: [credential],
    expectation: { expectedRevision: 1, processGeneration: 1 },
  };
}

function emptyPlatformView() {
  return { revision: 1, processGeneration: 1, accounts: [], links: [] };
}

function installHandlers(kind: PageKind, deferLaterAccountReads: boolean): void {
  const row = scenarioAccount(kind);
  accountReads = 0;
  pageReads = 0;
  accountDetails = 0;
  billingCalls = 0;
  pendingAccountReads.splice(0, pendingAccountReads.length);
  Object.keys(handlers).forEach((key) => { delete handlers[key]; });
  const reject = (name: string) => async () => { throw new Error(`fixture: unexpected ${name}`); };
  handlers.accountsPage = async () => {
    pageReads++;
    return accountsPage(kind);
  };
  handlers.accountDetail = async () => {
    accountDetails++;
    const full = projection(scenarioAccount(kind));
    return { revision: { revision: 1, processGeneration: 1, pricingRevision: "fixture" }, account: scenarioAccount(kind),
      destination: full.destinations[0], credential: full.credentials[0], identity: null, connection: null, platform: null, platformLink: null };
  };
  handlers.getAccounts = () => {
    accountReads += 1;
    if (deferLaterAccountReads && accountReads > 1) {
      const gate = deferred<unknown>();
      pendingAccountReads.push(gate);
      return gate.promise;
    }
    return Promise.resolve([scenarioAccount(kind)]);
  };
  handlers.getSettings = async () => settings();
  handlers.getBrowserCapabilities = async () => ({ mode: "native", reason: null });
  handlers.updateSettings = async () => settings();
  handlers.patchSettings = reject("patchSettings");
  handlers.accountRefresh = reject("accountRefresh");
  handlers.updateAccount = reject("updateAccount");
  handlers.toggleAccount = reject("toggleAccount");
  handlers.resetAccountCooldown = reject("resetAccountCooldown");
  handlers.createManagedAccount = reject("createManagedAccount");
  handlers.advanceAccountSetup = reject("advanceAccountSetup");
  handlers.verifyManagedAccountKey = reject("verifyManagedAccountKey");
  handlers.resetAccountBrowserProfile = reject("resetAccountBrowserProfile");
  handlers.getProviderCatalog = async () => [catalogEntry];
  handlers.accountsSnapshot = async () => ({ accounts: await handlers.getAccounts(), expectation: { expectedRevision: 1, processGeneration: 1 } });
  handlers.catalogSnapshot = async () => ({ catalog: await handlers.getProviderCatalog(), expectation: { expectedRevision: 1, processGeneration: 1 } });
  handlers.connectionsSnapshot = async () => ({ connections: await handlers.connections(), expectation: { expectedRevision: 1, processGeneration: 1 } });
  handlers.listSnapshot = async () => projection(row);
  handlers.identitySnapshot = async () => ({ identities: [], expectation: { expectedRevision: 1, processGeneration: 1 } });
  handlers.connections = async () => [];
  handlers.platformList = async () => emptyPlatformView();
  handlers.billingStatus = async () => { throw new Error("billing offline"); };
}

function bind(api: Record<string, unknown>, key: string, handlerKey = key): void {
  api[key] = (...args: unknown[]) => {
    const handler = handlers[handlerKey];
    if (!handler) throw new Error(`fixture: unmocked ${handlerKey}`);
    return handler(...args);
  };
}

function messageCount(type: string): number {
  return pageMessages().filter((message) => message.type === type).length;
}

function storedAccount(id = "acc-1") {
  return useAccountsStore().accounts.find((row) => row.id === id) ?? null;
}

function snapshotPresent(): boolean {
  return globalThis.localStorage.getItem("ocg.snapshot.v1:accounts") !== null;
}

async function mount(kind: PageKind, deferLaterAccountReads = false, pageHandler?: AsyncFn, beforeMount?: () => Promise<void>): Promise<{ app: App; root: HostNode; router: ReturnType<typeof createRouter>; helpers: AccountViewHelpers }> {
  installHandlers(kind, deferLaterAccountReads);
  if (pageHandler) handlers.accountsPage = pageHandler;
  pageMessages().splice(0, pageMessages().length);
  renderErrors.splice(0, renderErrors.length);
  dropAllSnapshots();
  setActivePinia(createPinia());
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: "/", name: "accounts", component: defineComponent({ setup: () => () => h("div") }) },
      { path: "/providers", name: "providers", component: defineComponent({ setup: () => () => h("div") }) },
      { path: "/cpa", name: "cpa", component: defineComponent({ setup: () => () => h("div") }) },
    ],
  });
  await router.push({ name: "accounts" });
  await router.isReady();
  const pinia = createPinia();
  setActivePinia(pinia);
  await beforeMount?.();
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Accounts as never);
  app.use(pinia);
  app.use(router);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.config.errorHandler = (error) => {
    renderErrors.push(error instanceof Error ? error.stack ?? error.message : String(error));
  };
  const helpers = app.mount(root) as unknown as AccountViewHelpers;
  await waitFor("credential row", () => byClass(root, "credential-row").length > 0, root);
  await waitFor("page snapshot", () => pageReads >= 1);
  await settle(8);
  if (renderErrors.length > 0) throw new Error(`fixture: page render failed\n${renderErrors.join("\n")}`);
  pageMessages().splice(0, pageMessages().length);
  return { app, root, router, helpers };
}

function row(root: HostNode): HostNode {
  const found = byClass(root, "credential-row")[0];
  if (!found) throw new Error("fixture: credential row should stay mounted");
  return found;
}

function purchaseActions(root: HostNode): HostNode {
  const found = byClass(root, "purchase-date-popover__actions")[0];
  if (!found) throw new Error(`fixture: purchase-date actions should render\n${outline(root)}`);
  return found;
}

async function openWizard(root: HostNode): Promise<void> {
  const pending = byClass(root, "managed-pending")[0];
  if (!pending) throw new Error(`fixture: managed continuation should render\n${outline(root)}`);
  const opener = buttonsUnder(pending).find((button) => button.props["data-variant"] === "primary") ?? buttonsUnder(pending)[0];
  if (!opener) throw new Error("fixture: managed continuation has no button");
  fire(opener);
  await waitFor("managed wizard actions", () => byClass(root, "managed-wizard__actions").length > 0, root);
}

function wizardPrimary(root: HostNode): HostNode {
  const actions = byClass(root, "managed-wizard__actions")[0];
  if (!actions) throw new Error("fixture: wizard actions should render");
  const primary = buttonsUnder(actions).find((button) => button.props["data-variant"] === "primary");
  if (!primary) throw new Error("fixture: wizard primary action should render");
  return primary;
}

async function teardownOutcome(): Promise<{ accounts: string[]; success: number; warning: number; error: number; snapshot: boolean; destinations: number }> {
  await settle(8);
  return {
    accounts: useAccountsStore().accounts.map((item) => item.id),
    success: messageCount("success"),
    warning: messageCount("warning"),
    error: messageCount("error"),
    snapshot: snapshotPresent(),
    destinations: useDestinationsStore().destinations.length,
  };
}

before(async () => {
  installLocalStorage();
  installDocument();
  const testWindow = installTestWindow();
  Object.assign(testWindow, { localStorage: globalThis.localStorage, document: globalThis.document, navigator: globalThis.navigator });
  globalThis.localStorage.setItem("ocg-manager.locale", "zh-CN");
  globalThis.fetch = async (input: unknown) => {
    throw new Error(`accounts behavior test blocked a network fetch ${String(input)}`);
  };
  installUiKit();
  const outDir = path.resolve(".artifacts/frontend-logic-repair/account-tests/accounts-client");
  await build({
    configFile: false,
    root: process.cwd(),
    logLevel: "error",
    cacheDir: path.resolve(".artifacts/frontend-logic-repair/account-tests/accounts-vite-cache"),
    plugins: [uiStubPlugin(), vue()],
    resolve: { alias: { "@": path.resolve("src") } },
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: {
        entry: path.resolve("src/test-helpers/accounts-entry.ts"),
        fileName: () => "bundle.mjs",
        formats: ["es"],
      },
      minify: false,
      outDir,
      rollupOptions: {
        external: ["vue", "pinia", "vue-router"],
        output: { inlineDynamicImports: true },
      },
    },
  });
  const client = await import(pathToFileURL(path.join(outDir, "bundle.mjs")).href);
  Accounts = client.Accounts;
  DashboardConflictError = client.DashboardConflictError;
  useSessionStore = client.useSessionStore;
  useAccountsStore = client.useAccountsStore;
  useDestinationsStore = client.useDestinationsStore;
  usePlatformAccountsStore = client.usePlatformAccountsStore;
  useAccountPageStore = client.useAccountPageStore;
  useProvidersStore = client.useProvidersStore;
  useIdentitiesStore = client.useIdentitiesStore;
  acceptPlatformView = view => client.usePlatformAccountsStore().acceptView(view);
  useSettingsStore = client.useSettingsStore;
  useControlPlaneStore = client.useControlPlaneStore;
  dropAllSnapshots = client.dropAllSnapshots;
  for (const key of ["getAccounts", "getSettings", "getBrowserCapabilities", "updateSettings", "patchSettings", "updateAccount", "toggleAccount", "resetAccountCooldown", "createManagedAccount", "advanceAccountSetup", "verifyManagedAccountKey", "resetAccountBrowserProfile"]) {
    bind(client.dashboardApi, key);
  }
  bind(client.providerApi, "getProviderCatalog");
  bind(client.dashboardApi, "getAccountsSnapshot", "accountsSnapshot");
  bind(client.providerApi, "getProviderCatalogSnapshot", "catalogSnapshot");
  bind(client.routingCardsApi, "listSnapshot");
  bind(client.identitiesApi, "listSnapshot", "identitySnapshot");
  bind(client.connectionsApi, "list", "connections");
  bind(client.connectionsApi, "listSnapshot", "connectionsSnapshot");
  bind(client.platformAccountsApi, "list", "platformList");
  bind(client.billingApi, "status", "billingStatus");
  bind(client.pagesApi, "accounts", "accountsPage");
  bind(client.pagesApi, "accountDetail", "accountDetail");
  bind(client.pagesApi, "refreshAccount", "accountRefresh");
  bind(client.pagesApi, "accountCredentials", "accountCredentials");
}, { timeout: 180000 });

function accountsPage(kind: PageKind) {
  const full = scenarioAccount(kind);
  const projected = projection(full);
  const camel = (value: Record<string, unknown>) => Object.fromEntries(Object.entries(value).map(([key, entry]) =>
    [key.replace(/_([a-z0-9])/g, (_match, letter: string) => letter.toUpperCase()), entry]));
  const { key: _key, password: _password, custom_config: _config, model_capabilities: _models, ...scalars } = full;
  const credential = projected.credentials[0]!;
  const { catalog: _catalog, ...destination } = projected.destinations[0]!;
  return { revision: { revision: 1, processGeneration: 1, pricingRevision: "fixture" }, readVersion: "fixture-1", asOf: "2026-10-07T00:00:00Z",
    validUntil: null, totalCards: 1, totalCredentials: 1, matchedCards: 1, matchedCredentials: 1, offset: 0, limit: 10, hasMore: false, errors: [],
    planOptions: [{ value: "opencode", label: "OpenCode Go", cardCount: 1, credentialCount: 1 }], planFilters: [], routingMode: "strict-priority", conversationSticky: true,
    cards: [{ cardId: "card-1", position: 0, destination: { ...camel(destination), catalogCount: 0, enabledCatalogCount: 0 },
      platform: null, totalCredentials: 1, matchedCredentials: 1, rowsOffset: 0, rowsHasMore: false, credentialCreate: null, actions: [], availability: "available",
      rows: [{ account: { ...camel(scalars), modelCapabilityCount: 0 }, credential: { ...camel(credential), scope: { kind: "all", modelCount: 0, singleModel: null } },
        platformLink: null, status: kind === "ready" ? "cooling" : "registering", routeAvailable: kind === "ready", modelCount: 0, inferenceEndpointUrl: null, billing: null,
        refresh: { supported: true, observedAt: null, freshUntil: null, nextAllowedAt: null },
        tags: { credentialCount: 1, bindingDisabled: false, quotaShareName: null, quotaShareCount: 0, duplicateName: false },
        actions: ["toggle", "purchase-date", "reset", "reset-profile", "continue-setup"].map(key => ({ key, allowed: true, reason: null })) }] }] };
}

test("default Accounts arrival renders the Rust summary without complete resource or billing reads", { timeout: 20000 }, async () => {
  const mounted = await mount("ready");
  try { assert.deepEqual({ pageReads, accountReads, accountDetails, billingCalls, rows: byClass(mounted.root, "credential-row").length },
    { pageReads: 1, accountReads: 0, accountDetails: 0, billingCalls: 0, rows: 1 }); }
  finally { mounted.app.unmount(); dropAllSnapshots(); }
});

test("a plain HTTP row editor opens and saves its receipt while an unrelated platform read remains held", { timeout: 20000 }, async () => {
  const heldPlatform = deferred<unknown>(); let platformReads = 0; let platformPending = false;
  let platformFlight: Promise<unknown> | undefined; let writes = 0;
  const plain = account({ name: "Lab account 001", provider_id: "custom", purchase_date: "", expires_on: "", cooldown_until: null,
    custom_config: { account_id: "acc-1", endpoint_url: "https://lab.example.test", upstream_protocol: "chat_completions",
      created_at: "2020-01-01T00:00:00Z", updated_at: "2020-01-01T00:00:00Z" } });
  const resources = projection(plain);
  const destination = { ...resources.destinations[0], legacy: { kind: "custom_account", id: plain.id }, base_url: "https://lab.example.test",
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false }, plan: null };
  const mounted = await mount("ready", false, async () => {
    pageReads++; const base = accountsPage("ready");
    return { ...base, cards: base.cards.map(card => ({ ...card, rows: card.rows.map(row => ({ ...row,
      account: { ...row.account, name: plain.name, providerId: "custom", purchaseDate: "", expiresOn: "", cooldownUntil: null },
      credential: { ...row.credential, name: plain.name }, actions: [{ key: "edit", allowed: true, reason: null }] })) })) };
  }, async () => {
    handlers.platformList = () => {
      platformReads++; platformPending = true;
      return heldPlatform.promise.finally(() => { platformPending = false; });
    };
    platformFlight = usePlatformAccountsStore().load();
    handlers.accountDetail = async () => {
      accountDetails++; return { revision: { revision: 1, processGeneration: 1, pricingRevision: "fixture" }, account: plain,
        destination, credential: resources.credentials[0], identity: null, connection: null, platform: null, platformLink: null };
    };
    handlers.getProviderCatalog = async () => [{ ...catalogEntry, provider_id: "custom", display_name: "Custom API",
      managed_registration: false, usage_availability: "unavailable", model_source: "account_config",
      form_fields: [{ id: "name", kind: "text", required: true, immutable_after_create: false },
        { id: "key", kind: "secret", required: true, immutable_after_create: false },
        { id: "notes", kind: "text", required: false, immutable_after_create: false }] }];
    handlers.billingStatus = async () => ({ accountId: plain.id, cash: null, configurableCredits: false, credits: null, manualCalibration: false,
      model: "quota", officialRefresh: false, presets: [], processGeneration: 1, revision: 1, source: "unavailable", unit: "tokens", usage: null });
    handlers.updateAccount = async (id, update) => {
      assert.equal(id, plain.id);
      assert.equal((update as { name?: string }).name, "Renamed Lab");
      writes++; return { ...plain, name: "Renamed Lab", updated_at: "2026-10-07T01:00:00Z" };
    };
  });
  try {
    const edit = walkHostNodes(row(mounted.root)).find(node => node.props["data-menu-key"] === "edit");
    if (!edit) throw new Error("fixture: plain row should expose its edit action");
    fire(edit);
    await waitFor("plain HTTP editor", () => byClass(mounted.root, "account-modal").length === 1, mounted.root);
    assert.equal(accountDetails, 1); assert.equal(accountReads, 0);
    assert.equal(useAccountsStore().loaded, false); assert.equal(useDestinationsStore().loaded, false);
    assert.equal(platformReads, 1); assert.equal(platformPending, true);
    const editor = byClass(mounted.root, "account-modal")[0]!;
    const name = walkHostNodes(editor).find(node => node.type === "input" && node.props.value === plain.name)!;
    const change = name.props.onInput;
    assertTextInput(change, "fixture: plain editor name should have an input handler"); change("Renamed Lab");
    await settle();
    const save = buttonsUnder(editor).find(button => button.props["data-variant"] === "primary")!;
    await waitFor("plain editor save readiness", () => !save.props.disabled, mounted.root);
    fire(save);
    await waitFor("plain editor write receipt", () => writes === 1 && byClass(mounted.root, "account-modal").length === 0, mounted.root);
    assert.equal(storedAccount()?.name, "Renamed Lab"); assert.equal(messageCount("success"), 1);
    assert.equal(platformReads, 1); assert.equal(platformPending, true);
  } finally {
    heldPlatform.resolve(emptyPlatformView()); await platformFlight; await settle(); mounted.app.unmount(); dropAllSnapshots();
  }
});

test("newer complete connection and platform receipts supersede cached detail while pending reads retain it", { timeout: 20000 }, async () => {
  const connection = (url: string): Connection => ({ id: "c", name: "Selected", origin: "custom", template_ref: null,
    adapter_kind: "http", lifecycle: "configured", authorization: "valid", eligibility: { state: "eligible", reason: "none" },
    credential_count: 1, enabled_credential_count: 1, target_count: 0, targets: [], display_family: null, offering: "api",
    legacy: { kind: "builtin_provider", id: "opencode" },
    endpoints: [{ id: "e", connection_id: "c", auth_scheme: "bearer", locked: false, operation: "chat_create",
      url, wire_protocol: "chat_completions" }] });
  const identity = (origin: string): Identity => ({ legacy: { kind: "account", id: "acc-1" },
    identity: { id: "identity", label: "Selected", authority_ref: null, identity_confidence: "declared", enabled: true, notes: null },
    declared_relations: [], credentials: [{ legacy: { kind: "account", id: "acc-1" },
      credential: { id: "credential", purpose: "inference", material_kind: "api_key", has_material: true, version: 1,
        enabled: true, auth_state: "valid", auth_state_version: 1, expires_at: null },
      bindings: [{ id: "binding", connection_id: "c", allowed_endpoint_ids: ["e"], allowed_origins: [origin],
        model_scope: { kind: "all" }, enabled: true, routing_rank: 0 }],
      last_error: null, onboarding_task: null, quota_pool_id: null, quota_windows: [], subject: "account_credential", subscription: null }] });
  const oldParent: PlatformAccount = { id: "p", name: "Old parent", kind: "new_api", baseUrl: "https://platform.example",
    hasUserCredential: true, snapshot: null, version: 1 };
  const oldLink: PlatformLink = { accountId: "acc-1", platformAccountId: "p", snapshot: null,
    group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: true } };
  const held = deferred<unknown>(); let platformReads = 0; let platformFlight: Promise<unknown> | undefined;
  const mounted = await mount("ready", false, async () => {
    pageReads++; return { ...accountsPage("ready"), revision: { revision: 7, processGeneration: 99, pricingRevision: "fixture" } };
  }, async () => {
    useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
    handlers.accountDetail = async () => {
      accountDetails++; const full = projection();
      return { revision: { revision: 7, processGeneration: 99, pricingRevision: "fixture" }, account: account(),
        destination: full.destinations[0], credential: full.credentials[0], identity: identity("https://old.example"),
        connection: connection("https://old.example/v1"), platform: oldParent, platformLink: oldLink };
    };
    handlers.platformList = () => { platformReads++; return held.promise; };
  });
  try {
    const detail = await useAccountPageStore().loadDetail("acc-1");
    assert.equal(mounted.helpers.connectionsForAccount("acc-1")[0]?.endpoints[0]?.url, "https://old.example/v1");
    assert.equal(mounted.helpers.platformParentForAccount("acc-1")?.name, "Old parent");
    platformFlight = usePlatformAccountsStore().load(); await settle();
    assert.equal(mounted.helpers.platformLinkForAccount("acc-1"), oldLink);
    assert.equal(mounted.helpers.platformParentForAccount("acc-1"), oldParent);
    handlers.connectionsSnapshot = async () => {
      useControlPlaneStore().sync({ revision: 8, processGeneration: 99 });
      return { connections: [connection("https://new.example/v1")], expectation: { expectedRevision: 8, processGeneration: 99 } };
    };
    handlers.identitySnapshot = async () => ({ identities: [identity("https://new.example")],
      expectation: { expectedRevision: 8, processGeneration: 99 } });
    await useProvidersStore().loadConnections(); await useIdentitiesStore().loadPresented();
    assert.equal(useAccountPageStore().details.get("acc-1"), detail);
    assert.equal(accountInferenceEndpointUrl(detail.account, mounted.helpers.identityForCard("acc-1"),
      mounted.helpers.connectionsForAccount("acc-1")), "https://new.example/v1");
    handlers.connectionsSnapshot = async () => {
      useControlPlaneStore().sync({ revision: 9, processGeneration: 99 });
      return { connections: [], expectation: { expectedRevision: 9, processGeneration: 99 } };
    };
    await useProvidersStore().loadConnections();
    assert.equal(useAccountPageStore().details.get("acc-1"), detail);
    assert.deepEqual(mounted.helpers.connectionsForAccount("acc-1"), []);
    held.resolve({ revision: 8, processGeneration: 99, accounts: [{ ...oldParent, name: "Fresh parent" }], links: [oldLink] });
    await platformFlight;
    assert.equal(mounted.helpers.platformParentForAccount("acc-1")?.name, "Fresh parent");
    acceptPlatformView({ revision: 9, processGeneration: 99, accounts: [{ ...oldParent, name: "Fresh parent" }], links: [] });
    assert.equal(useAccountPageStore().details.get("acc-1"), detail);
    assert.equal(mounted.helpers.platformLinkForAccount("acc-1"), null);
    assert.equal(mounted.helpers.platformParentForAccount("acc-1"), null);
    assert.equal(platformReads, 1);
  } finally {
    held.resolve(emptyPlatformView()); await platformFlight; mounted.app.unmount(); dropAllSnapshots();
  }
});

test("one Previous returns to the accepted preceding card cursor after a differently sized initial slice", { timeout: 20000 }, async () => {
  const offsets: number[] = [];
  const rows = (offset: number, count: number) => accountsPage("ready").cards[0]!.rows.flatMap(row =>
    Array.from({ length: count }, (_unused, at) => ({ ...row, credential: { ...row.credential, id: `credential-${offset + at}`,
      legacyAccountId: `account-${offset + at}` }, account: { ...row.account, id: `account-${offset + at}` } })));
  const mounted = await mount("ready", false, async () => {
    pageReads++; const base = accountsPage("ready");
    return { ...base, totalCredentials: 14, matchedCredentials: 14, cards: base.cards.map(card => ({ ...card,
      totalCredentials: 14, matchedCredentials: 14, rowsHasMore: true, rows: rows(0, 9) })) };
  }, async () => {
    handlers.accountCredentials = async (_id, query) => {
      const offset = (query as { offset: number }).offset; offsets.push(offset);
      assert.equal((query as { limit: number }).limit, 5);
      return { revision: { revision: 1, processGeneration: 1, pricingRevision: "fixture" }, readVersion: "fixture-1",
        asOf: "2026-10-07T00:00:00Z", validUntil: null, cardId: "card-1", total: 14, filteredTotal: 14,
        rows: rows(offset, 5), offset, limit: 5, hasMore: offset + 5 < 14, errors: [] };
    };
  });
  try {
    assert.equal(byClass(mounted.root, "credential-row").length, 9);
    const pagination = () => byClass(mounted.root, "account-page-pagination")[0]!;
    fire(buttonsUnder(pagination())[1]!);
    await waitFor("next card cursor", () => row(mounted.root).props["data-credential-id"] === "credential-9", mounted.root);
    await settle();
    fire(buttonsUnder(pagination())[0]!);
    await waitFor("preceding card cursor", () => row(mounted.root).props["data-credential-id"] === "credential-0", mounted.root);
    assert.deepEqual(offsets, [9, 0]);
    assert.equal(buttonsUnder(pagination())[0]?.props.disabled, true);
    assert.equal(accountReads, 0);
  } finally { mounted.app.unmount(); dropAllSnapshots(); }
});

test("a large server inventory stays bounded across card paging", { timeout: 20000 }, async () => {
  const offsets: number[] = [];
  const mounted = await mount("ready", false, async (query: unknown) => {
    pageReads++;
    const offset = (query as { offset?: number }).offset ?? 0;
    offsets.push(offset);
    const base = accountsPage("ready");
    const sample = base.cards[0]!;
    return { ...base, totalCards: 100, totalCredentials: 100, matchedCards: 100, matchedCredentials: 100,
      offset, hasMore: offset + 10 < 100, cards: Array.from({ length: 10 }, (_, at) => {
        const id = String(offset + at);
        return { ...sample, cardId: `card-${id}`, position: offset + at, rows: sample.rows.map(row => ({ ...row,
          account: { ...row.account, id: `account-${id}`, name: `Synthetic ${id}` },
          credential: { ...row.credential, id: `credential-${id}`, legacyAccountId: `account-${id}` } })) };
      }) };
  });
  try {
    assert.equal(byClass(mounted.root, "credential-row").length, 10);
    assert.equal(accountReads, 0);
    const paging = byClass(mounted.root, "account-page-pagination").at(-1)!;
    fire(buttonsUnder(paging).at(-1)!);
    await waitFor("next card page", () => offsets.length === 2, mounted.root); await settle(8);
    assert.deepEqual(offsets, [0, 10]);
    assert.equal(byClass(mounted.root, "credential-row").length, 10);
    assert.equal(byClass(mounted.root, "credential-row")[0]?.props["data-credential-id"], "credential-10");
    assert.equal(accountReads, 0);
  } finally { mounted.app.unmount(); dropAllSnapshots(); }
});

test("browser filter restoration sends the server query without resetting its page", { timeout: 20000 }, async () => {
  const queries: Array<{ search?: string; offset?: number }> = [];
  const mounted = await mount("ready", false, async (query: unknown) => {
    pageReads++; queries.push(query as { search?: string; offset?: number });
    return { ...accountsPage("ready"), offset: (query as { offset?: number }).offset ?? 0 };
  });
  try {
    await mounted.router.push({ name: "accounts", query: { search: "synthetic", plan: "opencode", offset: "20" } });
    await waitFor("restored filter query", () => queries.length >= 2, mounted.root);
    assert.deepEqual({ search: queries.at(-1)?.search, offset: queries.at(-1)?.offset }, { search: "synthetic", offset: 20 });
    assert.equal(accountReads, 0);
  } finally { mounted.app.unmount(); dropAllSnapshots(); }
});

test("each status selection sends its semantic server code and keeps the returned summary row", { timeout: 20000 }, async () => {
  const queries: Array<string | undefined> = [];
  const mounted = await mount("ready", false, async (query: unknown) => {
    pageReads++;
    const status = (query as { status?: string }).status;
    queries.push(status);
    const page = accountsPage("ready");
    const row = page.cards[0]!.rows[0]!;
    const serverStatus = ({ available: "enabled", "auth-error": "unavailable" } as Record<string, string>)[status ?? ""] ?? status ?? "cooling";
    return { ...page, cards: [{ ...page.cards[0]!, rows: [{ ...row, status: serverStatus,
      credential: { ...row.credential, id: `credential-${status ?? 'all'}` } }] }] };
  });
  try {
    for (const status of ["available", "cooling", "auth-error", "disabled", "registering", "all"]) {
      const selector = walkHostNodes(mounted.root).find(node => Array.isArray(node.props.options)
        && (node.props.options as Array<{ value?: unknown }>).some(option => option.value === "auth-error"));
      const select = selector?.props["onUpdate:value"];
      assertTextInput(select, "fixture: semantic status filter should expose its selection handler");
      const previous = queries.length;
      select(status);
      const timers = (globalThis.window as unknown as { __timers: Map<number, { fn: () => void }> }).__timers;
      const timer = [...timers.entries()].at(-1);
      if (!timer) throw new Error("fixture: status selection should schedule a debounced server query");
      timers.delete(timer[0]); timer[1].fn();
      await waitFor("semantic status response", () => queries.length > previous, mounted.root); await settle(8);
      assert.equal(queries.at(-1), status === "all" ? undefined : status);
      assert.equal(rowForStatus(mounted.root)?.props["data-credential-id"], `credential-${status}`);
    }
    assert.equal(accountReads, 0);
  } finally { mounted.app.unmount(); dropAllSnapshots(); }
});

function rowForStatus(root: HostNode): HostNode | undefined { return byClass(root, "credential-row")[0]; }

test("routing controls show newer page facts and keep the save receipt when the page follow-up fails", { timeout: 20000 }, async () => {
  let settingsReads = 0;
  let writes = 0;
  let pageCalls = 0;
  const mounted = await mount("ready", false, async () => {
    pageReads++;
    if (++pageCalls > 1) throw new Error("page follow-up offline");
    return { ...accountsPage("ready"), revision: { revision: 2, processGeneration: 1, pricingRevision: "fixture" }, routingMode: "round-robin" };
  }, async () => {
    handlers.getSettings = async () => {
      if (++settingsReads > 2) throw new Error("settings canonical follow-up offline");
      return settingsReads === 1 ? settings() : { ...settings(), revision: 2, routing_mode: "round-robin" };
    };
    handlers.patchSettings = async () => { writes++; return { revision: 3, processGeneration: 1 }; };
    await useSettingsStore().loadPresented();
  });
  try {
    const selector = byClass(mounted.root, "routing-select")[0]!;
    assert.equal(selector.props.value, "round-robin");
    const select = selector.props["onUpdate:value"];
    assertTextInput(select, "fixture: routing mode should expose its selection handler");
    select("strict-priority");
    await waitFor("routing save receipt", () => writes === 1 && byClass(mounted.root, "routing-select")[0]?.props.value === "strict-priority", mounted.root);
    await settle(8);
    assert.equal(settingsReads, 3);
    assert.equal(writes, 1);
    assert.equal(byClass(mounted.root, "routing-select")[0]?.props.value, "strict-priority");
    assert.ok(pageCalls > 1);
  } finally { mounted.app.unmount(); dropAllSnapshots(); }
});

test("a manual partial refresh warns once and retains rendered content when revalidation fails", { timeout: 20000 }, async () => {
  let calls = 0;
  let refreshes = 0;
  const mounted = await mount("ready", false, async () => {
    pageReads++;
    if (++calls > 1) throw new Error("page refresh offline");
    const base = accountsPage("ready");
    return { ...base, cards: base.cards.map(card => ({ ...card, rows: card.rows.map(row => ({ ...row,
      actions: [...row.actions, { key: "refresh-usage", allowed: true, reason: null }] })) })) };
  }, async () => {
    useControlPlaneStore().sync({ revision: 1, processGeneration: 1 });
    handlers.accountRefresh = async (_id, mode) => {
      assert.equal(mode, "manual"); refreshes++;
      return { revision: { revision: 2, processGeneration: 1, pricingRevision: "fixture" }, outcome: "partial", account: null,
        billing: null, refresh: { supported: true, observedAt: "2026-10-07T01:00:00Z", freshUntil: null, nextAllowedAt: null },
        errors: [{ resource: "platform", id: "acc-1", code: "models_unavailable" },
          { resource: "platform", id: "acc-1", code: "models_unavailable" }] };
    };
  });
  try {
    const refreshClick = byClass(mounted.root, "credential-refresh-trigger")[0]!.props.onClick;
    assert.equal(typeof refreshClick, "function");
    if (typeof refreshClick !== "function") throw new Error("fixture: missing refresh click handler");
    refreshClick();
    await waitFor("partial refresh warning and bounded follow-up", () => refreshes === 1 && calls > 1 && messageCount("warning") === 1, mounted.root);
    assert.equal(messageCount("success"), 0);
    assert.equal(byClass(mounted.root, "credential-row").length, 1);
    assert.equal(accountReads, 0); assert.equal(accountDetails, 0);
  } finally { mounted.app.unmount(); dropAllSnapshots(); }
});

test("purchase-date control ends when the write is acknowledged while usage is still pending", { timeout: 20000 }, async () => {
  const mounted = await mount("ready");
  const usage = deferred<unknown>();
  let pending = false;
  let requested = "";
  handlers.billingStatus = () => {
    billingCalls += 1;
    pending = true;
    return usage.promise.finally(() => { pending = false; });
  };
  handlers.updateAccount = async (_id: unknown, update: unknown) => {
    requested = readPurchaseDate(update);
    return account({ purchase_date: requested });
  };
  try {
    const actions = purchaseActions(mounted.root);
    const today = buttonsUnder(actions)[0];
    if (!today) throw new Error("fixture: update-to-today control should render");
    fire(today);
    await waitFor("purchase-date ack", () => storedAccount()?.purchase_date === requested && requested !== "", mounted.root);
    await settle(6);
    const trigger = byClass(mounted.root, "account-expiry-trigger")[0];
    const save = buttonsUnder(purchaseActions(mounted.root))[1];
    assert.deepEqual({
      triggerDisabled: Boolean(trigger?.props.disabled),
      saveLoading: save?.props["data-loading"] ?? null,
      success: messageCount("success"),
      billingCalls,
      usagePending: pending,
      purchaseDate: storedAccount()?.purchase_date ?? null,
    }, {
      triggerDisabled: false,
      saveLoading: "false",
      success: 1,
      billingCalls: 1,
      usagePending: true,
      purchaseDate: requested,
    });
  } finally {
    usage.resolve({ revision: 1, processGeneration: 1 });
    await settle();
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("purchase-date success does not upsert or notify after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("ready");
  const gate = deferred<unknown>();
  let calls = 0;
  handlers.updateAccount = () => {
    calls += 1;
    return gate.promise;
  };
  handlers.billingStatus = async () => { throw new Error("billing offline"); };
  try {
    fire(buttonsUnder(purchaseActions(mounted.root))[0]!);
    await waitFor("purchase-date request", () => calls === 1, mounted.root);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    gate.resolve(account({ purchase_date: "2024-05-06" }));
    assert.deepEqual(await teardownOutcome(), { accounts: [], success: 0, warning: 0, error: 0, snapshot: false, destinations: 0 });
  } finally {
    gate.resolve(account());
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("toggle does not restore the account after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("ready");
  const gate = deferred<unknown>();
  let calls = 0;
  handlers.toggleAccount = () => {
    calls += 1;
    return gate.promise;
  };
  try {
    const control = walkHostNodes(row(mounted.root)).find((node) => node.props.role === "switch");
    if (!control) throw new Error(`fixture: account switch should render\n${outline(mounted.root)}`);
    fire(control);
    await waitFor("toggle request", () => calls === 1);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    gate.resolve(account({ enabled: false }));
    const outcome = await teardownOutcome();
    if (outcome.destinations > 0) console.log(`OBSERVATION toggle destinations repopulated: ${outcome.destinations}`);
    assert.deepEqual({ accounts: outcome.accounts, success: outcome.success, warning: outcome.warning, error: outcome.error, snapshot: outcome.snapshot }, {
      accounts: [], success: 0, warning: 0, error: 0, snapshot: false,
    });
  } finally {
    gate.resolve(account());
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("cooldown reset does not upsert or notify after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("ready");
  const gate = deferred<unknown>();
  let calls = 0;
  handlers.resetAccountCooldown = () => {
    calls += 1;
    return gate.promise;
  };
  try {
    const control = walkHostNodes(row(mounted.root)).find((node) => node.props["data-menu-key"] === "reset");
    if (!control) throw new Error(`fixture: reset control should render\n${outline(row(mounted.root))}`);
    fire(control);
    await waitFor("cooldown reset request", () => calls === 1);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    gate.resolve(account({ cooldown_until: null }));
    assert.deepEqual(await teardownOutcome(), { accounts: [], success: 0, warning: 0, error: 0, snapshot: false, destinations: 0 });
  } finally {
    gate.resolve(account());
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("managed setup advance does not upsert or notify after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("google");
  const gate = deferred<unknown>();
  let calls = 0;
  handlers.advanceAccountSetup = () => {
    calls += 1;
    return gate.promise;
  };
  try {
    await openWizard(mounted.root);
    fire(wizardPrimary(mounted.root));
    await waitFor("setup advance request", () => calls === 1);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    gate.resolve(account({ account_type: "managed", setup_step: "opencode_registration" }));
    assert.deepEqual(await teardownOutcome(), { accounts: [], success: 0, warning: 0, error: 0, snapshot: false, destinations: 0 });
  } finally {
    gate.resolve(account());
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("managed key verification does not upsert or notify after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("verify");
  const gate = deferred<unknown>();
  let calls = 0;
  handlers.verifyManagedAccountKey = () => {
    calls += 1;
    return gate.promise;
  };
  try {
    await openWizard(mounted.root);
    const keyField = walkHostNodes(mounted.root).find((node) => node.type === "input" && classTokens(node.props.class).includes("managed-wizard__key"))
      ?? walkHostNodes(mounted.root).filter((node) => node.type === "input").at(-1);
    const onKey = keyField?.props.onInput;
    assertTextInput(onKey, `fixture: key field should render\n${outline(mounted.root)}`);
    onKey("sk-synthetic");
    await settle(4);
    fire(wizardPrimary(mounted.root));
    await waitFor("key verification request", () => calls === 1);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    gate.resolve(account({ account_type: "managed", setup_step: "ready" }));
    assert.deepEqual(await teardownOutcome(), { accounts: [], success: 0, warning: 0, error: 0, snapshot: false, destinations: 0 });
  } finally {
    gate.resolve(account());
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("managed profile reset does not upsert or notify after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("google");
  const gate = deferred<unknown>();
  let calls = 0;
  handlers.resetAccountBrowserProfile = () => {
    calls += 1;
    return gate.promise;
  };
  try {
    const control = walkHostNodes(row(mounted.root)).find((node) => node.props["data-menu-key"] === "reset-profile");
    if (!control) throw new Error(`fixture: reset-profile control should render\n${outline(row(mounted.root))}`);
    fire(control);
    await waitFor("profile reset request", () => calls === 1);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    gate.resolve(account({ account_type: "managed", setup_step: "google_account" }));
    assert.deepEqual(await teardownOutcome(), { accounts: [], success: 0, warning: 0, error: 0, snapshot: false, destinations: 0 });
  } finally {
    gate.resolve(account());
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("purchase-date conflict recovery does not restore accounts or warn after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("ready");
  const readsAtMount = accountReads;
  const recovery = deferred<unknown>();
  let recoveryStarted = false;
  useControlPlaneStore().sync({ revision: 1, processGeneration: 1 });
  handlers.getAccounts = () => { accountReads++; recoveryStarted = true; return recovery.promise; };
  handlers.accountsSnapshot = async () => ({ accounts: await handlers.getAccounts(), expectation: { expectedRevision: 2, processGeneration: 1 } });
  handlers.updateAccount = async () => {
    useControlPlaneStore().sync({ revision: 2, processGeneration: 1 });
    throw new DashboardConflictError("revision conflict", 2, 1);
  };
  try {
    fire(buttonsUnder(purchaseActions(mounted.root))[0]!);
    await waitFor("conflict reload", () => recoveryStarted, mounted.root);
    assert.equal(accountReads, readsAtMount + 1);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    recovery.resolve([account({ id: "restored", name: "Restored" })]);
    assert.deepEqual(await teardownOutcome(), { accounts: [], success: 0, warning: 0, error: 0, snapshot: false, destinations: 0 });
  } finally {
    recovery.resolve([]);
    mounted.app.unmount();
    dropAllSnapshots();
  }
});

test("managed account create does not upsert or notify after session teardown", { timeout: 20000 }, async () => {
  const mounted = await mount("ready");
  const gate = deferred<unknown>();
  let calls = 0;
  handlers.createManagedAccount = () => {
    calls += 1;
    return gate.promise;
  };
  try {
    const toolbar = byClass(mounted.root, "accounts-actions")[0];
    const add = toolbar ? buttonsUnder(toolbar).find((button) => button.props["data-variant"] === "primary") : undefined;
    if (!add) throw new Error(`fixture: add-account control should render\n${outline(mounted.root)}`);
    fire(add);
    await waitFor("add chooser", () => walkHostNodes(mounted.root).some((node) => node.props.id === "account-add-option-opencode"), mounted.root);
    const services = walkHostNodes(mounted.root).find((node) => node.props["data-choice"] === "services");
    if (services) fire(services);
    await settle(4);
    const option = walkHostNodes(mounted.root).find((node) => node.props.id === "account-add-option-opencode");
    if (!option) throw new Error(`fixture: opencode option should render\n${outline(mounted.root)}`);
    fire(option);
    await settle(4);
    const detail = byClass(mounted.root, "account-add-detail__actions")[0];
    const register = detail ? buttonsUnder(detail)[0] : undefined;
    if (!register) throw new Error(`fixture: managed registration control should render\n${outline(mounted.root)}`);
    await waitFor("managed registration options", () => !register.props.disabled, mounted.root);
    fire(register);
    await waitFor("managed create form", () => byClass(mounted.root, "account-managed-modal").length > 0, mounted.root);
    const modal = byClass(mounted.root, "account-managed-modal")[0]!;
    const name = walkHostNodes(modal).find((node) => node.type === "input");
    const onName = name?.props.onInput;
    assertTextInput(onName, "fixture: managed name field should render");
    onName("Synthetic draft");
    await settle(4);
    const create = buttonsUnder(modal).find((button) => button.props["data-variant"] === "primary");
    if (!create) throw new Error("fixture: managed create control should render");
    fire(create);
    await waitFor("managed create request", () => calls === 1, mounted.root);
    pageMessages().splice(0, pageMessages().length);
    useSessionStore().dropSession();
    gate.resolve(account({ id: "created", account_type: "managed", setup_step: "google_account", name: "Synthetic draft" }));
    assert.deepEqual(await teardownOutcome(), { accounts: [], success: 0, warning: 0, error: 0, snapshot: false, destinations: 0 });
  } finally {
    gate.resolve(account());
    mounted.app.unmount();
    dropAllSnapshots();
  }
});
