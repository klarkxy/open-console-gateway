import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import { pathToFileURL } from "node:url";
import { build } from "vite";
import vue from "@vitejs/plugin-vue";
import { getActivePinia } from "pinia";
import { createMemoryHistory, createRouter, type Router } from "vue-router";
import { defineComponent, ssrContextKey, type App, type Component } from "vue";
import { v3AccountDto, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { dropAllSnapshots } from "../stores/persistence.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSessionStore } from "../stores/session.ts";
import { useProviderPageStore } from "../stores/providerPage.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import {
  createVueHostRenderer,
  installTestWindow,
  settle,
  text,
  walkHostNodes,
  type HostNode,
} from "../test-helpers/vue-host-runtime.ts";

const DROP = "drop-me";
const KEEP = "keep-me";
const PROBE = "probe-sentinel";
const PROJECTION = "F10_PROJECTION";
const RELOAD = "F09_RELOAD";

let buildDir = "";
let Providers: Component;
const renderer = createVueHostRenderer();
const storage = memoryStorage();

type Recorded = { url: string; method: string; body?: Record<string, any> };
type Release = { reject: (error: Error) => void; resolve: () => void };
type Gate = { holdGets: boolean; failGets: boolean; token: string; pending: Release[]; variant?: "http" | "refreshable";
  holdRefresh?: boolean; readRevision?: number;
  editVersion?: string; headerVersion?: string; modelVersion?: string; holdReadSuffixes?: string[]; editAccounts?: object[] };
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
    NPagination: `export const NPagination = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", attrs, [
        ...(slots.prev?.({ page: attrs.page }) ?? []),
        ...(slots.label?.({ type: "page", node: attrs.page, active: true }) ?? []),
        ...(slots.next?.({ page: attrs.page }) ?? []),
      ]);
    } });`,
    NAlert: `export const NAlert = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { role: "alert", type: attrs.type, class: attrs.class }, [attrs.title ?? "", ...Object.values(slots).flatMap((slot) => slot?.() ?? [])]);
    } });`,
    NButton: `export const NButton = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("button", attrs, slots.default?.());
    } });`,
    NDataTable: `export const NDataTable = defineComponent({ inheritAttrs: false, setup(_, { attrs }) {
      return () => {
        const columns = Array.isArray(attrs.columns) ? attrs.columns : [];
        const data = Array.isArray(attrs.data) ? attrs.data : [];
        return h("table", {}, data.map((row, index) => h("tr", { key: String(index) }, columns.map((column) => {
          const rendered = typeof column.render === "function" ? column.render(row) : row?.[column.key];
          return h("td", {}, rendered ?? "");
        }))));
      };
    } });`,
    NModal: `export const NModal = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => attrs.show === false ? h("div") : h("div", { role: "dialog", class: attrs.class }, [...(slots.default?.() ?? []), ...(slots.footer?.() ?? [])]);
    } });`,
    NPopconfirm: `export const NPopconfirm = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { class: "popconfirm" }, [...(slots.trigger?.() ?? []), h("button", { class: "popconfirm-positive", onClick: () => invoke(attrs.onPositiveClick) })]);
    } });`,
    NSpin: `export const NSpin = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { role: "status", class: attrs.class }, slots.default?.());
    } });`,
    NTabPane: `export const NTabPane = defineComponent({ inheritAttrs: false, setup(_, { attrs, slots }) {
      return () => h("div", { "data-tab": attrs.name }, slots.default?.());
    } });`,
    NTooltip: `export const NTooltip = defineComponent({ inheritAttrs: false, setup(_, { slots }) {
      return () => h("span", {}, [...(slots.trigger?.() ?? []), ...(slots.default?.() ?? [])]);
    } });`,
    useDialog: `export function useDialog() { const noop = () => ({ destroy() {} }); return { warning: noop, error: noop, success: noop, info: noop }; }`,
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
    function invoke(value) {
      if (typeof value === "function") value();
      else if (Array.isArray(value)) value.forEach(invoke);
    }
    ${body.join("\n")}
  `;
}

function iconSource(names: string[]): string {
  const exports = names.map((name) => `export const ${name} = icon;`).join("\n");
  return `
    import { defineComponent, h } from "vue";
    const icon = defineComponent(() => () => h("i"));
    ${exports}
  `;
}

function harnessPlugin(naive: string, icons: string) {
  return {
    name: "ocg-providers-host",
    enforce: "pre" as const,
    resolveId(source: string) {
      if (source === "naive-ui") return "\0ocg-naive";
      if (source === "@vicons/antd") return "\0ocg-icons";
      return null;
    },
    load(id: string) {
      if (id.includes("type=style")) return "";
      if (id === "\0ocg-naive") return naive;
      if (id === "\0ocg-icons") return icons;
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

function contractModel(id: string): object {
  return {
    alias: id,
    modelId: id,
    preferredProtocol: "chat_completions",
    protocols: { chat_completions: evidence(), responses: null, messages: null },
    routable: true,
    disabledReasons: [],
  };
}

function catalogPresentation(modelIds: string[], disabled = false) {
  return { total: modelIds.length, allDisabled: disabled, models: modelIds.map(id => {
    const contract = contractModel(id) as Record<string, any>;
    if (disabled) { contract.protocols.chat_completions.enabled = false; contract.routable = false; }
    return { publicModel: id, upstreamModel: id, contract, upstreamOverride: null,
      targetProtocol: "chat_completions", testProtocol: disabled ? null : "chat_completions",
      writableProtocols: ["chat_completions"], effectiveOn: !disabled,
      actions: [{ key: "toggle", allowed: true, reason: null }, { key: "test", allowed: !disabled, reason: disabled ? "unavailable" : null },
        { key: "modelEditable", allowed: true, reason: null }, { key: "metadataEditable", allowed: true, reason: null }] };
  }) };
}

function contractsBody(modelIds: string[]): object {
  return {
    revision: 12,
    processGeneration: 42,
    pricingRevision: "p1",
    customEndpoints: [],
    providers: [{
      presentation: catalogPresentation(modelIds),
      scopeKind: "provider",
      scopeId: "opencode",
      providerId: "opencode",
      staticProtocolSnapshotDate: null,
      accounts: [],
      catalog: { source: "static", sourceUrl: "", refreshedAt: null, models: modelIds, refreshSupported: false },
      models: modelIds.map(contractModel),
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
    connections: [{
      adapterKind: "opencode_go",
      authorization: "valid",
      credentialCount: 1,
      credentialCreate: { allowed: false, materialKinds: [], reason: null },
      displayFamily: "OpenCode",
      eligibility: { reason: "none", state: "eligible" },
      enabledCredentialCount: 1,
      endpoints: [{
        officialBalance: false,
        authScheme: "bearer",
        connectionId: "conn-opencode",
        id: "ep-opencode",
        locked: true,
        operation: "chat_create",
        url: "https://lab.example/v1/chat/completions",
        wireProtocol: "chat_completions",
      }],
      id: "conn-opencode",
      legacy: { id: "opencode", kind: "builtin_provider" },
      lifecycle: "configured",
      name: "OpenCode",
      offering: "api",
      origin: "builtin",
      targetCount: 1,
      targets: [{
        connectionId: "conn-opencode",
        enabled: true,
        endpointIds: ["ep-opencode"],
        id: "tgt-opencode",
        publicName: PROBE,
        upstreamModelId: PROBE,
      }],
      templateRef: null,
    }],
  };
}

function cardsBody(): object {
  return {
    cards: [{ id: "card-opencode", destinationId: "dest-opencode", credentialIds: [] }],
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
      catalog: [],
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

function removalReceipt(): object {
  return {
    revision: { revision: 13, processGeneration: 42, pricingRevision: "p1" },
    removedIds: [DROP],
    catalogModels: [KEEP],
  };
}

function probeReceipt(): object {
  return {
    accountId: null,
    providerId: "opencode",
    modelId: PROBE,
    results: [{ protocol: "chat_completions", success: true, skipped: false, error: null }],
    contract: null,
    revision: 12,
    processGeneration: 42,
    pricingRevision: "p1",
  };
}

function pathnameOf(url: string): string {
  const marker = "/dashboard/api/v4";
  const start = url.indexOf(marker);
  const pathName = start >= 0 ? url.slice(start + marker.length) : url;
  return decodeURIComponent(pathName.split("?")[0] ?? pathName);
}

function pageItem(modelIds: string[]) {
  const connection = (connectionBody() as { connections: Record<string, any>[] }).connections[0]!;
  return { ...connection, railKey: "d:dest-opencode", connectionId: connection.id, destinationId: "dest-opencode",
    providerId: "opencode", brandFamily: "OpenCode", presetId: null, catalogCount: modelIds.length };
}
function pageDetailBody(modelIds: string[]) {
  const item = pageItem(modelIds);
  const contract = (contractsBody(modelIds) as { providers: Record<string, any>[] }).providers[0]!;
  const { accounts, ...contractSummary } = contract;
  const fullDestination = (cardsBody() as { destinations: Record<string, any>[] }).destinations[0]!;
  const { catalog: _catalog, ...destination } = fullDestination;
  return { revision: { revision: 12, processGeneration: 42 }, readVersion: "page12", item,
    destination: { ...destination, catalogCount: modelIds.length, enabledCatalogCount: modelIds.length },
    endpoints: (connectionBody() as { connections: Record<string, any>[] }).connections[0]!.endpoints,
    catalogEntry: (catalogBody() as { entries: object[] }).entries[0], modelWriteTarget: { kind: "provider", id: "opencode" },
    scope: { ...contractSummary, accountCount: accounts.length, key: "provider:opencode", label: "OpenCode", allDisabled: false,
      catalog: { source: "static", sourceUrl: "", refreshedAt: null, modelCount: modelIds.length, refreshSupported: false } },
    actions: [{ key: "modelEditable", allowed: true, reason: null }, { key: "metadataEditable", allowed: true, reason: null }] };
}
function bodyFor(pathname: string, method: string, modelIds: string[], params = new URLSearchParams()): object {
  if (pathname === "/pages/providers") return { revision: { revision: 12, processGeneration: 42 }, readVersion: "rail12",
    asOf: new Date().toISOString(), validUntil: new Date(Date.now() + 15_000).toISOString(), total: 1, filteredTotal: 1,
    items: [pageItem(modelIds)], offset: 0, limit: 50, hasMore: false, errors: [] };
  if (pathname.startsWith("/pages/providers/") && pathname.endsWith("/edit-detail")) return {
    revision: { revision: 12, processGeneration: 42 }, readVersion: "page12", item: pageItem(modelIds),
    destination: (cardsBody() as { destinations: object[] }).destinations[0], credentials: [], definition: null,
    relatedContracts: contractsBody(modelIds), catalogEntry: (catalogBody() as { entries: object[] }).entries[0],
    scope: { summary: (pageDetailBody(modelIds) as any).scope,
      accounts: (contractsBody(modelIds) as any).providers[0].accounts,
      models: modelIds.map(id => ({ publicModel: id, upstreamModel: id, contract: contractModel(id), upstreamOverride: null,
        metadata: null, metadataSource: null, targetProtocol: "chat_completions", testProtocol: "chat_completions",
        writableProtocols: ["chat_completions"], effectiveOn: true, actions: [] })) },
    accounts: [], identities: [], connection: (connectionBody() as { connections: object[] }).connections[0] };
  if (pathname.startsWith("/pages/providers/") && pathname.endsWith("/models")) {
    const search = params.get("search")?.toLowerCase() ?? "";
    const matched = modelIds.filter(id => !search || id.toLowerCase().includes(search));
    let offset = Number(params.get("offset") ?? 0);
    const target = params.get("model");
    if (target && matched.includes(target)) offset = Math.floor(matched.indexOf(target) / 50) * 50;
    return {
      revision: { revision: 12, processGeneration: 42 }, readVersion: "page12", total: modelIds.length, filteredTotal: matched.length,
      allDisabled: false, offset, limit: 50, hasMore: offset + 50 < matched.length,
      models: matched.slice(offset, offset + 50).map(id => ({ publicModel: id, upstreamModel: id, contract: contractModel(id), upstreamOverride: null,
        metadata: null, metadataSource: null, targetProtocol: "chat_completions", testProtocol: "chat_completions",
        writableProtocols: ["chat_completions"], effectiveOn: true, actions: [{key: "toggle", allowed: true}, {key: "test", allowed: true}, {key: "modelEditable", allowed: true}, {key: "metadataEditable", allowed: true}] })) };
  }
  if (pathname.startsWith("/pages/providers/")) return pageDetailBody(modelIds);
  if (pathname === "/provider-contracts" && method === "GET") return contractsBody(modelIds);
  if (pathname === "/providers" && method === "GET") return catalogBody();
  if (pathname === "/connections" && method === "GET") return connectionBody();
  if (pathname === "/account-records" && method === "GET") {
    return {
      accounts: [v3AccountDto("acct-opencode", { providerId: "opencode" })],
      revision: 12,
      processGeneration: 42,
    };
  }
  if (pathname === "/routing/cards" && method === "GET") return cardsBody();
  if (pathname === "/provider-contracts/provider/opencode/catalog/remove" && method === "POST") return removalReceipt();
  if (pathname === "/provider-contracts/provider/opencode/model-protocol-overrides" && method === "PUT") return contractsBody(modelIds);
  if (pathname === "/providers/opencode/protocol-probes" && method === "POST") return probeReceipt();
  throw new Error(`unexpected providers request ${method} ${pathname}`);
}

function httpDestination(modelIds: string[], disabled = false) {
  const source = (cardsBody() as { destinations: Record<string, any>[] }).destinations[0]!;
  return { ...source, presentation: catalogPresentation(modelIds, disabled), adapter: "http", legacy: { kind: "dynamic", id: "dyn-http" }, name: "HTTP Provider",
    capabilities: { ...source.capabilities, discoverableModels: true },
    catalog: modelIds.map(publicModel => ({ publicModel, upstreamModel: publicModel, protocols: ["chat_completions"], preferred: "chat_completions", enabled: !disabled, upstreamOverride: null })) };
}
function variantBody(body: Record<string, any>, pathname: string, gate: Gate, modelIds: string[]) {
  if (gate.variant === "http") {
    const item = { ...pageItem(modelIds), providerId: "dyn-http", name: "HTTP Provider", legacy: { kind: "dynamic_provider", id: "dyn-http" } };
    if (pathname === "/pages/providers") return { ...body, items: [item] };
    if (pathname.endsWith("/edit-detail")) return { ...body, item, destination: httpDestination(modelIds) };
    if (pathname.startsWith("/pages/providers/") && !pathname.endsWith("/models")) {
      const { catalog: _catalog, ...summary } = httpDestination(modelIds);
      return { ...body, item, destination: { ...summary, catalogCount: modelIds.length, enabledCatalogCount: modelIds.length },
        catalogEntry: { ...body.catalogEntry, providerId: "dyn-http", origin: "custom", editable: true, deletable: true },
        scope: { ...body.scope, key: "custom_endpoint:dest-opencode", scopeKind: "custom_endpoint", scopeId: "dest-opencode", providerId: "dyn-http",
          catalog: { ...body.scope.catalog, refreshSupported: true }, card: { ...body.scope.card, catalogRefresh: true } },
        actions: [...body.actions, { key: "edit", allowed: true }, { key: "refreshCatalog", allowed: true }] };
    }
  }
  if (gate.variant === "refreshable" && body.scope) return { ...body, scope: { ...body.scope, card: { ...body.scope.card, catalogRefresh: true } } };
  return body;
}

function installDashboard(modelIds: string[], requests: Recorded[], gate: Gate): void {
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: async (input: string, init: RequestInit = {}) => {
      const url = String(input);
      const method = init.method ?? "GET";
      requests.push({ url, method, ...(init.body ? { body: JSON.parse(String(init.body)) } : {}) });
      const pathname = pathnameOf(url);
      if ((method === "POST" && pathname.endsWith("catalog/refresh") && gate.holdRefresh)
        || (method === "GET" && ((!pathname.endsWith("/edit-detail") && gate.holdGets)
        || gate.holdReadSuffixes?.some(suffix => pathname.endsWith(suffix))))) {
        await new Promise<void>((resolve, reject) => {
          gate.pending.push({ resolve, reject });
        });
      }
      if (method === "GET" && !pathname.endsWith("/edit-detail") && gate.failGets) throw new Error(gate.token);
      let body: object;
      if (pathname === "/destinations/dest-opencode/catalog" && method === "PUT") body = {
        destination: httpDestination(modelIds, true), credentials: [], revision: { revision: 13, processGeneration: 42, pricingRevision: "p1" } };
      else if (pathname === "/destinations/dest-opencode/catalog/refresh" && method === "POST") body = {
        destination: httpDestination([...modelIds, "refreshed-http-model"]), addedCount: 1, truncated: false,
        revision: { revision: 13, processGeneration: 42, pricingRevision: "p1" } };
      else if (pathname === "/provider-contracts/provider/opencode/catalog/refresh" && method === "POST") body = { ...contractsBody([...modelIds, "refreshed-builtin-model"]), revision: 13 };
      else body = variantBody(bodyFor(pathname, method, modelIds, new URL(url, "http://test.local").searchParams) as Record<string, any>, pathname, gate, modelIds);
      if (method === "GET" && gate.readRevision) {
        const revision = (body as { revision?: object | number }).revision;
        body = { ...body, revision: typeof revision === "object" ? { ...revision, revision: gate.readRevision } : gate.readRevision };
      }
      if (pathname.endsWith("/edit-detail") && gate.editVersion) body = { ...body, readVersion: gate.editVersion };
      if (pathname === "/pages/providers/d:dest-opencode" && gate.headerVersion) body = { ...body, readVersion: gate.headerVersion };
      if (pathname.endsWith("/edit-detail") && gate.editAccounts) body = { ...body, accounts: gate.editAccounts };
      if (pathname.endsWith("/models") && gate.modelVersion) body = { ...body, readVersion: gate.modelVersion };
      return new Response(JSON.stringify(body), {
        headers: { "Content-Type": "application/json" },
      });
    },
  });
}

function installDocument(): object {
  const element = () => {
    const node: Record<string, unknown> = {
      lang: "",
      style: {},
      className: "",
      textContent: "",
      clientWidth: 1280,
      clientHeight: 800,
      offsetWidth: 0,
      offsetHeight: 0,
    };
    node.classList = { add() {}, remove() {}, toggle() {}, contains: () => false };
    node.setAttribute = () => {};
    node.getAttribute = () => null;
    node.appendChild = () => node;
    node.removeChild = () => node;
    node.querySelector = () => null;
    node.querySelectorAll = () => [];
    node.addEventListener = () => {};
    node.removeEventListener = () => {};
    node.getBoundingClientRect = () => ({ width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0, x: 0, y: 0 });
    return node;
  };
  return {
    documentElement: element(),
    body: element(),
    head: element(),
    visibilityState: "visible",
    hidden: false,
    readyState: "complete",
    createElement: element,
    createElementNS: element,
    createTextNode: () => ({ textContent: "" }),
    querySelector: () => null,
    querySelectorAll: () => [],
    getElementById: () => null,
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => true,
    elementFromPoint: () => null,
  };
}

function prepareWindow(): void {
  const documentStub = installDocument();
  Object.defineProperty(globalThis, "document", { configurable: true, writable: true, value: documentStub });
  const view = installTestWindow({ pathname: "/dashboard/providers", href: "http://127.0.0.1/dashboard/providers?provider=opencode" });
  Object.assign(view, {
    dispatchEvent: () => true,
    localStorage: storage,
    document: documentStub,
    matchMedia: () => ({ matches: false, media: "", addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }),
    getComputedStyle: () => ({ getPropertyValue: () => "" }),
    requestAnimationFrame: (fn: () => void) => setTimeout(fn, 0),
    cancelAnimationFrame() {},
  });
  globalThis.localStorage = storage;
}

function messages(): MessageRecord[] {
  return (globalThis as { __ocgMessages?: MessageRecord[] }).__ocgMessages ?? [];
}

function messageText(type: string): string {
  return messages().filter((message) => message.type === type).map((message) => String(message.args[0] ?? "")).join("\n");
}

async function until(label: string, ready: () => boolean, detail: () => string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (ready()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  throw new Error(`${label}\n${detail()}`);
}

async function untilQuiet(requests: Recorded[], floor: number): Promise<void> {
  let last = -1;
  let stable = 0;
  for (let attempt = 0; attempt < 80; attempt += 1) {
    await new Promise((resolve) => setImmediate(resolve));
    if (requests.length === last && requests.length > floor) {
      stable += 1;
      if (stable >= 4) return;
    } else {
      stable = 0;
      last = requests.length;
    }
  }
  throw new Error(`provider reads did not settle (${requests.length})`);
}

function alerts(root: HostNode): HostNode[] {
  return walkHostNodes(root).filter((node) => node.props.role === "alert");
}

function className(node: HostNode): string {
  const value = node.props.class;
  return typeof value === "string" ? value : "";
}

function confirmFor(root: HostNode, modelId: string, kind: "probe" | "remove"): HostNode {
  const rows = walkHostNodes(root).filter((node) => node.type === "tr" && text(node).includes(modelId));
  for (const row of rows) {
    for (const pop of walkHostNodes(row).filter((node) => node.props.class === "popconfirm")) {
      const inner = walkHostNodes(pop);
      const matched = kind === "probe"
        ? inner.some((node) => String(node.props["aria-label"] ?? "").includes(modelId))
        : inner.some((node) => node.props.type === "error");
      if (!matched) continue;
      const positive = inner.find((node) => node.props.class === "popconfirm-positive");
      if (positive) return positive;
    }
  }
  throw new Error(`missing ${kind} confirm for ${modelId}; visible=${text(root).slice(0, 400)}`);
}

function click(node: HostNode): void {
  const handler = node.props.onClick;
  if (typeof handler === "function") handler();
  else if (Array.isArray(handler)) handler.forEach((entry) => { if (typeof entry === "function") entry(); });
  else throw new Error("confirm has no click handler");
}

function releasePending(gate: Gate, error?: Error): void {
  const pending = gate.pending.splice(0);
  for (const item of pending) {
    if (error) item.reject(error);
    else item.resolve();
  }
}

async function openProviders(modelIds: string[], gate: Gate, query: Record<string, string> = { provider: "opencode" }): Promise<{ app: App; root: HostNode; requests: Recorded[]; router: Router }> {
  dropAllSnapshots();
  storage.clear();
  prepareWindow();
  (globalThis as { __ocgMessages?: MessageRecord[] }).__ocgMessages = [];
  setupControlPlane(12, 42);
  const requests: Recorded[] = [];
  installDashboard(modelIds, requests, gate);
  const pinia = getActivePinia();
  if (!pinia) throw new Error("pinia should be active");
  useProvidersStore();
  useDestinationsStore();
  useAccountsStore();
  useSessionStore();
  const router: Router = createRouter({
    history: createMemoryHistory("/dashboard/"),
    routes: [
      { path: "/providers", name: "providers", component: defineComponent({ setup: () => () => null }) },
      { path: "/accounts", name: "accounts", component: defineComponent({ setup: () => () => null }) },
      { path: "/aliases", name: "aliases", component: defineComponent({ setup: () => () => null }) },
      { path: "/:pathMatch(.*)*", component: defineComponent({ setup: () => () => null }) },
    ],
  });
  await router.push({ name: "providers", query });
  await router.isReady();
  const root: HostNode = { children: [], props: {}, type: "root" };
  const app = renderer.createApp(Providers);
  app.use(pinia);
  app.use(router);
  app.provide(ssrContextKey, { modules: new Set<string>() });
  app.mount(root);
  await untilQuiet(requests, 0);
  await settle();
  return { app, root, requests, router };
}

function detail(root: HostNode, requests: Recorded[]): string {
  return `${requests.map((request) => `${request.method} ${pathnameOf(request.url)}`).join("\n")}\n${text(root).slice(0, 500)}\n${JSON.stringify(messages())}`;
}

before(async () => {
  prepareWindow();
  const scratch = path.join(process.cwd(), ".artifacts", "frontend-logic-repair", "provider-tests");
  await mkdir(scratch, { recursive: true });
  buildDir = await mkdtemp(path.join(scratch, "build-providers-"));
  await build({
    configFile: false,
    logLevel: "silent",
    plugins: [harnessPlugin(naiveSource(importedNames("naive-ui")), iconSource(importedNames("@vicons/antd"))), vue()],
    build: {
      emptyOutDir: true,
      target: "esnext",
      lib: { entry: path.resolve("src/views/Providers.vue"), fileName: () => "providers.mjs", formats: ["es"] },
      outDir: buildDir,
      rollupOptions: {
        external: ["vue", "pinia", "vue-router"],
        output: { inlineDynamicImports: true },
      },
    },
    esbuild: { target: "esnext" },
  });
  Providers = (await import(pathToFileURL(path.join(buildDir, "providers.mjs")).href)).default;
});

after(async () => {
  if (buildDir) await rm(buildDir, { force: true, recursive: true });
});

describe("provider probe and catalog removal", { concurrency: false }, () => {
  test("initial provider reads and DOM stay bounded with a huge selected catalog", async () => {
    const models = Array.from({ length: 1500 }, (_, index) => `model-${String(index).padStart(4, "0")}`);
    const mounted = await openProviders(models, { holdGets: false, failGets: false, token: "", pending: [] });
    try {
      await until("bounded rows", () => text(mounted.root).includes(models[0]!), () => detail(mounted.root, mounted.requests));
      const rows = walkHostNodes(mounted.root).filter(node => node.type === "tr" && className(node).includes("is-selected") === false && text(node).includes("model-"));
      assert.equal(rows.length, 50); assert.equal(text(mounted.root).includes(models[50]!), false);
      assert.deepEqual(mounted.requests.map(request => pathnameOf(request.url)), ["/pages/providers", "/pages/providers/p:opencode", "/pages/providers/d:dest-opencode/models"]);
    } finally { mounted.app.unmount(); }
  });
  test("a model bookmark asks the server for its located page and retains its URL", async () => {
    const models = Array.from({ length: 150 }, (_, index) => `model-${String(index).padStart(4, "0")}`);
    const target = models[125]!;
    const mounted = await openProviders(models, { holdGets: false, failGets: false, token: "", pending: [] }, { provider: "opencode", model: target });
    try {
      await until("located model", () => text(mounted.root).includes(target), () => detail(mounted.root, mounted.requests));
      assert.ok(mounted.requests.some(request => request.url.includes(`model=${target}`)));
      assert.equal(mounted.router.currentRoute.value.query.model, target);
      assert.equal(mounted.router.currentRoute.value.query.destination, "dest-opencode");
      assert.equal(mounted.requests.some(request => pathnameOf(request.url).endsWith("edit-detail")), false);
    } finally { mounted.app.unmount(); }
  });
  test("select all filtered uses the complete selected inventory without expanding DOM", async () => {
    const models = Array.from({ length: 80 }, (_, index) => `model-${String(index).padStart(4, "0")}`);
    const mounted = await openProviders(models, { holdGets: false, failGets: false, token: "", pending: [] });
    try {
      await until("model rows", () => text(mounted.root).includes(models[0]!), () => detail(mounted.root, mounted.requests));
      const header = walkHostNodes(mounted.root).find(node => className(node).includes("matrix-cell--select-header"));
      assert.ok(header);
      const checkbox = walkHostNodes(header).find(node => typeof node.props["onUpdate:checked"] === "function");
      assert.ok(checkbox); (checkbox.props["onUpdate:checked"] as (value: boolean) => void)(true); await settle();
      const banner = walkHostNodes(mounted.root).find(node => className(node) === "matrix-select-all"); assert.ok(banner);
      const selectAll = walkHostNodes(banner).find(node => node.type === "button" && typeof node.props.onClick === "function");
      assert.ok(selectAll); click(selectAll);
      await until("selected inventory", () => mounted.requests.some(request => pathnameOf(request.url).endsWith("edit-detail")), () => detail(mounted.root, mounted.requests));
      await untilQuiet(mounted.requests, 3); await settle();
      const toolbar = walkHostNodes(mounted.root).find(node => className(node) === "matrix-toolbar__select"); assert.ok(toolbar);
      const batch = walkHostNodes(toolbar).find(node => node.type === "button" && typeof node.props.onClick === "function" && !node.props.disabled); assert.ok(batch); click(batch);
      await until("global batch write", () => mounted.requests.some(request => request.method === "PUT"), () => detail(mounted.root, mounted.requests));
      const write = mounted.requests.find(request => request.method === "PUT")!;
      const selected = write.body!.overrides as { modelId: string }[];
      assert.equal(new Set(selected.map(row => row.modelId)).size, 80);
      assert.equal(text(mounted.root).includes(models[79]!), false);
    } finally { mounted.app.unmount(); }
  });
  test("an HTTP protocol receipt stays visible when the following page read fails", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: "HTTP_READ_FAILURE", pending: [], variant: "http" };
    const mounted = await openProviders([KEEP], gate);
    try {
      await until("HTTP row", () => text(mounted.root).includes(KEEP), () => detail(mounted.root, mounted.requests));
      const row = walkHostNodes(mounted.root).find(node => node.type === "tr" && text(node).includes(KEEP)); assert.ok(row);
      const control = walkHostNodes(row).find(node => node.props.class === "matrix-switch"); assert.ok(control);
      gate.failGets = true;
      (control.props["onUpdate:value"] as (value: boolean) => void)(false);
      await until("HTTP toggle receipt", () => mounted.requests.some(request => request.method === "PUT") && walkHostNodes(mounted.root).some(node => node.props.class === "matrix-switch" && node.props.value === false && !node.props.loading), () => detail(mounted.root, mounted.requests));
      assert.equal(mounted.requests.filter(request => request.method === "PUT").length, 1);
      assert.ok(text(mounted.root).includes(KEEP));
    } finally { mounted.app.unmount(); }
  });
  for (const variant of ["http", "refreshable"] as const) {
    test(`${variant} catalog refresh commits rows before a failed page revalidation`, async () => {
      const gate: Gate = { holdGets: false, failGets: false, token: "REFRESH_READ_FAILURE", pending: [], variant };
      const mounted = await openProviders([KEEP], gate);
      const added = variant === "http" ? "refreshed-http-model" : "refreshed-builtin-model";
      try {
        await until("refresh row", () => text(mounted.root).includes(KEEP), () => detail(mounted.root, mounted.requests));
        const actions = walkHostNodes(mounted.root).find(node => node.props.class === "providers-catalog-actions"); assert.ok(actions);
        const refresh = walkHostNodes(actions).find(node => node.type === "button"); assert.ok(refresh);
        gate.failGets = true; click(refresh);
        await until("catalog receipt", () => text(mounted.root).includes(added), () => detail(mounted.root, mounted.requests));
        assert.equal(mounted.requests.filter(request => request.method === "POST" && pathnameOf(request.url).endsWith("catalog/refresh")).length, 1);
        assert.equal(messages().some(message => message.type === "error"), false);
      } finally { mounted.app.unmount(); }
    });
  }
  for (const variant of ["http", "refreshable"] as const) {
    test(`a deferred ${variant} refresh cannot commit or notify after session replacement`, async () => {
      const gate: Gate = { holdGets: false, failGets: false, token: "", pending: [], variant, holdRefresh: true };
      const mounted = await openProviders([KEEP], gate);
      const session = useSessionStore();
      const auth = { authenticated: true, initialized: true, local: false, processGeneration: 42, revision: 12 };
      session.applyStatus(auth);
      try {
        const actions = walkHostNodes(mounted.root).find(node => node.props.class === "providers-catalog-actions"); assert.ok(actions);
        const refresh = walkHostNodes(actions).find(node => node.type === "button"); assert.ok(refresh); click(refresh);
        await until("held refresh", () => gate.pending.length === 1, () => detail(mounted.root, mounted.requests));
        session.dropSession(); session.applyStatus({ ...auth, revision: 13 });
        useControlPlaneStore().sync({ revision: 13, processGeneration: 42 });
        gate.readRevision = 13;
        const page = useProviderPageStore();
        await page.loadDetail("d:dest-opencode"); await page.loadModels("d:dest-opencode", {}); await settle();
        assert.equal(page.models?.revision.revision, 13);
        const requestsBeforeReceipt = mounted.requests.length;
        const messagesBeforeReceipt = messages().length;
        releasePending(gate);
        for (let turn = 0; turn < 12; turn++) { await new Promise(resolve => setImmediate(resolve)); await settle(); }
        assert.equal(mounted.requests.length, requestsBeforeReceipt, "obsolete refresh must not revalidate the new session");
        assert.equal(messages().length, messagesBeforeReceipt, "obsolete refresh must not notify the new session");
        assert.deepEqual(page.models?.models.map(row => row.publicModel), [KEEP]);
        assert.equal(page.models?.total, 1);
      } finally { gate.holdRefresh = false; releasePending(gate); mounted.app.unmount(); }
    });
  }
  test("a header refresh during preparation permits an operation against the current matching view", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: "", pending: [] };
    const mounted = await openProviders([PROBE], gate);
    try {
      await until("probe row", () => text(mounted.root).includes(PROBE), () => detail(mounted.root, mounted.requests));
      gate.holdReadSuffixes = ["/edit-detail"];
      click(confirmFor(mounted.root, PROBE, "probe"));
      await until("pending operation detail", () => gate.pending.length === 1, () => detail(mounted.root, mounted.requests));
      gate.headerVersion = gate.editVersion = "refreshed-view";
      await useProviderPageStore().loadDetail("d:dest-opencode");
      gate.holdReadSuffixes = [];
      releasePending(gate);
      await until("operation submitted", () => mounted.requests.some(request => request.method === "POST"), () => detail(mounted.root, mounted.requests));
    } finally { releasePending(gate); mounted.app.unmount(); }
  });
  test("changed selected read identity requires reapply instead of an automatic model write", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: "", pending: [] };
    const mounted = await openProviders([PROBE], gate);
    try {
      await until("probe row", () => text(mounted.root).includes(PROBE), () => detail(mounted.root, mounted.requests));
      gate.editVersion = "changed-read-version"; click(confirmFor(mounted.root, PROBE, "probe"));
      await until("fresh edit detail", () => mounted.requests.some(request => pathnameOf(request.url).endsWith("edit-detail")), () => detail(mounted.root, mounted.requests));
      await untilQuiet(mounted.requests, 3); await settle();
      assert.equal(mounted.requests.some(request => request.method === "POST"), false);
      assert.ok(mounted.requests.filter(request => pathnameOf(request.url) === "/pages/providers").length > 1);
    } finally { mounted.app.unmount(); }
  });
  for (const stale of ["lowerRevision", "oldProcess"] as const) {
    test(`a delayed ${stale} edit detail cannot seed entities or open an editor`, async () => {
      const accountId = `rejected-edit-${stale}`;
      const gate: Gate = { holdGets: false, failGets: false, token: "", pending: [], variant: "http",
        editAccounts: [v3AccountDto(accountId)] };
      const mounted = await openProviders([KEEP], gate);
      try {
        const header = walkHostNodes(mounted.root).find(node => className(node) === "providers-catalog-head"); assert.ok(header);
        const edit = walkHostNodes(header).find(node => node.type === "button" && typeof node.props.onClick === "function" && "secondary" in node.props); assert.ok(edit);
        gate.holdReadSuffixes = ["/edit-detail"]; click(edit);
        await until("pending edit detail", () => gate.pending.length === 1, () => detail(mounted.root, mounted.requests));
        useControlPlaneStore().sync(stale === "lowerRevision"
          ? { revision: 13, processGeneration: 42 } : { revision: 1, processGeneration: 43 });
        gate.holdReadSuffixes = []; releasePending(gate);
        await untilQuiet(mounted.requests, 3); await settle();
        assert.equal(useAccountsStore().byId.has(accountId), false);
        assert.equal(useDestinationsStore().byId.has("dest-opencode"), false);
        assert.equal(walkHostNodes(mounted.root).some(node => node.props.role === "dialog" && className(node) === "destination-edit-modal"), false);
        assert.equal(mounted.requests.some(request => request.method !== "GET"), false);
      } finally { releasePending(gate); mounted.app.unmount(); }
    });
  }
  test("a rejected rail response does not select or load a provider", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: "", pending: [] };
    const mounted = await openProviders([KEEP], gate);
    try {
      const before = mounted.requests.length;
      const rail = walkHostNodes(mounted.root).find(node => "simple" in node.props && typeof node.props["onUpdate:page"] === "function"); assert.ok(rail);
      gate.holdReadSuffixes = ["/pages/providers"];
      (rail.props["onUpdate:page"] as (page: number) => void)(2);
      await until("pending rail", () => gate.pending.length === 1, () => detail(mounted.root, mounted.requests));
      useControlPlaneStore().sync({ revision: 13, processGeneration: 42 });
      gate.holdReadSuffixes = []; releasePending(gate);
      await untilQuiet(mounted.requests, before); await settle();
      assert.deepEqual(mounted.requests.slice(before).map(request => pathnameOf(request.url)), ["/pages/providers"]);
      assert.equal(mounted.router.currentRoute.value.query.destination, "dest-opencode");
      assert.ok(text(mounted.root).includes(KEEP));
    } finally { releasePending(gate); mounted.app.unmount(); }
  });
  test("rejected model rows cannot trigger header reconciliation", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: "", pending: [] };
    const mounted = await openProviders([KEEP], gate);
    try {
      const before = mounted.requests.length;
      const search = walkHostNodes(mounted.root).find(node => className(node) === "matrix-search"); assert.ok(search);
      gate.holdReadSuffixes = ["/models"]; gate.modelVersion = "unaccepted-version";
      (search.props["onUpdate:value"] as (value: string) => void)(KEEP);
      await until("pending model rows", () => gate.pending.length === 1, () => detail(mounted.root, mounted.requests));
      useControlPlaneStore().sync({ revision: 13, processGeneration: 42 });
      gate.holdReadSuffixes = []; releasePending(gate);
      await untilQuiet(mounted.requests, before); await settle();
      assert.deepEqual(mounted.requests.slice(before).map(request => pathnameOf(request.url)), ["/pages/providers/d:dest-opencode/models"]);
      assert.ok(text(mounted.root).includes(KEEP));
    } finally { releasePending(gate); mounted.app.unmount(); }
  });
  test("header reconciliation cannot restore a superseded model search", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: "", pending: [] };
    const mounted = await openProviders([KEEP], gate);
    try {
      const search = walkHostNodes(mounted.root).find(node => className(node) === "matrix-search"); assert.ok(search);
      gate.holdReadSuffixes = ["/pages/providers/d:dest-opencode"]; gate.modelVersion = "changed-version";
      (search.props["onUpdate:value"] as (value: string) => void)(KEEP);
      await until("pending header reconciliation", () => gate.pending.length === 1, () => detail(mounted.root, mounted.requests));
      gate.modelVersion = "page12";
      (search.props["onUpdate:value"] as (value: string) => void)("unmatched-search");
      await until("latest model search", () => mounted.requests.some(request => request.url.includes("search=unmatched-search")), () => detail(mounted.root, mounted.requests));
      await settle(); gate.holdReadSuffixes = []; releasePending(gate);
      await untilQuiet(mounted.requests, 3); await settle();
      const modelReads = mounted.requests.filter(request => pathnameOf(request.url).endsWith("/models"));
      assert.equal(modelReads.length, 3);
      assert.equal(new URL(modelReads.at(-1)!.url, "http://test.local").searchParams.get("search"), "unmatched-search");
      assert.equal(walkHostNodes(mounted.root).some(node => node.type === "tr" && text(node).includes(KEEP)), false);
    } finally { releasePending(gate); mounted.app.unmount(); }
  });
  test("a successful builtin probe stays success when the later projection read fails", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: PROJECTION, pending: [] };
    const mounted = await openProviders([PROBE], gate);
    try {
      await until("probe row", () => text(mounted.root).includes(PROBE), () => detail(mounted.root, mounted.requests));
      gate.failGets = true;
      click(confirmFor(mounted.root, PROBE, "probe"));
      await until(
        "probe success",
        () => messages().some((message) => message.type === "success")
          && alerts(mounted.root).some((node) => className(node).includes("providers-probe-summary")),
        () => detail(mounted.root, mounted.requests),
      );
      await settle();
      assert.equal(messages().some((message) => message.type === "error"), false, messageText("error"));
      assert.equal(mounted.requests.filter((request) => request.method === "POST" && pathnameOf(request.url).endsWith("/protocol-probes")).length, 1);
      const summaries = alerts(mounted.root).filter((node) => className(node).includes("providers-probe-summary"));
      assert.ok(summaries.length > 0);
      assert.ok(summaries.every((node) => node.props.type === "success"));
      assert.equal(alerts(mounted.root).some((node) => node.props.type === "error"), false);
      assert.match(`${messageText("warning")}\n${alerts(mounted.root).map((node) => text(node)).join("\n")}`, new RegExp(PROJECTION));
    } finally {
      releasePending(gate, new Error(PROJECTION));
      mounted.app.unmount();
    }
  });

  test("a stalled projection read does not turn a successful probe into a failure", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: PROJECTION, pending: [] };
    const mounted = await openProviders([PROBE], gate);
    try {
      await until("probe row", () => text(mounted.root).includes(PROBE), () => detail(mounted.root, mounted.requests));
      const before = mounted.requests.length;
      gate.holdGets = true;
      click(confirmFor(mounted.root, PROBE, "probe"));
      await until(
        "probe success while the projection read is pending",
        () => messages().some((message) => message.type === "success")
          && gate.pending.length > 0
          && mounted.requests.slice(before).some((request) => pathnameOf(request.url) === "/pages/providers")
          && alerts(mounted.root).some((node) => className(node).includes("providers-probe-summary")),
        () => detail(mounted.root, mounted.requests),
      );
      const summaries = alerts(mounted.root).filter((node) => className(node).includes("providers-probe-summary"));
      assert.ok(summaries.length > 0);
      assert.ok(summaries.every((node) => node.props.type === "success"));
      assert.equal(messages().some((message) => message.type === "error"), false);
      releasePending(gate, new Error(PROJECTION));
      gate.holdGets = false;
      gate.failGets = true;
      await until(
        "projection failure after the probe receipt",
        () => messageText("warning").includes(PROJECTION) || alerts(mounted.root).some((node) => text(node).includes(PROJECTION)),
        () => detail(mounted.root, mounted.requests),
      );
      assert.equal(messages().some((message) => message.type === "error"), false, messageText("error"));
      assert.ok(alerts(mounted.root).filter((node) => className(node).includes("providers-probe-summary")).every((node) => node.props.type === "success"));
      assert.equal(alerts(mounted.root).some((node) => node.props.type === "error"), false);
      assert.equal(mounted.requests.filter((request) => request.method === "POST" && pathnameOf(request.url).endsWith("/protocol-probes")).length, 1);
    } finally {
      releasePending(gate, new Error(PROJECTION));
      mounted.app.unmount();
    }
  });

  test("a confirmed model removal stays committed when the contracts reload fails", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: RELOAD, pending: [] };
    const mounted = await openProviders([DROP, KEEP], gate);
    try {
      await until("removal rows", () => text(mounted.root).includes(DROP) && text(mounted.root).includes(KEEP), () => detail(mounted.root, mounted.requests));
      gate.failGets = true;
      click(confirmFor(mounted.root, DROP, "remove"));
      await until(
        "removal receipt",
        () => messages().some((message) => message.type === "success") && !text(mounted.root).includes(DROP),
        () => detail(mounted.root, mounted.requests),
      );
      await settle();
      assert.ok(text(mounted.root).includes(KEEP));
      assert.equal(messages().some((message) => message.type === "error"), false, messageText("error"));
      assert.equal(mounted.requests.filter((request) => request.method === "POST" && pathnameOf(request.url).endsWith("/catalog/remove")).length, 1);
      assert.match(`${messageText("warning")}\n${alerts(mounted.root).map((node) => text(node)).join("\n")}`, new RegExp(RELOAD));
      await useProvidersStore().loadContracts().catch(() => undefined);
      assert.equal(mounted.requests.filter((request) => request.method === "POST" && pathnameOf(request.url).endsWith("/catalog/remove")).length, 1);
      assert.equal(text(mounted.root).includes(DROP), false);
      assert.ok(text(mounted.root).includes(KEEP));
    } finally {
      releasePending(gate);
      mounted.app.unmount();
    }
  });

  test("a confirmed model removal stays committed while the contracts reload is stalled", async () => {
    const gate: Gate = { holdGets: false, failGets: false, token: RELOAD, pending: [] };
    const mounted = await openProviders([DROP, KEEP], gate);
    try {
      await until("removal rows", () => text(mounted.root).includes(DROP) && text(mounted.root).includes(KEEP), () => detail(mounted.root, mounted.requests));
      const before = mounted.requests.length;
      gate.holdGets = true;
      click(confirmFor(mounted.root, DROP, "remove"));
      await until(
        "removal receipt while the reload is pending",
        () => messages().some((message) => message.type === "success")
          && !text(mounted.root).includes(DROP)
          && gate.pending.length > 0
          && mounted.requests.slice(before).some((request) => pathnameOf(request.url) === "/pages/providers"),
        () => detail(mounted.root, mounted.requests),
      );
      assert.ok(text(mounted.root).includes(KEEP));
      assert.equal(messages().some((message) => message.type === "error"), false, messageText("error"));
      assert.equal(mounted.requests.filter((request) => request.method === "POST" && pathnameOf(request.url).endsWith("/catalog/remove")).length, 1);
      releasePending(gate, new Error(RELOAD));
      gate.holdGets = false;
      await settle();
      assert.equal(messages().some((message) => message.type === "error"), false, messageText("error"));
      assert.equal(mounted.requests.filter((request) => request.method === "POST" && pathnameOf(request.url).endsWith("/catalog/remove")).length, 1);
      assert.equal(text(mounted.root).includes(DROP), false);
      assert.ok(text(mounted.root).includes(KEEP));
    } finally {
      releasePending(gate, new Error(RELOAD));
      mounted.app.unmount();
    }
  });
});
