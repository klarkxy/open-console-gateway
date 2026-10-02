import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, appendFileSync, rmSync } from "node:fs";

// One-shot branch preparation, removed before the product PR.
const accountBytes = readFileSync("src/views/Accounts.vue");
assert.equal(createHash("sha1").update(`blob ${accountBytes.length}\0`).update(accountBytes).digest("hex"), "7713cee5db224c6caabf4edf33de2bfcb61eeb84", "Accounts source changed; refuse to patch an unreviewed base");
function edit(path, before, after) {
  const source = readFileSync(path, "utf8");
  const at = source.indexOf(before);
  assert.ok(at >= 0 && source.indexOf(before, at + before.length) < 0, `expected one match in ${path}: ${before.slice(0, 100)}`);
  writeFileSync(path, source.slice(0, at) + after + source.slice(at + before.length));
}
const view = "src/views/Accounts.vue";
edit(view, 'import { createAccountRefreshQueue, platformRefreshBinding, waitForAccountRefreshIdle, type AccountRefreshState } from "../domain/account-refresh-queue.ts";', 'import { createAccountRefreshQueue, platformRefreshBinding, waitForAccountRefreshIdle, type AccountRefreshState } from "../domain/account-refresh-queue.ts";\nimport { ACCOUNT_REFRESH_CONCURRENCY } from "../domain/account-refresh-scheduler.ts";');
edit(view, '} = useAccountUsage(accounts, now, providerCatalog, {', '} = useAccountUsage(accounts, now, providerCatalog, {\n  quotaOnly: true,');
edit(view, '    if (accountCapabilities(overlay, providerCatalog.value, destinationForAccountId(overlay.id)).testable) {', `    if (!parent && usageCompanionCatalog({
      providerId: overlay.provider_id,
      catalog: providerCatalog.value,
      destination: destinationForAccountId(overlay.id),
    }).kind !== "none") {
      utilities.push({
        key: "refresh-models",
        label: t("刷新模型目录"),
        accountId: menuTarget.id,
        accountName: menuTarget.name,
        disabled: busy.value || !accountIsReady(overlay) || Boolean(refreshStates.value[overlay.id]),
      });
    }
    if (accountCapabilities(overlay, providerCatalog.value, destinationForAccountId(overlay.id)).testable) {`);
edit(view, '    overlay ? usageRefreshLoadingFor(overlay.id).value : false,\n    providerCatalog.value,', '    overlay ? usageRefreshLoadingFor(overlay.id).value : false,\n    refreshStates.value[overlayId],\n    providerCatalog.value,');
edit(view, '  if (key === "test-connection") {\n    openAccountTest(accountId);', '  if (key === "refresh-models") {\n    queueAccountModelRefresh(accountId);\n    return;\n  }\n  if (key === "test-connection") {\n    openAccountTest(accountId);');
edit(view, '  void refreshQueue.enqueue(accountId, isCurrent, async current => {\n    if (parent) {', '  // Unknown snapshots and platform/model writes retain the exclusive lane.\n  const sharedQuota = !parent && billingStore.slotFor(accountId).value?.status?.officialRefresh === true;\n  void refreshQueue.enqueue(accountId, isCurrent, async current => {\n    if (parent) {');
edit(view, `      if (current()) await refreshAccountUsage(accountId);
    }
    if (current()) await destinationsStore.load();
  }).catch(error => {
    if (isCurrent()) message.error(t("刷新失败：{error}", { error: dashboardErrorDetail(error) }));
  });
}

function queuePlatformParentRefresh`, `      // A capability change must not turn a shared quota job into a model write.
      if (sharedQuota && billingStore.slotFor(accountId).value?.status?.officialRefresh !== true) return;
      if (current()) await refreshAccountUsage(accountId);
    }
  }, { exclusive: !sharedQuota }).then(async () => {
    // Release this account's refresh state and pool slot before projection I/O.
    if (isCurrent()) await destinationsStore.load();
  }).catch(error => {
    if (isCurrent()) message.error(t("刷新失败：{error}", { error: dashboardErrorDetail(error) }));
  });
}

function queueAccountModelRefresh(accountId: string): void {
  const account = accountsStore.byId.get(accountId);
  if (!account || platformStore.linkForAccount(accountId)) return;
  const destinationId = destinationForAccountId(accountId)?.id;
  const session = billingStore.sessionEpoch;
  const current = () => sessionStore.authenticated && billingStore.sessionEpoch === session
    && accountsStore.byId.get(accountId)?.updated_at === account.updated_at
    && destinationForAccountId(accountId)?.id === destinationId
    && !platformStore.linkForAccount(accountId);
  void refreshQueue.enqueue(accountId, current, async isCurrent => {
    await destinationsStore.load();
    if (isCurrent()) await refreshCompanionCatalog(accountId, isCurrent);
  }).catch(error => {
    if (current()) message.error(t("刷新失败：{error}", { error: dashboardErrorDetail(error) }));
  });
}

function queuePlatformParentRefresh`);
edit(view, '    await platformSectionRef.value?.refreshParent(platformStore.parents.find(row => row.id === parent.id)!);\n    if (isCurrent()) await destinationsStore.load();\n  }).catch(error => {', '    await platformSectionRef.value?.refreshParent(platformStore.parents.find(row => row.id === parent.id)!);\n  }).then(async () => {\n    if (current()) await destinationsStore.load();\n  }).catch(error => {');
edit(view, '  const overlay = loadIdentitiesOverlay();\n  try {\n    const loaded = await accountsStore.loadPresented();', `  // These are independent local reads. Start them together, but settle the
  // identity/endpoint binding before loading per-account billing snapshots.
  const overlay = loadIdentitiesOverlay();
  const projection = destinationsStore.load().catch(() => undefined);
  const connections = providersStore.connections
    ? Promise.resolve()
    : providersStore.loadConnections().then(() => undefined).catch(() => undefined);
  try {
    const loaded = await accountsStore.loadPresented();`);
edit(view, `    try {
      await destinationsStore.load();
    } catch {
      // A projection refusal must not hide the V3 account list.
    }
    if (!current()) return false;
    refreshCpaSnapshot();
    await overlay;
    if (!current()) return false;
    if (!providersStore.connections) {
      await providersStore.loadConnections().catch(() => undefined);
    }
    if (!current()) return false;
    // Limit concurrent provider/local usage reads for large account lists.`, `    await projection;
    if (!current()) return false;
    refreshCpaSnapshot();
    await Promise.all([overlay, connections]);
    if (!current()) return false;
    // Limit concurrent provider/local usage reads for large account lists.`);
edit(view, '  await mapWithConcurrency(list.filter(account => accountIsReady(account) && accountHasUsageDisplay(account)), 4, async account => {', '  await mapWithConcurrency(list.filter(account => accountIsReady(account)\n    && (accountHasUsageDisplay(account) || (!providerCatalog.value && !platformStore.linkForAccount(account.id)))), 4, async account => {');
edit(view, `  // Catalog and quota limits gate nothing but the usage fan-out inside
  // loadAccounts, so they fetch in parallel; the account list follows so
  // usage display decisions read the settled catalog.
  const registrationOptions = loadRegistrationOptions();
  await Promise.allSettled([loadProviderCatalog(), loadQuotaLimits()]);
  await loadAccounts();
  // loadAccounts already waits for the first connections snapshot.
  if (!providersStore.connections) await providersStore.loadConnections().catch(() => undefined);`, `  const registrationOptions = loadRegistrationOptions();
  const metadata = Promise.allSettled([loadProviderCatalog(), loadQuotaLimits()]);
  // Account rows and existing quota snapshots do not wait for pricing/catalog.
  await loadAccounts();
  await metadata;
  // Fill any newly classified rows without re-reading matching cached slots.
  await loadUsageSnapshots();`);
edit(view, 'const automaticRefresh = createAccountsAutoRefresh({\n  allowed:', 'const automaticRefresh = createAccountsAutoRefresh({\n  concurrency: ACCOUNT_REFRESH_CONCURRENCY,\n  allowed:');
edit(view, '  // Automatic writes advance CAS; keep the next card-layout edit on current tokens.', '  // Reconcile shared layout tokens once after the pass, not after each quota.');
edit(view, 'binding: `${account.updated_at}\\0${parent.id}\\0${parent.version}`,', 'binding: `${account.updated_at}\\0${platformRefreshBinding(parent)}`,');
edit(view, 'busy: platformStore.loading || Boolean(platformStore.refreshing[`${parent.id}:${account.id}`])', 'busy: platformStore.loading || Boolean(refreshStates.value[account.id])\n            || Boolean(platformStore.refreshing[`${parent.id}:${account.id}`])');
edit(view, '              if (await waitForPlatformRefresh(account.id, parent.id, current)) await platformStore.refreshChild(parent.id, account.id);\n            });', '              if (await waitForPlatformRefresh(account.id, parent.id, current)) await platformStore.refreshChild(parent.id, account.id);\n            }, { priority: "background" });');
edit(view, `      if (target) byId.set(account.id, { ...target, refresh: (isCurrent) => refreshQueue.enqueue(
        account.id, isCurrent, current => target.refresh(current),
      ) });`, `      if (target) byId.set(account.id, {
        ...target,
        busy: target.busy || Boolean(refreshStates.value[account.id]),
        refresh: (isCurrent) => refreshQueue.enqueue(
          account.id, isCurrent, current => target.refresh(current),
          { exclusive: false, priority: "background" },
        ),
      });`);
const usage = "src/domain/useAccountUsage.ts";
edit(usage, '    /** Manual companion work, including model-only accounts; covered by the refresh lock. */', '    /** Quota observations finish without model discovery; model-only fallback remains available. */\n    quotaOnly?: boolean;\n    /** Manual companion work, including model-only accounts; covered by the refresh lock. */');
edit(usage, '          await options?.afterUsageRefresh?.(accountId, isCurrent);\n          if (refreshed && isCurrent())', '          if (!canRefreshUsage || !options?.quotaOnly) {\n            await options?.afterUsageRefresh?.(accountId, isCurrent);\n          }\n          if (refreshed && isCurrent())');
const tests = "src/domain/useAccountUsage.test.ts";
edit(tests, '  afterUsageRefresh?: (accountId: string, isCurrent: () => boolean) => Promise<void>,\n) {', '  afterUsageRefresh?: (accountId: string, isCurrent: () => boolean) => Promise<void>,\n  quotaOnly = false,\n) {');
edit(tests, '    afterUsageRefresh,\n  }))!;', '    afterUsageRefresh,\n    quotaOnly,\n  }))!;');
appendFileSync(tests, `

test("quota-only mode completes without starting slow companion discovery", async (t) => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; await f.request.promise; }, true);
  f.store.refreshUsage = async () => status(80);
  await f.usage.refreshAccountUsage("a");
  assert.equal(companions, 0);
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
  assert.equal(f.notifications(), 1);
});

test("quota-only mode retains manual model-only fallback and its account lock", async (t) => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; await f.request.promise; }, true);
  billingApi.status = async () => ({ ...status(10), officialRefresh: false });
  await f.usage.loadAccountUsage("a");
  f.store.refreshUsage = async () => { assert.fail("model-only account sent a quota POST"); };
  const refresh = f.usage.refreshAccountUsage("a");
  assert.equal(companions, 1);
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, true);
  await f.usage.refreshAccountUsage("a");
  assert.equal(companions, 1);
  f.request.resolve(savedUsage);
  await refresh;
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
});

test("quota-only failures keep the cached value and never invoke model writes", async (t) => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; }, true);
  f.store.refreshUsage = async () => { throw new Error("offline"); };
  await f.usage.refreshAccountUsage("a");
  assert.equal(companions, 0);
  assert.equal(f.usage.getUsage("a").window_5h, 10);
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
});
`);
edit("src/domain/accounts-auto-refresh.ts", 'import { ACCOUNT_REFRESH_CONCURRENCY } from "./account-refresh-scheduler.ts";\n', '');
edit("src/domain/accounts-auto-refresh.ts", '  const concurrency = options.concurrency ?? ACCOUNT_REFRESH_CONCURRENCY;', '  // Callers that mix control-plane writes keep the serial default; the\n  // Accounts quota scheduler explicitly opts into its bounded shared pool.\n  const concurrency = options.concurrency ?? 1;');
edit("src/domain/accounts-auto-refresh.test.ts", 'test("the default pool advances fast accounts before the slow first account completes"', 'test("a four-worker pool advances fast accounts before the slow first account completes"');
edit("src/domain/accounts-auto-refresh.test.ts", '    allowed: () => true, targets: () => targets,\n    afterRefresh:', '    allowed: () => true, targets: () => targets, concurrency: 4,\n    afterRefresh:');
edit("src/domain/usage-refresh-catalog.ts", ' * How **Refresh quota** should also refresh models for this card, so Accounts\n * does not send the operator to Providers to hunt for 刷新模型目录.', ' * Model-discovery capability for the account\'s independent catalog action\n * and the manual fallback on model-only accounts.');
edit("docs/user/accounts.md", '**Refresh quota** also refreshes that destination’s model list (the official Provider catalog for built-in Plans; `/v1/models` discovery for Custom / known-host balance cards and platform Keys), so you do not need to open **Providers** only to refresh the catalog.', '**Refresh quota** on ordinary quota/balance accounts completes independently of model discovery. Use the row menu’s **Refresh model catalog** for the official Provider catalog or Custom / known-host `/v1/models` discovery. Model-only accounts keep their manual model-refresh fallback; platform Keys keep their existing platform synchronization.');
edit("docs/user/accounts.md", '\nGOAT cards offer **Refresh quota**', '\nAccount rows and saved quota snapshots load independently of catalog/pricing metadata. Same-session, same-binding snapshots remain visible while revalidating, including after a failed upstream refresh. Ordinary quota observations share a pool of at most four requests and publish per account; a slow account does not block completed peers. Duplicate requests for one account share completion, and queued manual requests take priority over background requests. Platform synchronization and model catalog writes remain exclusive to preserve CAS. Automatic refresh still runs only while the Accounts page is active and visible, respects freshness and server retry deadlines, and reconciles the destination projection once per pass. It is not a new server-wide background poller.\n\nGOAT cards offer **Refresh quota**');
edit("docs/user/accounts.zh-CN.md", '**刷新额度**会同时刷新该目的地的模型列表（内置 Plan 走官方目录刷新；Custom / 已知主机余额卡和平台 Key 走 `/v1/models` 发现），不必只为刷新目录再打开**供应商**页。', '普通额度／余额账号的**刷新额度**独立完成，不再等待模型发现。模型更新使用行菜单的**刷新模型目录**（内置 Plan 走官方目录；Custom / 已知主机余额卡走 `/v1/models`）。仅支持模型的账号保留手动刷新模型的回退行为；平台 Key 保留原有平台同步。');
edit("docs/user/accounts.zh-CN.md", '\nGOAT 卡片可通过 **刷新额度**', '\n账号列表和已有额度快照不再等待目录／定价元数据才开始加载。同一会话、同一账号绑定下的快照在重新验证时继续显示，上游刷新失败也保留旧值。普通额度读取最多四路并发，每个账号独立回填，慢账号不阻塞已经完成的其他账号。同账号重复请求合并，尚未执行的手动任务优先于后台任务；平台同步和模型目录写入继续互斥以保护 CAS。自动刷新仍只在账号页激活且可见时进行，遵守新鲜度与服务端重试时间，每轮只统一重读一次目的地投影；本次没有新增服务端全局轮询。\n\nGOAT 卡片可通过 **刷新额度**');
rmSync("scripts/prepare-quota-fix.mjs");
rmSync(".github/workflows/prepare-quota-fix.yml");
console.log("Applied reviewed quota-refresh changes; temporary preparation files removed.");
