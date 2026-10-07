import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { pagesApi, type AccountsPage, type AccountDetail, type AccountCardCredentialsPage, type AccountPageCard, type AccountPageRow } from "../api/pages.ts";
import { useAccountPageStore, normalizeAccountsQuery } from "./accountPage.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

const original = { accounts: pagesApi.accounts, accountDetail: pagesApi.accountDetail, accountCredentials: pagesApi.accountCredentials, refreshAccount: pagesApi.refreshAccount };
afterEach(() => Object.assign(pagesApi, original));
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function snapshot(version: string, processGeneration = 1): AccountsPage {
  return { revision: { revision: 1, processGeneration, pricingRevision: "test" }, readVersion: version,
    asOf: "2026-10-07T00:00:00Z", validUntil: null, totalCards: 0, totalCredentials: 0, matchedCards: 0, matchedCredentials: 0,
    cards: [], offset: 0, limit: 10, hasMore: false, errors: [], planOptions: [], planFilters: [], routingMode: "strict-priority", conversationSticky: false };
}
function setup() { setActivePinia(createPinia()); return useAccountPageStore(); }

function pageRow(id: string, status = "enabled"): AccountPageRow {
  return { account: null, status, routeAvailable: true, modelCount: 0,
    inferenceEndpointUrl: null, platformLink: null, actions: [], billing: null,
    refresh: { supported: false, observedAt: null, freshUntil: null, nextAllowedAt: null },
    tags: { credentialCount: 1, bindingDisabled: false, quotaShareName: null, quotaShareCount: 0, duplicateName: false },
    credential: { id, legacyAccountId: id, destinationId: "destination", name: "Synthetic",
      authState: "unknown", hasSecret: true, keyPreview: "••••", enabled: true, routingRank: 0, notes: null, lastError: null,
      onboardingTask: null, purchaseDate: null, quotaPoolId: null, quotaRecovery: null,
      scope: { kind: "all", modelCount: 0, singleModel: null }, grants: { allowedEndpointIds: [], allowedOrigins: [] },
      cooldowns: { genericUntil: null, fiveHourUntil: null, weekUntil: null, monthUntil: null, freeUntil: null } } };
}
function pageCard(rows: AccountPageRow[] = [], offset = 0, total = 20): AccountPageCard {
  return { cardId: "card", position: 0, destination: {
    id: "destination", name: "Synthetic", adapter: "http", legacy: { kind: "builtin", id: "synthetic" },
    accountControls: { browserProfile: false, configurationOwner: "destination", consoleLink: null, toggleWrite: "account" },
    authScheme: "bearer", baseUrl: null, brandFamily: null, enabled: true, maxCredentials: null, observerCredentialId: null,
    plan: null, protocols: ["chat_completions"], protocolRoutes: [], modelResolution: "public_only", catalogCount: 0, enabledCatalogCount: 0,
    capabilities: { billingTierRequired: false, discoverableModels: false, externalIntegration: false, identityHeaders: false,
      managedSignup: false, observer: false, officialBalanceProbe: [], redirectPolicy: "no_follow", testable: true },
  }, platform: null, totalCredentials: total, matchedCredentials: total, rows, rowsOffset: offset,
    rowsHasMore: offset + rows.length < total, credentialCreate: null, actions: [], availability: "available", cpaStatus: null };
}
function cardSlice(rows: AccountPageRow[], offset = 5, version = "A", total = 20,
  asOf = snapshot(version).asOf, validUntil: string | null = null): AccountCardCredentialsPage {
  return { revision: snapshot(version).revision, readVersion: version, cardId: "card", total, filteredTotal: total,
    rows, offset, limit: 5, hasMore: offset + rows.length < total, errors: [], asOf, validUntil };
}

test("page queries normalize filters and keep requested pages bounded", () => {
  assert.deepEqual(normalizeAccountsQuery({ search: "  synthetic ", plan: "all", status: "all", offset: -5, limit: 500 }),
    { search: "synthetic", plan: undefined, status: undefined, offset: 0, limit: 100 });
});
test("identical page flights join and warm navigation reuses the session snapshot", async () => {
  const store = setup(); const gate = deferred<AccountsPage>(); let calls = 0;
  pagesApi.accounts = async () => { calls++; return gate.promise; };
  const first = store.load(); const joined = store.load(); gate.resolve(snapshot("first")); await Promise.all([first, joined]);
  await store.load(undefined, { maxAgeMs: 15_000 });
  assert.equal(calls, 1); assert.equal(store.page?.readVersion, "first"); assert.equal(store.loading, false);
});
test("a joined nonzero page initializes its server card cursor and preserves it in the warm cache", async () => {
  const store = setup(); const gate = deferred<AccountsPage>();
  pagesApi.accounts = () => gate.promise;
  const first = store.load({ offset: 10 }); const joined = store.load({ offset: 10 });
  gate.resolve({ ...snapshot("A"), offset: 10, cards: [pageCard([pageRow("ten")], 10)] });
  await Promise.all([first, joined]);
  assert.deepEqual(store.cardPaging.card, { offset: 10, hasMore: true });
  await store.load({ offset: 10 }, { maxAgeMs: 15_000 });
  assert.equal(store.page?.cards[0]?.rowsOffset, 10);
  assert.deepEqual(store.cardPaging.card, { offset: 10, hasMore: true });
});
test("a selected credential page survives automatic-demand and timer revalidation with fresh deadline facts", async () => {
  const store = setup(); let pageReads = 0; let cardReads = 0;
  pagesApi.accounts = async () => ({ ...snapshot("A"), cards: [pageCard([pageRow("initial")])],
    validUntil: new Date(Date.now() - 1).toISOString(), asOf: ++pageReads === 1 ? "2026-10-07T00:00:00Z" : "2026-10-07T00:00:02Z" });
  pagesApi.accountCredentials = async (_id, query) => {
    assert.equal(query?.offset, 5); assert.equal(query?.limit, 5);
    return cardSlice([pageRow("selected", ++cardReads === 1 ? "cooling" : "enabled")], 5, "A", 20,
      pageReads === 1 ? "2026-10-07T00:00:00Z" : "2026-10-07T00:00:02Z");
  };
  pagesApi.refreshAccount = async () => ({ revision: snapshot("A").revision, outcome: "fresh", account: null, billing: null,
    refresh: { supported: true, observedAt: null, freshUntil: null, nextAllowedAt: null }, errors: [] });
  await store.load(); assert.equal(cardReads, 0);
  await store.loadCredentials("card", 5);
  const receipt = await store.refreshAccount("selected", "automatic", { expectedRevision: 1, processGeneration: 1 });
  assert.equal(receipt?.outcome, "fresh"); assert.equal(pageReads, 1); assert.equal(store.cardPaging.card?.offset, 5);
  await store.load(undefined, { maxAgeMs: 15_000 });
  assert.equal(pageReads, 2); assert.equal(cardReads, 2);
  assert.equal(store.page?.readVersion, "A"); assert.equal(store.page?.cards[0]?.rows[0]?.credential.id, "selected");
  assert.equal(store.page?.cards[0]?.rows[0]?.status, "enabled");
  assert.equal(store.page?.cards[0]?.rowsOffset, 5); assert.equal(store.cardPaging.card?.offset, 5);
});
test("after a deadline GET fails, paging cannot mix fresh rows with an older header carrying the same source token", async () => {
  const store = setup(); let pageReads = 0; let cardReads = 0;
  const oldTime = "2026-10-07T00:00:00Z"; const freshTime = "2026-10-07T00:00:02Z";
  const oldPage = { ...snapshot("A"), asOf: oldTime, validUntil: "2026-10-07T00:00:01Z",
    cards: [{ ...pageCard([pageRow("initial", "cooling")]), availability: "cooling" }] };
  pagesApi.accounts = async () => { pageReads++; return oldPage; }; await store.load();
  pagesApi.accounts = async () => { pageReads++; throw new Error("deadline header offline"); };
  await assert.rejects(store.load(), /deadline header offline/);
  pagesApi.accountCredentials = async () => {
    cardReads++; return cardSlice([pageRow("selected", "enabled")], 5, "A", 20, freshTime, "2026-10-07T00:00:17Z");
  };
  await assert.rejects(store.loadCredentials("card", 5), /deadline header offline/);
  assert.equal(pageReads, 3); assert.equal(cardReads, 1); assert.equal(store.page, oldPage);
  assert.equal(store.page?.asOf, oldTime); assert.equal(store.page?.cards[0]?.availability, "cooling");
  assert.equal(store.page?.cards[0]?.rows[0]?.status, "cooling"); assert.equal(store.cardPaging.card?.offset, 0);
  pagesApi.accounts = async () => {
    pageReads++; return { ...snapshot("A"), asOf: freshTime, validUntil: "2026-10-07T00:00:17Z",
      cards: [pageCard([pageRow("initial", "enabled")])] };
  };
  await store.loadCredentials("card", 5);
  assert.equal(pageReads, 4); assert.equal(cardReads, 2); assert.equal(store.page?.readVersion, "A");
  assert.equal(store.page?.asOf, freshTime); assert.equal(store.page?.cards[0]?.availability, "available");
  assert.equal(store.page?.cards[0]?.rows[0]?.credential.id, "selected");
  assert.equal(store.page?.cards[0]?.rows[0]?.status, "enabled"); assert.equal(store.cardPaging.card?.offset, 5);
});
test("retained slices use observation time as well as source token and retain the prior pair on reconciliation failure", async () => {
  const store = setup(); let pageReads = 0; let cardReads = 0;
  const oldTime = "2026-10-07T00:00:00Z"; const freshTime = "2026-10-07T00:00:02Z";
  let deadlinePassed = false;
  pagesApi.accounts = async () => {
    if (++pageReads === 3) throw new Error("time reconciliation offline");
    const currentTime = pageReads > 3 ? freshTime : oldTime;
    return { ...snapshot("A"), asOf: currentTime, cards: [{ ...pageCard([pageRow("initial")]),
      availability: currentTime === oldTime ? "cooling" : "available" }] };
  };
  pagesApi.accountCredentials = async () => {
    cardReads++; return cardSlice([pageRow("selected", deadlinePassed ? "enabled" : "cooling")], 5, "A", 20,
      deadlinePassed ? freshTime : oldTime);
  };
  await store.load(); await store.loadCredentials("card", 5); const prior = store.page;
  deadlinePassed = true;
  await assert.rejects(store.load(), /time reconciliation offline/);
  assert.equal(pageReads, 3); assert.equal(cardReads, 2); assert.equal(store.page, prior);
  assert.equal(store.page?.asOf, oldTime); assert.equal(store.page?.cards[0]?.availability, "cooling");
  assert.equal(store.page?.cards[0]?.rows[0]?.status, "cooling"); assert.equal(store.cardPaging.card?.offset, 5);
  await store.load();
  assert.equal(pageReads, 4); assert.equal(cardReads, 3); assert.equal(store.page?.readVersion, "A");
  assert.equal(store.page?.asOf, freshTime); assert.equal(store.page?.cards[0]?.availability, "available");
  assert.equal(store.page?.cards[0]?.rows[0]?.status, "enabled"); assert.equal(store.cardPaging.card?.offset, 5);
});
test("direct paging retries its slice once when a reconciled header has a later observation time", async () => {
  const store = setup(); let pageReads = 0; let cardReads = 0;
  pagesApi.accounts = async () => ({ ...snapshot("A"), asOf: ++pageReads === 1 ? "2026-10-07T00:00:00Z" : "2026-10-07T00:00:03Z",
    cards: [pageCard([pageRow("initial")])] });
  pagesApi.accountCredentials = async () => cardSlice([pageRow(++cardReads === 1 ? "at-two" : "at-three")], 5, "A", 20,
    cardReads === 1 ? "2026-10-07T00:00:02Z" : "2026-10-07T00:00:03Z");
  await store.load(); await store.loadCredentials("card", 5);
  assert.equal(pageReads, 2); assert.equal(cardReads, 2); assert.equal(store.page?.asOf, "2026-10-07T00:00:03Z");
  assert.equal(store.page?.cards[0]?.rows[0]?.credential.id, "at-three"); assert.equal(store.cardPaging.card?.offset, 5);
});
test("a selected card page supersedes a late global refresh rather than resetting its rows", async () => {
  const store = setup(); pagesApi.accounts = async () => ({ ...snapshot("A"), cards: [pageCard([pageRow("initial")])] });
  await store.load(); const gate = deferred<AccountsPage>(); pagesApi.accounts = () => gate.promise;
  const pending = store.load(); pagesApi.accountCredentials = async () => cardSlice([pageRow("selected")]);
  await store.loadCredentials("card", 5);
  gate.resolve({ ...snapshot("A"), cards: [pageCard([pageRow("late-initial")])] }); await pending;
  assert.equal(store.page?.cards[0]?.rows[0]?.credential.id, "selected"); assert.equal(store.cardPaging.card?.offset, 5);
});
test("restoring a selected slice reconciles once when its transport publishes a newer control revision", async () => {
  const store = setup(); const control = useControlPlaneStore(); control.sync({ revision: 1, processGeneration: 1 });
  let pageReads = 0; let cardReads = 0;
  pagesApi.accounts = async () => {
    const changed = ++pageReads > 2;
    return { ...snapshot(changed ? "B" : "A"), revision: { ...snapshot("A").revision, revision: changed ? 2 : 1 },
      totalCredentials: changed ? 30 : 20, cards: [pageCard([pageRow("initial")], 0, changed ? 30 : 20)] };
  };
  pagesApi.accountCredentials = async () => {
    const changed = ++cardReads > 1;
    if (changed) control.sync({ revision: 2, processGeneration: 1 });
    return { ...cardSlice([pageRow(changed ? "selected-B" : "selected-A")], 5, changed ? "B" : "A", changed ? 30 : 20),
      revision: { ...snapshot("A").revision, revision: changed ? 2 : 1 } };
  };
  await store.load(); await store.loadCredentials("card", 5); await store.load();
  assert.equal(pageReads, 3); assert.equal(cardReads, 3);
  assert.equal(store.page?.readVersion, "B"); assert.equal(store.page?.revision.revision, 2);
  assert.equal(store.page?.totalCredentials, 30); assert.equal(store.page?.cards[0]?.totalCredentials, 30);
  assert.equal(store.page?.cards[0]?.rows[0]?.credential.id, "selected-B"); assert.equal(store.cardPaging.card?.offset, 5);
});
test("a partial refresh commits the new observation and preserves last-good billing through a failed page follow-up", async () => {
  const store = setup(); const row = pageRow("selected");
  row.account = { id: "selected", name: "Synthetic", accountType: "key", credentialKind: "api_key", quotaScope: "key",
    providerId: "synthetic", enabled: true, planRoutable: true, setupStep: "ready", username: null, notes: null,
    createdAt: "2026-10-07T00:00:00Z", updatedAt: "2026-10-07T00:00:00Z", purchaseDate: "", expiresOn: "",
    cooldown5hUntil: null, cooldownFreeUntil: null, cooldownGenericUntil: null, cooldownMonthUntil: null, cooldownUntil: null,
    cooldownWeekUntil: null, authError: null, connectionVerifiedAt: null, lastError: null, modelCapabilityCount: 0,
    ollamaBillingTier: null, processGeneration: 1, revision: 1, usageSyncLastSuccessAt: null, usageSyncNextAllowedAt: null,
    verificationError: null, verificationStatus: "verified" };
  row.billing = { surfaceKind: "quota", providerWindows: false, quotaManualCalibration: false, quotaEditorLimits: [], accountId: "selected", cash: null, configurableCredits: false, credits: null, manualCalibration: false,
    model: "quota", officialRefresh: false, presets: [], processGeneration: 1, revision: 1, source: "official", unit: "tokens", usage: null };
  const priorBilling = row.billing;
  pagesApi.accounts = async () => ({ ...snapshot("A"), cards: [pageCard([row], 5)] }); await store.load();
  pagesApi.refreshAccount = async () => ({ revision: { ...snapshot("A").revision, revision: 2 }, outcome: "partial", account: null,
    billing: null, refresh: { supported: true, observedAt: "2026-10-07T01:00:00Z", freshUntil: null, nextAllowedAt: null },
    errors: [{ resource: "platform", id: "selected", code: "models_unavailable" }] });
  const receipt = await store.refreshAccount("selected", "manual", { expectedRevision: 1, processGeneration: 1 });
  assert.equal(receipt?.outcome, "partial"); assert.equal(store.page?.revision.revision, 2);
  assert.equal(store.page?.cards[0]?.rows[0]?.refresh.observedAt, "2026-10-07T01:00:00Z");
  pagesApi.accounts = async () => { throw new Error("page follow-up offline"); };
  await assert.rejects(store.load(), /page follow-up offline/);
  assert.equal(store.page?.cards[0]?.rows[0]?.billing, priorBilling); assert.equal(store.cardPaging.card?.offset, 5);
});
test("server deadline expires a warm snapshot before the generic read interval", async () => {
  const store = setup(); let calls = 0;
  pagesApi.accounts = async () => ({ ...snapshot(String(++calls)), validUntil: new Date(Date.now() - 1).toISOString() });
  await store.load(); await store.load(undefined, { maxAgeMs: 15_000 }); assert.equal(calls, 2);
});
test("a prior query finishing late cannot replace the current page or its error", async () => {
  const store = setup(); const old = deferred<AccountsPage>();
  pagesApi.accounts = query => query?.search === "old" ? old.promise : Promise.resolve(snapshot("current"));
  const pending = store.load({ search: "old" }); await store.load({ search: "current" });
  old.resolve(snapshot("obsolete")); await pending; assert.equal(store.page?.readVersion, "current");
});
test("a write receipt fences pending page and detail reads", async () => {
  const store = setup(); const page = deferred<AccountsPage>(); const detail = deferred<AccountDetail>();
  pagesApi.accounts = () => page.promise; pagesApi.accountDetail = () => detail.promise;
  const pending = store.load(); const full = store.loadDetail("synthetic"); store.noteMutation();
  page.resolve(snapshot("old")); detail.resolve({ revision: snapshot("x").revision } as AccountDetail);
  await Promise.all([pending, full]); assert.equal(store.page, null); assert.equal(store.details.size, 0);
});
test("logout clears the snapshot and late reads cannot restore it", async () => {
  const store = setup(); const gate = deferred<AccountsPage>(); pagesApi.accounts = () => gate.promise;
  const pending = store.load(); store.clear(); gate.resolve(snapshot("dead-session")); await pending;
  assert.equal(store.loaded, false); assert.equal(store.page, null); assert.equal(store.details.size, 0);
});
test("the GET that discovers a restarted backend commits its current body, while old process results stay excluded", async () => {
  const store = setup(); const control = useControlPlaneStore(); control.sync({ revision: 1, processGeneration: 99 });
  const old = deferred<AccountsPage>();
  pagesApi.accounts = async query => {
    if (query?.search === "old") return old.promise;
    control.sync({ revision: 1, processGeneration: 100 }); return snapshot("restarted", 100);
  };
  const pending = store.load({ search: "old" }); await store.load({ search: "new" });
  assert.equal(store.page?.readVersion, "restarted");
  old.resolve(snapshot("obsolete", 99)); await pending; assert.equal(store.page?.readVersion, "restarted");
  await store.load({ search: "old" }, { maxAgeMs: 15_000 }); assert.equal(store.page?.readVersion, "restarted");
});
test("failed revalidation retains last-good page with an independent error", async () => {
  const store = setup(); pagesApi.accounts = async () => snapshot("good"); await store.load();
  pagesApi.accounts = async () => { throw new Error("page offline"); };
  await assert.rejects(store.load(), /page offline/); assert.equal(store.page?.readVersion, "good"); assert.equal(store.error, "page offline");
});
test("a late lower revision cannot replace the page after newer control tokens were observed", async () => {
  const store = setup(); const control = useControlPlaneStore();
  pagesApi.accounts = async () => snapshot("good"); await store.load();
  control.sync({ revision: 2, processGeneration: 1 }); pagesApi.accounts = async () => snapshot("obsolete");
  await store.load(); assert.equal(store.page?.readVersion, "good");
});
test("a joined page failure clears loading and exposes the current query failure", async () => {
  const store = setup(); const gate = deferred<AccountsPage>(); pagesApi.accounts = () => gate.promise;
  const first = store.load(); const second = store.load(); gate.reject(new Error("joined offline"));
  await Promise.allSettled([first, second]); assert.equal(store.loading, false); assert.equal(store.error, "joined offline");
});
test("credential page responses from an obsolete filter never replace current card membership", async () => {
  const store = setup(); pagesApi.accounts = async () => snapshot("current"); await store.load();
  const gate = deferred<AccountCardCredentialsPage>(); pagesApi.accountCredentials = () => gate.promise;
  const pending = store.loadCredentials("card", 5); await store.load({ search: "new" });
  gate.resolve({ revision: snapshot("x").revision, readVersion: "old", asOf: snapshot("x").asOf, validUntil: null, cardId: "card", total: 10, filteredTotal: 10,
    rows: [], offset: 5, limit: 5, hasMore: false, errors: [] }); await pending;
  assert.equal(store.cardPaging.card, undefined); assert.equal(store.page?.readVersion, "current");
});
test("an external write between card and page reads reconciles complete header facts before applying the row page", async () => {
  const store = setup(); let reads = 0;
  const destination: AccountPageCard["destination"] = {
    id: "destination", name: "Synthetic", adapter: "http", legacy: { kind: "builtin", id: "synthetic" },
    accountControls: { browserProfile: false, configurationOwner: "destination", consoleLink: null, toggleWrite: "account" },
    authScheme: "bearer", baseUrl: null, brandFamily: null, enabled: true, maxCredentials: null, observerCredentialId: null,
    plan: null, protocols: ["chat_completions"], protocolRoutes: [], modelResolution: "public_only", catalogCount: 0, enabledCatalogCount: 0,
    capabilities: { billingTierRequired: false, discoverableModels: false, externalIntegration: false, identityHeaders: false,
      managedSignup: false, observer: false, officialBalanceProbe: [], redirectPolicy: "no_follow", testable: true },
  };
  const card = (total: number): AccountPageCard => {
    const complete = { cardId: "card", position: 0, destination, platform: null, totalCredentials: total,
      matchedCredentials: total, rows: [], rowsOffset: 0, rowsHasMore: true, credentialCreate: null,
      actions: [], availability: "available", cpaStatus: null };
    return complete;
  };
  pagesApi.accounts = async () => ({ ...snapshot(++reads === 1 ? "A" : "B"), totalCredentials: reads === 1 ? 10 : 20,
    cards: [card(reads === 1 ? 10 : 20)] });
  await store.load();
  const changedRow: AccountPageRow = { account: null, status: "enabled", routeAvailable: true, modelCount: 0,
    inferenceEndpointUrl: null, platformLink: null, actions: [], billing: null,
    refresh: { supported: false, observedAt: null, freshUntil: null, nextAllowedAt: null },
    tags: { credentialCount: 1, bindingDisabled: false, quotaShareName: null, quotaShareCount: 0, duplicateName: false },
    credential: { id: "after-external-write", legacyAccountId: "synthetic", destinationId: "destination", name: "Synthetic",
      authState: "unknown", hasSecret: true, keyPreview: "••••", enabled: true, routingRank: 0, notes: null, lastError: null,
      onboardingTask: null, purchaseDate: null, quotaPoolId: null, quotaRecovery: null,
      scope: { kind: "all", modelCount: 0, singleModel: null }, grants: { allowedEndpointIds: [], allowedOrigins: [] },
      cooldowns: { genericUntil: null, fiveHourUntil: null, weekUntil: null, monthUntil: null, freeUntil: null } },
  };
  pagesApi.accountCredentials = async () => ({ revision: snapshot("B").revision, readVersion: "B", asOf: snapshot("B").asOf, validUntil: null, cardId: "card", total: 20,
    filteredTotal: 20, rows: [changedRow], offset: 5, limit: 5, hasMore: true, errors: [] });
  await store.loadCredentials("card", 5);
  assert.equal(reads, 2); assert.equal(store.page?.readVersion, "B"); assert.equal(store.page?.totalCredentials, 20);
  assert.equal(store.page?.cards[0]?.totalCredentials, 20); assert.equal(store.page?.cards[0]?.rows[0]?.credential.id, "after-external-write");
});
