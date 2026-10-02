import assert from "node:assert/strict";
import nodeTest, { type TestContext } from "node:test";

// These tests replace the shared billing client. A lane keeps one replacement
// visible to the session and CAS assertions that follow it.
let lane: Promise<void> = Promise.resolve();
function test(name: string, body: (t: TestContext) => Promise<void> | void): Promise<void> {
  return nodeTest(name, (t) => {
    const run = lane.then(() => body(t));
    lane = run.then(() => undefined, () => undefined);
    return run;
  });
}
import { createPinia, setActivePinia } from "pinia";
import { effectScope, nextTick, ref } from "vue";
import { dashboardApi, type Account, type UsageWindow } from "../api/dashboard.ts";
import { billingApi, type BillingStatus } from "../api/billing.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { ManualCalibrationPlan, UsageEditState, UsageKey } from "./accounts-usage.ts";
import { useBillingStore } from "../stores/billing.ts";
import { useAccountUsage, type AccountUsageEdits } from "./useAccountUsage.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { createAccountsAutoRefresh } from "./accounts-auto-refresh.ts";


function status(used: number): BillingStatus {
  return {
    accountId: "a", model: "quota", source: "local_estimate", unit: "USD",
    configurableCredits: false, manualCalibration: true, officialRefresh: true,
    cash: null, credits: null, presets: [], revision: 3, processGeneration: 1,
    usage: {
      accountId: "a", providerId: "opencode-go", availability: "available",
      creditBalances: [], experimental: false, freeCooldownUntil: null,
      pricingRevision: null, processGeneration: 1, revision: 3, syncState: null,
      quotaWindows: [{
        accountId: "a", windowKind: "five_hours", used, limitValue: 100,
        startedAt: null, resetsAt: null, calibrationOffset: 0, unit: "USD",
        source: "local_estimate", observedAt: null, updatedAt: "2026-09-21T00:00:00Z",
      }],
    },
  };
}

function deferred() {
  let resolve!: (value: UsageWindow) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<UsageWindow>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function requireUsageEdit(
  edits: Record<string, AccountUsageEdits>,
  accountId: string,
  key: UsageKey,
): UsageEditState {
  const edit = edits[accountId]?.[key];
  if (!edit) throw new Error(`fixture: missing ${key} edit for ${accountId}`);
  return edit;
}

const savedUsage: UsageWindow = {
  account_id: "a", window_5h: 80, window_week: 0, window_month: 0,
  resets_in_5h: null, resets_in_week: null, resets_in_month: null,
};

async function fixture(
  t: TestContext,
  afterUsageRefresh?: (accountId: string, isCurrent: () => boolean) => Promise<void>,
) {
  setActivePinia(createPinia());
  const originalStatus = billingApi.status;
  const originalSave = dashboardApi.updateAccountUsage;
  const request = deferred();
  dashboardApi.updateAccountUsage = () => request.promise;
  let used = 10;
  billingApi.status = async () => status(used);
  const accounts = ref([{ id: "a", provider_id: "opencode-go", updated_at: "v1" } as Account]);
  let notifications = 0;
  const notify = () => { notifications++; return {} as never; };
  const scope = effectScope();
  const usage = scope.run(() => useAccountUsage(accounts, ref(Date.now()), ref(null), {
    message: { success: notify, warning: notify, error: notify },
    afterUsageRefresh,
  }))!;
  t.after(() => {
    scope.stop(); billingApi.status = originalStatus; dashboardApi.updateAccountUsage = originalSave;
  });
  await usage.loadAccountUsage("a");
  usage.updateUsageDraft("a", "window_5h", 80);
  return {
    accounts, usage, request, scope, store: useBillingStore(),
    notifications: () => notifications,
    reload: async (next: number) => { used = next; await usage.loadAccountUsage("a"); },
  };
}

test("manual calibration updates its current billing snapshot", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.request.resolve(savedUsage);
  await saving;
  assert.equal(f.usage.getUsage("a").window_5h, 80);
  assert.equal(requireUsageEdit(f.usage.usageEdits.value, "a", "window_5h").saving, false);
});

test("late calibration cannot overwrite the same account in a new session", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.store.clear();
  await nextTick();
  await f.reload(25);
  f.request.resolve(savedUsage);
  await saving;
  assert.equal(f.usage.getUsage("a").window_5h, 25);
  assert.equal(requireUsageEdit(f.usage.usageEdits.value, "a", "window_5h").saved, 25);
  assert.equal(f.notifications(), 0);
});

test("late calibration cannot overwrite a changed account binding", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.accounts.value = [{ ...f.accounts.value[0]!, updated_at: "v2" }];
  await f.reload(30);
  f.request.resolve(savedUsage);
  await saving;
  assert.equal(f.usage.getUsage("a").window_5h, 30);
  assert.equal(requireUsageEdit(f.usage.usageEdits.value, "a", "window_5h").saving, false);
  assert.equal(f.notifications(), 0);
});

test("a disposed editor ignores a late calibration failure", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.scope.stop();
  f.request.reject(new Error("old request failed"));
  await saving;
  assert.equal(f.notifications(), 0);
  assert.equal(f.usage.getUsage("a").window_5h, 10);
});

test("late refresh does not notify or start companion discovery after logout", async (t) => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; });
  f.store.refreshUsage = async () => { await f.request.promise; return status(80); };
  const refreshing = f.usage.refreshAccountUsage("a");
  f.store.clear();
  await nextTick();
  await f.reload(25);
  f.request.resolve(savedUsage);
  await refreshing;
  assert.equal(f.usage.getUsage("a").window_5h, 25);
  assert.equal(f.notifications(), 0);
  assert.equal(companions, 0);
});

test("companion discovery receives a guard that expires during its own await", async (t) => {
  const discovery = deferred();
  let started = false;
  let writes = 0;
  const f = await fixture(t, async (_id, isCurrent) => {
    started = true;
    await discovery.promise;
    if (isCurrent()) writes++;
  });
  f.store.refreshUsage = async () => status(10);
  const refreshing = f.usage.refreshAccountUsage("a");
  await nextTick();
  assert.equal(started, true);
  f.store.clear();
  f.scope.stop();
  discovery.resolve(savedUsage);
  await refreshing;
  assert.equal(writes, 0);
});

test("late usage load cannot recreate disposed editor drafts", async (t) => {
  const f = await fixture(t);
  let resolve!: (status: BillingStatus) => void;
  billingApi.status = () => new Promise((yes) => { resolve = yes; });
  const loading = f.usage.loadAccountUsage("a");
  f.scope.stop();
  f.usage.usageEdits.value = {};
  resolve(status(25));
  await loading;
  assert.deepEqual(f.usage.usageEdits.value, {});
});


test("automatic refresh uses the existing mutation silently without companion discovery", async (t) => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; });
  let refreshes = 0;
  f.store.refreshUsage = async () => { refreshes++; return status(10); };
  f.accounts.value[0]!.enabled = true;
  f.accounts.value[0]!.setup_step = "ready";
  // An unsaved calibration defers automatic work.
  assert.equal(f.usage.automaticRefreshTarget(f.accounts.value[0]!)?.busy, true);
  f.usage.updateUsageDraft("a", "window_5h", 10);
  const target = f.usage.automaticRefreshTarget(f.accounts.value[0]!)!;
  assert.equal(target.busy, false);
  await target.refresh(() => true);
  assert.equal(refreshes, 1);
  assert.equal(f.notifications(), 0);
  assert.equal(companions, 0);
  f.accounts.value[0]!.enabled = false;
  assert.equal(f.usage.automaticRefreshTarget(f.accounts.value[0]!), null);
});

test("automatic work rechecks visibility after its local read and keeps manual feedback", async (t) => {
  const f = await fixture(t);
  f.accounts.value[0]!.enabled = true;
  f.accounts.value[0]!.setup_step = "ready";
  f.usage.updateUsageDraft("a", "window_5h", 10);
  let refreshes = 0;
  f.store.refreshUsage = async () => { refreshes++; return status(10); };
  await f.usage.automaticRefreshTarget(f.accounts.value[0]!)!.refresh(() => false);
  assert.equal(refreshes, 0);
  await f.usage.refreshAccountUsage("a");
  assert.equal(refreshes, 1);
  assert.equal(f.notifications(), 1);
});

test("automatic refresh failure is quiet and cannot wipe the last good usage", async (t) => {
  const f = await fixture(t);
  f.store.refreshUsage = async () => { throw new Error("offline"); };
  await f.usage.refreshAccountUsage("a", true);
  assert.equal(f.notifications(), 0);
  assert.equal(f.usage.getUsage("a").window_5h, 10);
});


test("a serial automatic pass obtains fresh CAS tokens for each real billing mutation", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => { globalThis.fetch = originalFetch; });
  setupControlPlane(3, 1);
  let revision = 3;
  const posts: string[] = [];
  installFetchMock(req => {
    const id = req.url.includes("/accounts/a/") ? "a" : "b";
    const snapshot = status(10);
    snapshot.accountId = id;
    snapshot.revision = revision;
    snapshot.usage!.accountId = id;
    snapshot.usage!.revision = revision;
    if (req.method === "POST") {
      assert.equal(req.body?.expectedRevision, revision);
      posts.push(id);
      snapshot.usage!.revision = ++revision;
      return snapshot.usage!;
    }
    assert.ok(req.url.endsWith("/billing"));
    return snapshot;
  });
  const accounts = ref(["a", "b"].map(id => ({
    id, provider_id: "opencode-go", updated_at: "v1", enabled: true, setup_step: "ready",
  } as Account)));
  const scope = effectScope(); t.after(() => scope.stop());
  const unexpectedToast = () => { assert.fail("automatic pass must be quiet"); };
  const usage = scope.run(() => useAccountUsage(accounts, ref(Date.now()), ref(null), {
    message: { success: unexpectedToast, warning: unexpectedToast, error: unexpectedToast },
  }))!;
  await Promise.all(accounts.value.map(account => usage.loadAccountUsage(account.id)));
  const refresh = createAccountsAutoRefresh({
    allowed: () => true,
    targets: () => accounts.value.flatMap(account => {
      const target = usage.automaticRefreshTarget(account); return target ? [target] : [];
    }),
  });
  await refresh.run();
  assert.deepEqual(posts, ["a", "b"]);
  assert.equal(useBillingStore().byId.b?.status?.revision, 5);
});


test("declared support retries a missing billing snapshot before any upstream refresh", async (t) => {
  const f = await fixture(t);
  f.accounts.value[0] = { ...f.accounts.value[0]!, provider_id: "opencode", enabled: true, setup_step: "ready" };
  f.store.remove("a");
  delete f.usage.usageEdits.value.a;
  let refreshes = 0;
  f.store.refreshUsage = async () => { refreshes++; return status(10); };
  billingApi.status = async () => { throw new Error("temporary local read failure"); };
  await f.usage.automaticRefreshTarget(f.accounts.value[0]!)!.refresh(() => true);
  assert.equal(refreshes, 0);
  billingApi.status = async () => status(10);
  await f.usage.automaticRefreshTarget(f.accounts.value[0]!)!.refresh(() => true);
  assert.equal(refreshes, 1);
  assert.equal(f.notifications(), 0);
});

test("a five-hour observation leaves week and month unavailable", async (t) => {
  const f = await fixture(t);
  const usage = f.usage.getUsage("a");
  assert.equal(usage.window_5h, 10);
  assert.equal(usage.window_week, null);
  assert.equal(usage.window_month, null);
});

const TIMED_WINDOWS = ["window_5h", "window_week", "window_month"] as const satisfies readonly UsageKey[];
const OLLAMA_WINDOWS = ["window_month"] as const satisfies readonly UsageKey[];

function catalogRow(
  providerId: string,
  manual: boolean,
  availability: ProviderCatalogEntry["usage_availability"],
): ProviderCatalogEntry {
  return {
    provider_id: providerId,
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: providerId,
    display_family: providerId,
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    creation_unavailable_reason: null,
    verification_policy: "not_required",
    verification_runtime_availability: "not_applicable",
    routable: true,
    managed_registration: false,
    usage_availability: availability,
    manual_usage_calibration: manual,
    quota_unit: providerId === "ollama" ? "usd_credits" : "percent",
    model_source: "builtin",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
  };
}

const calibrationCatalog: ProviderCatalogEntry[] = [
  catalogRow("opencode", true, "available"),
  catalogRow("command-code", true, "available"),
  catalogRow("ollama", true, "local_state"),
  catalogRow("minimax", false, "available"),
];

function percentWindow(accountId: string, windowKind: string, used: number) {
  return {
    accountId,
    windowKind,
    used,
    limitValue: 100,
    startedAt: null,
    resetsAt: null,
    calibrationOffset: 0,
    unit: "percent",
    source: "manual",
    observedAt: "2026-10-01T00:00:00.000Z",
    updatedAt: "2026-10-01T00:00:00.000Z",
  };
}

function calibrationStatus(
  accountId: string,
  providerId: string,
  manual: boolean,
  model: BillingStatus["model"],
  windows: ReturnType<typeof percentWindow>[] = [],
): BillingStatus {
  return {
    accountId,
    model,
    source: windows.length === 0 ? "unavailable" : "official",
    unit: "percent",
    configurableCredits: false,
    manualCalibration: manual,
    officialRefresh: providerId !== "ollama",
    cash: null,
    credits: null,
    presets: [],
    revision: 3,
    processGeneration: 1,
    usage: {
      accountId,
      providerId,
      availability: providerId === "ollama" ? "local_state" : "available",
      creditBalances: [],
      experimental: false,
      freeCooldownUntil: null,
      pricingRevision: null,
      processGeneration: 1,
      revision: 3,
      syncState: null,
      quotaWindows: windows,
    },
  };
}

function calibrationAccount(id: string, providerId: string): Account {
  return { id, provider_id: providerId, updated_at: "v1", enabled: true, setup_step: "ready" } as Account;
}

async function calibrationFixture(
  t: TestContext,
  account: Account,
  snapshot: BillingStatus,
  options?: {
    catalog?: ProviderCatalogEntry[];
    plan?: ManualCalibrationPlan | null;
  },
) {
  setActivePinia(createPinia());
  const originalStatus = billingApi.status;
  const originalSave = dashboardApi.updateAccountUsage;
  const posts: Array<{ id: string; window: UsageKey; percent: number; resets: number | null | undefined }> = [];
  const reads: string[] = [];
  billingApi.status = async (id: string) => {
    reads.push(id);
    return snapshot.accountId === id ? snapshot : calibrationStatus(id, "minimax", false, "quota");
  };
  dashboardApi.updateAccountUsage = async (id, window, percent, resets) => {
    posts.push({ id, window, percent, resets });
    const response: UsageWindow = {
      account_id: id,
      window_5h: window === "window_5h" ? percent : null,
      window_week: window === "window_week" ? percent : null,
      window_month: window === "window_month" ? percent : null,
      resets_in_5h: window === "window_5h" ? "2026-10-01T00:30:00.000Z" : null,
      resets_in_week: window === "window_week" ? "2026-10-01T00:30:00.000Z" : null,
      resets_in_month: null,
    };
    return response;
  };
  const accounts = ref([account]);
  const notify = () => ({} as never);
  const scope = effectScope();
  const usage = scope.run(() => useAccountUsage(
    accounts,
    ref(Date.parse("2026-10-01T00:00:00.000Z")),
    ref(options?.catalog ?? calibrationCatalog),
    {
      message: { success: notify, warning: notify, error: notify },
      ...(options && "plan" in options ? { calibrationPlanFor: () => options.plan } : {}),
    },
  ))!;
  t.after(() => {
    scope.stop();
    billingApi.status = originalStatus;
    dashboardApi.updateAccountUsage = originalSave;
  });
  await usage.loadAccountUsage(account.id);
  return { accounts, usage, posts, reads, store: useBillingStore() };
}

test("omitted quota windows stay unknown and are not a usable zero", async (t) => {
  const account = calibrationAccount("goat", "command-code");
  const f = await calibrationFixture(t, account, calibrationStatus("goat", "command-code", true, "quota"));
  const observed = f.usage.getUsage("goat");
  assert.equal(observed.window_5h, null);
  assert.equal(observed.window_week, null);
  assert.equal(observed.window_month, null);
  assert.notEqual(observed.window_5h, 0);
  const presented = f.usage.providerUsageFor("goat").value;
  assert.equal(presented?.quota_windows.length, 0);
  assert.equal(presented?.quota_windows.some((window) => window.used === 0), false);
});

test("an explicit zero stays zero while an omitted sibling stays unknown", async (t) => {
  const account = calibrationAccount("goat", "command-code");
  const f = await calibrationFixture(
    t,
    account,
    calibrationStatus("goat", "command-code", true, "quota", [percentWindow("goat", "five_hours", 0)]),
  );
  const observed = f.usage.getUsage("goat");
  assert.equal(observed.window_5h, 0);
  assert.equal(observed.window_week, null);
  assert.equal(observed.window_month, null);
  assert.notEqual(observed.window_week, 0);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.draft, 0);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.saved, 0);
  assert.ok(f.usage.usageEdits.value.goat?.window_week);
  assert.equal(f.usage.usageEdits.value.goat?.window_week?.draft, null);
  assert.equal(f.usage.usageEdits.value.goat?.window_month?.draft, null);
  assert.equal(f.usage.providerUsageFor("goat").value?.quota_windows.length, 1);
});

async function assertBlankDraft(
  t: TestContext,
  providerId: string,
  manual: boolean,
  model: BillingStatus["model"],
  windows: readonly UsageKey[],
) {
  const account = calibrationAccount(providerId, providerId);
  const f = await calibrationFixture(
    t,
    account,
    calibrationStatus(providerId, providerId, manual, model),
  );
  assert.equal(f.usage.providerUsageFor(providerId).value?.quota_windows.length, 0);
  for (const key of TIMED_WINDOWS) assert.equal(f.usage.getUsage(providerId)[key], null);
  assert.equal(f.usage.hasAvailableUsageEditor(account), true);
  const edits = f.usage.usageEdits.value[providerId];
  for (const key of windows) {
    assert.ok(edits?.[key], key);
    assert.equal(edits?.[key]?.draft, null, key);
    assert.equal(edits?.[key]?.saved, null, key);
  }
  for (const key of TIMED_WINDOWS) {
    if (!windows.includes(key)) assert.equal(edits?.[key], undefined, key);
  }
  await f.usage.saveUsage(providerId, windows[0]!);
  assert.equal(f.posts.length, 0);
  assert.equal(f.reads.length, 1);
}

test("GOAT opens a blank draft for 5h, week, and month without posting", async (t) => {
  await assertBlankDraft(t, "command-code", true, "quota", TIMED_WINDOWS);
});

test("OpenCode Go opens a blank draft for its three windows without posting", async (t) => {
  await assertBlankDraft(t, "opencode", true, "quota", TIMED_WINDOWS);
});

test("Ollama opens a blank monthly draft and has no 5h or week editor", async (t) => {
  await assertBlankDraft(t, "ollama", true, "credits", OLLAMA_WINDOWS);
});

test("an unsupported provider with omitted windows keeps calibration disabled", async (t) => {
  const account = calibrationAccount("mini", "minimax");
  const f = await calibrationFixture(t, account, calibrationStatus("mini", "minimax", false, "quota"));
  assert.equal(f.usage.hasAvailableUsageEditor(account), false);
  assert.deepEqual(f.usage.usageEdits.value, {});
  assert.equal(f.usage.getUsage("mini").window_5h, null);
  assert.equal(f.usage.getUsage("mini").window_week, null);
  assert.equal(f.usage.getUsage("mini").window_month, null);
  await f.usage.saveUsage("mini", "window_5h");
  assert.equal(f.posts.length, 0);
  assert.equal(f.usage.providerUsageFor("mini").value?.quota_windows.length, 0);
});

const DENIED_PROVIDERS = ["opencode", "command-code", "ollama", "minimax"] as const;

async function assertCalibrationDenied(
  t: TestContext,
  providerId: string,
  manual: boolean,
  plan: ManualCalibrationPlan | null,
): Promise<void> {
  const account = calibrationAccount(providerId, providerId);
  const availability = providerId === "ollama" ? "local_state" : "available";
  const model = providerId === "ollama" ? "credits" : "quota";
  const f = await calibrationFixture(
    t,
    account,
    calibrationStatus(providerId, providerId, manual, model),
    {
      catalog: [catalogRow(providerId, manual, availability)],
      plan,
    },
  );
  assert.equal(f.usage.hasAvailableUsageEditor(account), false);
  assert.deepEqual(f.usage.usageEdits.value, {});
  for (const key of TIMED_WINDOWS) {
    assert.equal(f.usage.getUsage(providerId)[key], null);
    assert.notEqual(f.usage.getUsage(providerId)[key], 0);
  }
  await f.usage.saveUsage(providerId, "window_5h");
  await f.usage.saveUsage(providerId, "window_week");
  await f.usage.saveUsage(providerId, "window_month");
  assert.equal(f.posts.length, 0);
  assert.equal(f.reads.length, 1);
}

test("an explicit false loaded plan denies calibration for every provider including OpenCode Go", async (t) => {
  const plan: ManualCalibrationPlan = {
    manual_calibration: false,
    windows: [{ kind: "five_hours" }, { kind: "week" }, { kind: "month" }],
  };
  for (const providerId of DENIED_PROVIDERS) {
    await assertCalibrationDenied(t, providerId, true, plan);
  }
});

test("no loaded plan and false billing and catalog flags deny calibration including OpenCode Go", async (t) => {
  for (const providerId of DENIED_PROVIDERS) {
    await assertCalibrationDenied(t, providerId, false, null);
  }
});

test("a true plan authorizes only its listed known windows as a blank draft", async (t) => {
  const account = calibrationAccount("opencode", "opencode");
  const f = await calibrationFixture(
    t,
    account,
    calibrationStatus("opencode", "opencode", false, "quota"),
    {
      catalog: [catalogRow("opencode", false, "available")],
      plan: { manual_calibration: true, windows: [{ kind: "week" }, { kind: "free" }] },
    },
  );
  assert.equal(f.usage.hasAvailableUsageEditor(account), true);
  const edits = f.usage.usageEdits.value.opencode;
  assert.equal(edits?.window_week?.draft, null);
  assert.equal(edits?.window_week?.saved, null);
  assert.equal(edits?.window_5h, undefined);
  assert.equal(edits?.window_month, undefined);
  for (const key of TIMED_WINDOWS) {
    assert.equal(f.usage.getUsage("opencode")[key], null);
    assert.notEqual(f.usage.getUsage("opencode")[key], 0);
  }
  await f.usage.saveUsage("opencode", "window_week");
  await f.usage.saveUsage("opencode", "window_5h");
  assert.equal(f.posts.length, 0);
  assert.equal(f.reads.length, 1);
});

test("explicit 42.5 and reset 30 post once and the getter shows 42.5", async (t) => {
  const account = calibrationAccount("goat", "command-code");
  const f = await calibrationFixture(t, account, calibrationStatus("goat", "command-code", true, "quota"));
  const edit = f.usage.usageEdits.value.goat?.window_5h;
  assert.ok(edit);
  assert.equal(edit.draft, null);
  await f.usage.saveUsage("goat", "window_5h");
  assert.equal(f.posts.length, 0);
  f.usage.updateUsageDraft("goat", "window_5h", 42.5);
  f.usage.updateResetsFirstField("goat", "window_5h", 0);
  f.usage.updateResetsSecondField("goat", "window_5h", 30);
  await f.usage.saveUsage("goat", "window_5h");
  assert.equal(f.posts.length, 1);
  assert.deepEqual(f.posts[0], { id: "goat", window: "window_5h", percent: 42.5, resets: 30 });
  assert.equal(f.usage.getUsage("goat").window_5h, 42.5);
  assert.equal(f.usage.getUsage("goat").window_week, null);
  assert.equal(f.usage.getUsage("goat").window_month, null);
  const recorded = f.usage.providerUsageFor("goat").value?.quota_windows.find((window) => window.window_kind === "five_hours");
  assert.equal(recorded?.used, 42.5);
  assert.notEqual(recorded?.used, 0);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.draft, 42.5);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.saved, 42.5);
});

test("an entered blank-draft percent survives a reread that is still unknown", async (t) => {
  const account = calibrationAccount("goat", "command-code");
  const f = await calibrationFixture(t, account, calibrationStatus("goat", "command-code", true, "quota"));
  f.usage.updateUsageDraft("goat", "window_5h", 42.5);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.draft, 42.5);
  await f.usage.loadAccountUsage("goat");
  assert.equal(f.reads.length, 2);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.draft, 42.5);
  assert.equal(f.usage.getUsage("goat").window_5h, null);
  assert.equal(f.posts.length, 0);
});

test("a stale session cannot apply a first calibration response", async (t) => {
  const account = calibrationAccount("goat", "command-code");
  const f = await calibrationFixture(t, account, calibrationStatus("goat", "command-code", true, "quota"));
  f.usage.updateUsageDraft("goat", "window_5h", 42.5);
  f.usage.updateResetsFirstField("goat", "window_5h", 0);
  f.usage.updateResetsSecondField("goat", "window_5h", 30);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.draft, 42.5);
  let calls = 0;
  let release!: (usage: UsageWindow) => void;
  dashboardApi.updateAccountUsage = () => {
    calls += 1;
    return new Promise<UsageWindow>((resolve) => { release = resolve; });
  };
  const saving = f.usage.saveUsage("goat", "window_5h");
  assert.equal(calls, 1);
  f.store.clear();
  await nextTick();
  release({
    account_id: "goat",
    window_5h: 42.5,
    window_week: null,
    window_month: null,
    resets_in_5h: "2026-10-01T00:30:00.000Z",
    resets_in_week: null,
    resets_in_month: null,
  });
  await saving;
  assert.equal(calls, 1);
  assert.equal(f.usage.getUsage("goat").window_5h, null);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.saved, undefined);
});

test("a changed binding drops a first calibration response", async (t) => {
  const account = calibrationAccount("goat", "command-code");
  const f = await calibrationFixture(t, account, calibrationStatus("goat", "command-code", true, "quota"));
  f.usage.updateUsageDraft("goat", "window_5h", 42.5);
  f.usage.updateResetsFirstField("goat", "window_5h", 0);
  f.usage.updateResetsSecondField("goat", "window_5h", 30);
  assert.equal(f.usage.usageEdits.value.goat?.window_5h?.draft, 42.5);
  let calls = 0;
  let release!: (usage: UsageWindow) => void;
  dashboardApi.updateAccountUsage = () => {
    calls += 1;
    return new Promise<UsageWindow>((resolve) => { release = resolve; });
  };
  const saving = f.usage.saveUsage("goat", "window_5h");
  assert.equal(calls, 1);
  f.accounts.value = [{ ...account, updated_at: "v2" }];
  await nextTick();
  await f.usage.loadAccountUsage("goat");
  release({
    account_id: "goat",
    window_5h: 42.5,
    window_week: null,
    window_month: null,
    resets_in_5h: null,
    resets_in_week: null,
    resets_in_month: null,
  });
  await saving;
  assert.equal(calls, 1);
  assert.equal(f.usage.getUsage("goat").window_5h, null);
});
