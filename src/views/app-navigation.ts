import type { MessageKey } from "../i18n/index.ts";

export const APP_NAVIGATION_GROUPS = {
  core: { key: "core" },
  extensions: { key: "extensions", label: "扩展" },
} as const satisfies Record<string, { key: string; label?: MessageKey }>;

export type AppNavigationGroup = keyof typeof APP_NAVIGATION_GROUPS;
export type AppNavigationIcon =
  | "dashboard"
  | "keys"
  | "accounts"
  | "providers"
  | "aliases"
  | "applications"
  | "logs"
  | "settings"
  | "cpa";

export interface AppNavigationItem {
  key: string;
  label: MessageKey;
  icon: AppNavigationIcon;
  group: AppNavigationGroup;
}

// This is the single navigation registration source for desktop, mobile, and
// the page title. Browser remains an overlay rather than a menu entry.
export const APP_NAVIGATION = [
  { key: "dashboard", label: "仪表盘", icon: "dashboard", group: "core" },
  { key: "keys", label: "接入 Key", icon: "keys", group: "core" },
  { key: "accounts", label: "账号", icon: "accounts", group: "core" },
  { key: "providers", label: "供应商", icon: "providers", group: "core" },
  { key: "aliases", label: "别名", icon: "aliases", group: "core" },
  { key: "applications", label: "应用", icon: "applications", group: "core" },
  { key: "logs", label: "日志", icon: "logs", group: "core" },
  { key: "settings", label: "设置", icon: "settings", group: "core" },
  { key: "cpa", label: "CPA", icon: "cpa", group: "extensions" },
] as const satisfies readonly AppNavigationItem[];

export type AppNavigationViewKey = (typeof APP_NAVIGATION)[number]["key"];
export type AppViewKey = AppNavigationViewKey | "browser";

const APP_VIEW_KEYS: readonly AppViewKey[] = [
  ...APP_NAVIGATION.map(({ key }) => key),
  "browser",
];

export const CORE_APP_NAVIGATION = APP_NAVIGATION.filter(({ group }) => group === "core");
export const EXTENSION_APP_NAVIGATION = APP_NAVIGATION.filter(({ group }) => group === "extensions");

const LEGACY_PRICING_VIEW = "pricing";
const PROVIDERS_VIEW: AppViewKey = "providers";
/** Legacy Providers tab value; deep links carrying it resolve to Settings. */
export const PROVIDER_OTHER_TAB = "other";
const LEGACY_PROVIDER_CATALOG_TAB = "catalog";

const viewKeySet = new Set<string>(APP_VIEW_KEYS);

export const PROVIDER_DETAIL_TABS = ["models", "pricing", "settings"] as const;
export type ProviderDetailTab = (typeof PROVIDER_DETAIL_TABS)[number];

/**
 * Providers view deep link. `connection` is a V4 connection id; a leftover
 * `provider` catalog id is still accepted on write by callers that only know
 * the legacy identity (mapped on read). `add` opens the add flow (`preset`
 * picks the embedded form's preset, or the "manual" sentinel).
 */
export interface ProviderScopeQuery {
  connection?: string;
  provider?: string;
  tab?: ProviderDetailTab;
  add?: boolean;
  preset?: string;
}

/** Normalized Providers query, with legacy scope_kind/scope_id/tab mapped. */
export interface ProviderPageQuery {
  connection: string | null;
  provider: string | null;
  tab: ProviderDetailTab | null;
  add: boolean;
  preset: string | null;
}

export function isLegacyPricingView(raw: string | null | undefined): boolean {
  return raw === LEGACY_PRICING_VIEW;
}

export function resolveAppViewKey(raw: string | null | undefined): AppViewKey {
  if (!raw) return "dashboard";
  if (isLegacyPricingView(raw) || raw === PROVIDERS_VIEW) return "providers";
  return viewKeySet.has(raw) ? raw as AppViewKey : "dashboard";
}

export function normalizeProviderDetailTab(raw: string | null | undefined): ProviderDetailTab | null {
  if (!raw) return null;
  if (raw === LEGACY_PROVIDER_CATALOG_TAB) return "models";
  if (raw === PROVIDER_OTHER_TAB) return "settings";
  return (PROVIDER_DETAIL_TABS as readonly string[]).includes(raw)
    ? raw as ProviderDetailTab
    : null;
}

/**
 * Reads the Providers query. Legacy `scope_kind`/`scope_id` links keep
 * working: `provider` and `dynamic` scopes map to `provider=<id>`, `preset`
 * maps to the add flow, and account-owned `custom_endpoint` scopes degrade
 * to the default selection (their matrix lives on Accounts).
 */
export function readProviderPageQuery(search: string): ProviderPageQuery {
  const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
  const connection = params.get("connection");
  let provider = params.get("provider");
  let add = params.get("add") === "1";
  let preset = params.get("preset");
  const legacyKind = params.get("scope_kind");
  const legacyId = params.get("scope_id");
  if (!provider && legacyKind && legacyId) {
    if (legacyKind === "provider" || legacyKind === "dynamic") {
      provider = legacyId;
    } else if (legacyKind === "preset") {
      add = true;
      preset = preset ?? legacyId;
    }
  }
  return {
    connection,
    provider,
    tab: normalizeProviderDetailTab(params.get("tab")),
    add,
    preset,
  };
}

export function readAccountDeepLink(search: string): string | null {
  const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
  return params.get("account_id");
}

/** Legacy chooser id for built-in Custom API, from before ids became provider ids. */
const LEGACY_CUSTOM_ENDPOINT_OPTION_ID = "custom-endpoint";

/**
 * One-shot "open Add Account with this chooser option" deep link (Accounts
 * view only). Only an explicit Accounts view qualifies — a missing or
 * different view returns null. The legacy `custom-endpoint` value maps to the
 * current Custom API chooser id so old bookmarks keep working; every other
 * value passes through unchanged. Consumers must delete the parameter on use
 * so a reload or a close/reopen cycle never reopens the modal.
 */
export function readAccountAddDeepLink(search: string): string | null {
  const params = new URLSearchParams(search.startsWith("?") ? search.slice(1) : search);
  if (resolveAppViewKey(params.get("view")) !== "accounts") return null;
  const add = params.get("add");
  return add === LEGACY_CUSTOM_ENDPOINT_OPTION_ID ? "custom" : add;
}

export function applyAppViewSearchParams(
  url: URL,
  view: AppViewKey,
  scope?: ProviderScopeQuery | null,
): URL {
  url.searchParams.set("view", view);
  if (view !== "accounts") {
    url.searchParams.delete("account_id");
    if (view !== "providers") url.searchParams.delete("add");
  }
  // The Applications child tab (`app=dsh`) is scoped to its own view.
  if (view !== "applications") url.searchParams.delete("app");
  // Legacy Providers parameters are never written anymore, only mapped on read.
  url.searchParams.delete("scope_kind");
  url.searchParams.delete("scope_id");
  if (view !== "providers") {
    url.searchParams.delete("connection");
    url.searchParams.delete("provider");
    url.searchParams.delete("preset");
    url.searchParams.delete("tab");
    return url;
  }
  if (scope === undefined) return url;
  if (scope === null) {
    url.searchParams.delete("connection");
    url.searchParams.delete("provider");
    url.searchParams.delete("tab");
    url.searchParams.delete("add");
    url.searchParams.delete("preset");
    return url;
  }
  if (scope.connection) {
    url.searchParams.set("connection", scope.connection);
    url.searchParams.delete("provider");
  } else {
    url.searchParams.delete("connection");
    if (scope.provider) url.searchParams.set("provider", scope.provider);
    else url.searchParams.delete("provider");
  }
  if (scope.tab) url.searchParams.set("tab", scope.tab);
  else url.searchParams.delete("tab");
  if (scope.add) url.searchParams.set("add", "1");
  else url.searchParams.delete("add");
  if (scope.preset) url.searchParams.set("preset", scope.preset);
  else url.searchParams.delete("preset");
  return url;
}
