import assert from "node:assert/strict";
import test from "node:test";
import {
  CORE_APP_NAVIGATION,
  EXTENSION_APP_NAVIGATION,
  PROVIDER_OTHER_TAB,
  accountAddDeepLinkFromProviderAdd,
  accountAddQueryValue,
  appViewRoute,
  applyAppViewSearchParams,
  isLegacyPricingView,
  legacyAppHash,
  normalizeProviderDetailTab,
  readAccountAddDeepLink,
  readProviderPageQuery,
  resolveAppViewKey,
  routeQuerySearch,
} from "./app-navigation.ts";

test("navigation metadata keeps the fixed core order and exposes CPA under Extensions", () => {
  assert.deepEqual(
    CORE_APP_NAVIGATION.map(({ key }) => key),
    ["dashboard", "keys", "accounts", "providers", "aliases", "applications", "logs", "settings"],
  );
  assert.deepEqual(EXTENSION_APP_NAVIGATION.map(({ key }) => key), ["cpa"]);
});

test("the applications view key resolves and keeps its child-tab parameter", () => {
  assert.equal(resolveAppViewKey("applications"), "applications");
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=applications&app=dsh"),
    "applications",
  );
  assert.equal(url.searchParams.get("view"), "applications");
  assert.equal(url.searchParams.get("app"), "dsh");
});

test("leaving applications strips the child-tab parameter", () => {
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=applications&app=dsh"),
    "logs",
  );
  assert.equal(url.searchParams.get("view"), "logs");
  assert.equal(url.searchParams.get("app"), null);
});

test("legacy pricing view keys resolve to providers without inventing a second entry", () => {
  assert.equal(isLegacyPricingView("pricing"), true);
  assert.equal(resolveAppViewKey("pricing"), "providers");
  assert.equal(resolveAppViewKey("providers"), "providers");
  assert.equal(resolveAppViewKey("aliases"), "aliases");
  assert.equal(resolveAppViewKey("accounts"), "accounts");
  assert.equal(resolveAppViewKey("cpa"), "cpa");
  assert.equal(resolveAppViewKey("not-a-view"), "dashboard");
});

test("provider deep-link query fields round-trip on the providers view", () => {
  assert.deepEqual(readProviderPageQuery("?view=providers&provider=command-code"), {
    connection: null,
    provider: "command-code",
    destination: null,
    tab: null,
    add: false,
    preset: null,
  });
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=accounts"),
    "providers",
    { provider: "minimax", tab: "pricing" },
  );
  assert.equal(url.searchParams.get("view"), "providers");
  assert.equal(url.searchParams.get("provider"), "minimax");
  assert.equal(url.searchParams.get("connection"), null);
  assert.equal(url.searchParams.get("tab"), "pricing");
  assert.deepEqual(readProviderPageQuery(url.search), {
    connection: null,
    provider: "minimax",
    destination: null,
    tab: "pricing",
    add: false,
    preset: null,
  });
});

test("writing a connection id emits connection= and strips a leftover provider=", () => {
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=providers&provider=opencode"),
    "providers",
    { connection: "uuid-open", tab: "settings" },
  );
  assert.equal(url.searchParams.get("connection"), "uuid-open");
  assert.equal(url.searchParams.get("provider"), null);
  assert.deepEqual(readProviderPageQuery(url.search), {
    connection: "uuid-open",
    provider: null,
    destination: null,
    tab: "settings",
    add: false,
    preset: null,
  });
});

test("provider= and destination= can coexist on read so selection can rank them", () => {
  assert.deepEqual(
    readProviderPageQuery("?view=providers&destination=dest-a&provider=lab-http"),
    {
      connection: null,
      provider: "lab-http",
      destination: "dest-a",
      tab: null,
      add: false,
      preset: null,
    },
  );
});

test("the add flow round-trips with and without a preset", () => {
  const browse = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=accounts"),
    "providers",
    { add: true },
  );
  assert.deepEqual(readProviderPageQuery(browse.search), {
    connection: null,
    provider: null,
    destination: null,
    tab: null,
    add: true,
    preset: null,
  });
  const form = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=accounts"),
    "providers",
    { add: true, preset: "openai" },
  );
  assert.equal(form.searchParams.get("add"), "1");
  assert.equal(form.searchParams.get("preset"), "openai");
  assert.deepEqual(readProviderPageQuery(form.search), {
    connection: null,
    provider: null,
    destination: null,
    tab: null,
    add: true,
    preset: "openai",
  });
});

test("legacy provider scope links map onto the new query", () => {
  assert.deepEqual(
    readProviderPageQuery("?view=providers&scope_kind=provider&scope_id=command-code"),
    { connection: null, provider: "command-code", destination: null, tab: null, add: false, preset: null },
  );
  assert.deepEqual(
    readProviderPageQuery("?view=providers&scope_kind=dynamic&scope_id=acme"),
    { connection: null, provider: "acme", destination: null, tab: null, add: false, preset: null },
  );
  assert.deepEqual(
    readProviderPageQuery("?view=providers&scope_kind=preset&scope_id=openai"),
    { connection: null, provider: null, destination: null, tab: null, add: true, preset: "openai" },
  );
  // Account-owned custom endpoint scopes have no provider row; degrade to the
  // default selection instead of failing.
  assert.deepEqual(
    readProviderPageQuery("?view=providers&scope_kind=custom_endpoint&scope_id=acc-9"),
    { connection: null, provider: null, destination: null, tab: null, add: false, preset: null },
  );
  // An explicit new-style parameter always wins over a stale legacy one.
  assert.deepEqual(
    readProviderPageQuery("?view=providers&provider=kimi&scope_kind=provider&scope_id=opencode"),
    { connection: null, provider: "kimi", destination: null, tab: null, add: false, preset: null },
  );
});

test("legacy provider tab values map onto the detail tabs", () => {
  assert.equal(normalizeProviderDetailTab("catalog"), "models");
  assert.equal(normalizeProviderDetailTab(PROVIDER_OTHER_TAB), "settings");
  assert.equal(normalizeProviderDetailTab("models"), "models");
  assert.equal(normalizeProviderDetailTab("pricing"), "pricing");
  assert.equal(normalizeProviderDetailTab("settings"), "settings");
  assert.equal(normalizeProviderDetailTab("nope"), null);
  assert.equal(normalizeProviderDetailTab(null), null);
  assert.deepEqual(
    readProviderPageQuery("?view=providers&scope_kind=provider&scope_id=opencode&tab=other"),
    { connection: null, provider: "opencode", destination: null, tab: "settings", add: false, preset: null },
  );
  assert.deepEqual(
    readProviderPageQuery("?view=providers&provider=opencode&tab=catalog"),
    { connection: null, provider: "opencode", destination: null, tab: "models", add: false, preset: null },
  );
});

test("leaving providers strips provider query fields", () => {
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=providers&provider=opencode&connection=uuid-open&tab=settings&add=1&preset=openai"),
    "logs",
  );
  assert.equal(url.searchParams.get("view"), "logs");
  assert.equal(url.searchParams.get("connection"), null);
  assert.equal(url.searchParams.get("provider"), null);
  assert.equal(url.searchParams.get("tab"), null);
  assert.equal(url.searchParams.get("add"), null);
  assert.equal(url.searchParams.get("preset"), null);
  assert.equal(url.searchParams.get("destination"), null);
});

test("writing the providers view never re-emits legacy scope parameters", () => {
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=providers&scope_kind=provider&scope_id=opencode&tab=other"),
    "providers",
    { provider: "opencode", tab: "settings" },
  );
  assert.equal(url.searchParams.get("scope_kind"), null);
  assert.equal(url.searchParams.get("scope_id"), null);
  assert.equal(url.searchParams.get("provider"), "opencode");
  assert.equal(url.searchParams.get("tab"), "settings");
  // Leaving the view strips any stale legacy parameter too.
  const logs = applyAppViewSearchParams(url, "logs");
  assert.equal(logs.searchParams.get("scope_kind"), null);
  assert.equal(logs.searchParams.get("tab"), null);
});

test("leaving Accounts strips a stale account deep-link parameter", () => {
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=accounts&account_id=custom-1"),
    "providers",
  );
  assert.equal(url.searchParams.get("view"), "providers");
  assert.equal(url.searchParams.get("account_id"), null);
});

test("the add-account deep link reads on Accounts and is stripped elsewhere", () => {
  // Legacy bookmarks carry the retired chooser id; it maps to the current one.
  assert.deepEqual(readAccountAddDeepLink("?view=accounts&add=custom-endpoint"), { optionId: "custom" });
  assert.deepEqual(readAccountAddDeepLink("?view=accounts&add=custom"), { optionId: "custom" });
  // Other values pass through unchanged.
  assert.deepEqual(readAccountAddDeepLink("?view=accounts&add=opencode"), { optionId: "opencode" });
  assert.equal(readAccountAddDeepLink("?view=accounts"), null);
  // The shared Add entry from Providers opens the chooser without a preselect.
  assert.deepEqual(readAccountAddDeepLink("?view=accounts&add=1"), { optionId: null });
  assert.deepEqual(readAccountAddDeepLink("?view=accounts&add="), { optionId: null });
  // Wrong or missing view never qualifies, even with the parameter present.
  assert.equal(readAccountAddDeepLink("?view=providers&add=custom-endpoint"), null);
  assert.equal(readAccountAddDeepLink("?view=keys&add=custom-endpoint"), null);
  assert.equal(readAccountAddDeepLink("?add=custom-endpoint"), null);
  assert.equal(readAccountAddDeepLink(""), null);
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=accounts&add=custom-endpoint"),
    "logs",
  );
  assert.equal(url.searchParams.get("view"), "logs");
  assert.equal(url.searchParams.get("add"), null);
  const stay = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=providers&provider=openai"),
    "accounts",
  );
  assert.equal(stay.searchParams.get("view"), "accounts");
  assert.equal(stay.searchParams.get("provider"), null);
});

test("Providers add maps onto the shared Accounts chooser", () => {
  assert.deepEqual(accountAddDeepLinkFromProviderAdd(null), { optionId: "custom" });
  assert.deepEqual(accountAddDeepLinkFromProviderAdd("openai"), { optionId: "preset:openai" });
  assert.deepEqual(accountAddDeepLinkFromProviderAdd("manual"), { optionId: "preset:manual" });
  assert.equal(accountAddQueryValue({ optionId: null }), "1");
  assert.equal(accountAddQueryValue({ optionId: "preset:openai" }), "preset:openai");
});

test("a destination-only Providers scope writes destination= and strips on leave", () => {
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=accounts"),
    "providers",
    { destination: "dest-site" },
  );
  assert.equal(url.searchParams.get("destination"), "dest-site");
  assert.equal(url.searchParams.get("connection"), null);
  assert.deepEqual(readProviderPageQuery(url.search), {
    connection: null,
    provider: null,
    destination: "dest-site",
    tab: null,
    add: false,
    preset: null,
  });
  const withConnection = applyAppViewSearchParams(url, "providers", {
    connection: "uuid-open",
    destination: "dest-site",
  });
  assert.equal(withConnection.searchParams.get("connection"), "uuid-open");
  assert.equal(withConnection.searchParams.get("destination"), null);
  const logs = applyAppViewSearchParams(withConnection, "logs");
  assert.equal(logs.searchParams.get("destination"), null);
});

test("the Accounts add deep link survives a providers write untouched", () => {
  // `add` is shared between the Accounts chooser deep link and the Providers
  // add flow; writing one view's params must not clear the other's.
  const url = applyAppViewSearchParams(
    new URL("http://127.0.0.1:9042/dashboard/?view=accounts&add=custom-endpoint"),
    "accounts",
  );
  assert.equal(url.searchParams.get("add"), "custom-endpoint");
});

test("appViewRoute targets the named route with the legacy query semantics, minus view", () => {
  assert.deepEqual(appViewRoute("accounts"), { name: "accounts", query: {} });
  assert.deepEqual(appViewRoute("providers", { destination: "dest-site", tab: "pricing" }), {
    name: "providers",
    query: { destination: "dest-site", tab: "pricing" },
  });
  assert.deepEqual(appViewRoute("accounts", undefined, { account_id: "acct-1" }), {
    name: "accounts",
    query: { account_id: "acct-1" },
  });
  // A scoped providers write never leaks connection/destination across.
  assert.deepEqual(appViewRoute("providers", { connection: "uuid-open" }), {
    name: "providers",
    query: { connection: "uuid-open" },
  });
});

test("routeQuerySearch round-trips into the legacy readers", () => {
  const search = routeQuerySearch("providers", { destination: "dest-site", tab: "pricing" });
  assert.deepEqual(readProviderPageQuery(search), {
    connection: null,
    provider: null,
    destination: "dest-site",
    tab: "pricing",
    add: false,
    preset: null,
  });
  // The view name rides along so view-scoped readers (Accounts add) still match.
  assert.deepEqual(readAccountAddDeepLink(routeQuerySearch("accounts", { add: "custom" })), {
    optionId: "custom",
  });
  assert.equal(readAccountAddDeepLink(routeQuerySearch("providers", { add: "custom" })), null);
  // Repeated or empty values never reach the readers.
  assert.equal(routeQuerySearch("logs", {}), "?view=logs");
});

test("legacyAppHash converts pre-router URLs and leaves routed ones untouched", () => {
  assert.equal(legacyAppHash("http://127.0.0.1:9042/dashboard/"), null);
  assert.equal(legacyAppHash("http://127.0.0.1:9042/dashboard/#/accounts"), null);
  assert.equal(
    legacyAppHash("http://127.0.0.1:9042/dashboard/?view=accounts&account_id=acct-1"),
    "#/accounts?account_id=acct-1",
  );
  assert.equal(
    legacyAppHash("http://127.0.0.1:9042/dashboard/?view=pricing"),
    "#/providers",
  );
  assert.equal(
    legacyAppHash("http://127.0.0.1:9042/dashboard/?view=applications&app=dsh"),
    "#/applications?app=dsh",
  );
  assert.equal(
    legacyAppHash("http://127.0.0.1:9042/dashboard/?view=browser#session=abc%2F123"),
    "#/browser?session=abc%2F123",
  );
});
