import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { dashboardApi, type Account } from "../api/dashboard.ts";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import { useAccountsStore } from "./accounts.ts";
import { useAccountLifecycleStore } from "./accountLifecycle.ts";
import { useBillingStore } from "./billing.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useIdentitiesStore } from "./identities.ts";
import { usePlatformAccountsStore } from "./platformAccounts.ts";
import { useProvidersStore } from "./providers.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(yes => { resolve = yes; });
  return { promise, resolve };
}
const flush = () => new Promise<void>(resolve => setImmediate(resolve));
const account = (id: string): Account => ({ id, name: id, updated_at: "v1", provider_id: "custom" } as Account);
function fixture(t: TestContext) {
  setActivePinia(createPinia());
  const accounts = useAccountsStore();
  const billing = useBillingStore();
  const platforms = usePlatformAccountsStore();
  const destinations = useDestinationsStore();
  accounts.setAccounts([account("a"), account("b")]);
  platforms.acceptView({ accounts: [], links: ["a", "b"].map(accountId => ({
    accountId, platformAccountId: "parent", snapshot: null,
    group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: false },
  })), revision: 1, processGeneration: 1 });
  // These reads are deliberately unavailable: the local DELETE must still
  // commit, and recovery must never replay the destructive request.
  const revalidate = t.mock.method(destinations, "refreshAfterMutation", async () => { throw new Error("projection offline"); });
  t.mock.method(destinations, "load", async () => { throw new Error("projection offline"); });
  t.mock.method(platforms, "load", async () => { throw new Error("platform offline"); });
  t.mock.method(useIdentitiesStore(), "loadPresented", async () => { throw new Error("identity offline"); });
  t.mock.method(useProvidersStore(), "loadConnections", async () => { throw new Error("connections offline"); });
  return { accounts, billing, platforms, revalidate, lifecycle: useAccountLifecycleStore() };
}

test("duplicate delete clicks send one request and remove only the confirmed Key", async t => {
  const f = fixture(t);
  const pending = deferred<void>();
  const remove = t.mock.method(dashboardApi, "deleteAccount", () => pending.promise);
  const first = f.lifecycle.remove("a");
  const second = f.lifecycle.remove("a");
  await flush();
  assert.equal(remove.mock.callCount(), 1);
  assert.equal(f.lifecycle.deleting.a, true);
  assert.equal(f.accounts.byId.has("a"), true);
  pending.resolve(undefined);
  const results = await Promise.all([first, second]);
  for (const result of results) {
    assert.equal(result.kind, "deleted");
    if (result.kind === "deleted") assert.equal((await result.revalidation).kind, "failed");
  }
  assert.deepEqual(f.accounts.accounts.map(row => row.id), ["b"]);
  assert.deepEqual(f.platforms.links.map(row => row.accountId), ["b"]);
  assert.deepEqual(f.lifecycle.deleting, {});
  assert.equal(remove.mock.callCount(), 1);
});

test("a failed DELETE keeps both account and platform link and never revalidates", async t => {
  const f = fixture(t);
  t.mock.method(dashboardApi, "deleteAccount", async () => { throw new Error("delete rejected"); });
  await assert.rejects(f.lifecycle.remove("a"), /delete rejected/);
  assert.equal(f.accounts.byId.has("a"), true);
  assert.equal(f.platforms.links.length, 2);
  assert.equal(f.revalidate.mock.callCount(), 0);
  assert.deepEqual(f.lifecycle.deleting, {});
});

test("confirmed deletion releases the dialog result and lock before slow revalidation", async t => {
  const f = fixture(t);
  const reload = deferred<void>();
  t.mock.method(useDestinationsStore(), "refreshAfterMutation", () => reload.promise);
  const remove = t.mock.method(dashboardApi, "deleteAccount", async () => {});
  let settled = false;
  const removing = f.lifecycle.remove("a").then(result => { settled = true; return result; });
  await flush();
  assert.equal(settled, true);
  assert.equal(f.accounts.byId.has("a"), false);
  assert.deepEqual(f.lifecycle.deleting, {});
  assert.equal(remove.mock.callCount(), 1);
  const result = await removing;
  assert.equal(result.kind, "deleted");
  reload.resolve(undefined);
  if (result.kind === "deleted") assert.equal((await result.revalidation).kind, "failed");
});

test("background revalidation cannot report failure into a new session", async t => {
  const f = fixture(t);
  const reload = deferred<void>();
  t.mock.method(useDestinationsStore(), "refreshAfterMutation", () => reload.promise);
  t.mock.method(dashboardApi, "deleteAccount", async () => {});
  const result = await f.lifecycle.remove("a");
  assert.equal(result.kind, "deleted");
  f.billing.clear();
  reload.resolve(undefined);
  if (result.kind === "deleted") assert.equal((await result.revalidation).kind, "cancelled");
});

test("a revision conflict reloads state without retrying DELETE", async t => {
  const f = fixture(t);
  const remove = t.mock.method(dashboardApi, "deleteAccount", async () => {
    throw new DashboardRequestError("changed", 409, "revisionConflict");
  });
  const load = t.mock.method(f.accounts, "loadPresented", async () => f.accounts.accounts);
  assert.equal((await f.lifecycle.remove("a")).kind, "conflict");
  assert.equal(remove.mock.callCount(), 1);
  assert.equal(load.mock.callCount(), 1);
  assert.equal(f.accounts.byId.has("a"), true);
});

test("late DELETE receipt cannot erase a newer session's same-id account", async t => {
  const f = fixture(t);
  const pending = deferred<void>();
  t.mock.method(dashboardApi, "deleteAccount", () => pending.promise);
  const removing = f.lifecycle.remove("a");
  await flush();
  f.billing.clear();
  f.accounts.clearAccounts();
  f.platforms.clear();
  f.accounts.setAccounts([{ ...account("a"), name: "new-session" }]);
  pending.resolve(undefined);
  assert.equal((await removing).kind, "cancelled");
  assert.equal(f.accounts.byId.get("a")?.name, "new-session");
  assert.equal(f.revalidate.mock.callCount(), 0);
  assert.deepEqual(f.lifecycle.deleting, {});
});

test("a pending refresh or mutation receipt cannot resurrect a locally deleted row", async t => {
  const f = fixture(t);
  const old = [...f.accounts.accounts];
  const pending = deferred<Account[]>();
  t.mock.method(dashboardApi, "getAccounts", () => pending.promise);
  const loading = f.accounts.loadPresented();
  f.accounts.removeAccount("a");
  pending.resolve(old);
  await loading;
  f.accounts.upsertAccount(account("a"));
  f.accounts.setAccounts(old);
  assert.deepEqual(f.accounts.accounts.map(row => row.id), ["b"]);
});

test("a new authoritative list can confirm an intentionally restored account", async t => {
  const f = fixture(t);
  f.accounts.removeAccount("a");
  t.mock.method(dashboardApi, "getAccounts", async () => [account("a"), account("b")]);
  await f.accounts.loadPresented();
  f.accounts.upsertAccount({ ...account("a"), name: "restored" });
  assert.equal(f.accounts.byId.get("a")?.name, "restored");
});
