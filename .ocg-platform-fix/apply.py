from pathlib import Path
import re
import textwrap


def replace(path, old, new):
    p = Path(path)
    text = p.read_text()
    assert text.count(old) == 1, (path, old[:120], text.count(old))
    p.write_text(text.replace(old, new, 1))


def append(path, content):
    p = Path(path)
    p.write_text(p.read_text().rstrip() + '\n\n' + content.strip() + '\n')


# Pure refresh persistence and a scope-specific optimistic concurrency token.
replace('crates/ocg-core/src/platform.rs', 'pub mod reader;', 'pub mod reader;\npub(crate) mod refresh;')
replace('crates/ocg-core/src/db/platform.rs', '''        let mut saved = snapshot.clone();
        if snapshot.stale
            && let Some(mut old) = previous
        {
            old.stale = true;
            old.errors = snapshot.errors.clone();
            saved = old;
        }''', '''        let saved = crate::platform::refresh::merge_snapshot(previous.as_ref(), snapshot);''')
replace('crates/ocg-core/src/db/platform.rs', '''        let mut token = format!("{}:{}", parent.id, parent.version);
        if let Some(id) = account_id {
            let version = link_version_on(&self.conn, id, parent_id)?;
            let account = self.get_account(id)?.context("account not found")?;
            // Private comparison token; never returned or logged.
            token.push_str(&format!(":{id}:{version}:{}", account.key_cipher));
        }
        Ok(token)''', '''        if let Some(id) = account_id {
            let version = link_version_on(&self.conn, id, parent_id)?;
            let account = self.get_account(id)?.context("account not found")?;
            return crate::platform::refresh::refresh_identity(
                &parent,
                self.platform_credential_cipher(parent_id)?.as_deref(),
                Some((id, version, &account.key_cipher)),
            );
        }
        crate::platform::refresh::refresh_identity(&parent, None, None)''')

# Do not fetch a management wallet while refreshing a Sub2API inference Key.
replace('crates/ocg-core/src/platform/reader.rs', '''    if let Some(user) = user {
        match get_json(
            client,
            base,
            "api/v1/user/profile",''', '''    if let Some(user) = user
        && key.is_none()
    {
        match get_json(
            client,
            base,
            "api/v1/user/profile",''')
replace('crates/ocg-core/src/platform/reader.rs', '''                .any(|quota| matches!(quota.kind, PlatformQuotaKind::Wallet))''', '''                .any(|quota| {
                    matches!(quota.kind, PlatformQuotaKind::Wallet)
                        && quota.source == "sub2api.v1.usage"
                })''')
append('crates/ocg-core/src/platform/tests.rs', r'''
#[test]
fn key_authenticated_balance_is_not_suppressed_by_a_management_wallet() {
    let mut snapshot = PlatformSnapshot::default();
    parse_sub2_profile(&json!({"balance": 100}), &mut snapshot);
    parse_sub2_usage(&json!({"mode": "unrestricted", "balance": 20}), &mut snapshot).unwrap();
    snapshot.quotas.retain(|quota| matches!(quota.kind, PlatformQuotaKind::KeyLimit) || quota.source == "sub2api.v1.usage");
    let wallet = snapshot.quotas.iter().find(|quota| matches!(quota.kind, PlatformQuotaKind::Wallet)).unwrap();
    assert_eq!(wallet.remaining, Some(20.0));
    assert_eq!(wallet.source, "sub2api.v1.usage");
}

#[tokio::test]
async fn sub2_key_refresh_does_not_fetch_management_wallet_subscriptions_or_groups() {
    let mut routes = HashMap::new();
    routes.insert("/api/v1/user/profile".into(), Route::ok(r#"{"code":0,"data":{"balance":100}}"#));
    routes.insert("/v1/models".into(), Route::ok(r#"{"data":[{"id":"model-a"}]}"#));
    routes.insert("/v1/usage".into(), Route::ok(r#"{"mode":"unrestricted","balance":20}"#));
    routes.insert("/v1/sub2api/billing".into(), Route::ok(r#"{"object":"sub2api.key_billing","schema_version":1,"billing_scope":"token","effective_rate_multiplier":1}"#));
    routes.insert("/api/v1/model-plaza".into(), Route::ok(r#"{"code":0,"data":{"groups":[]}}"#));
    let (base, client, hits) = spawn_mock(routes).await;
    let group = PlatformGroup::default();
    let snapshot = read(&client, &PlatformReadRequest {
        kind: PlatformKind::Sub2api, base_url: &base, user_credential: Some(USER),
        key: Some(KEY), group: &group, now: now(),
    }).await;
    let wallet = snapshot.quotas.iter().find(|quota| matches!(quota.kind, PlatformQuotaKind::Wallet)).unwrap();
    assert_eq!(wallet.remaining, Some(20.0));
    assert_eq!(wallet.source, "sub2api.v1.usage");
    let hits = hits.lock().unwrap();
    for forbidden in ["/api/v1/user/profile", "/api/v1/subscriptions/summary", "/api/v1/groups/available"] {
        assert!(!hits.iter().any(|hit| hit.path == forbidden));
    }
    assert!(hits.iter().any(|hit| hit.path == "/v1/usage" && hit.authorization.as_deref() == Some(format!("Bearer {KEY}").as_str())));
    assert!(hits.iter().any(|hit| hit.path == "/api/v1/model-plaza" && hit.authorization.as_deref() == Some(format!("Bearer {USER}").as_str())));
}
''')

# Keep the existing import tests, but move them to a sibling tests.rs module.
old_import = Path('crates/ocg-core/src/platform/import.rs').read_text()
marker = '#[cfg(test)]\nmod tests {'
assert old_import.count(marker) == 1
old_tests = old_import.split(marker, 1)[1].strip()
assert old_tests.endswith('}')
old_tests = textwrap.dedent(old_tests[:-1].strip('\n'))
Path('crates/ocg-core/src/platform/import.rs').write_text(Path('.ocg-platform-fix/import.rs').read_text())
test_path = Path('crates/ocg-core/src/platform/import/tests.rs')
test_path.parent.mkdir(parents=True, exist_ok=True)
test_path.write_text(old_tests + '\n' + Path('.ocg-platform-fix/import-tests-extra.rs').read_text())

# Additive V4 page/continuation fields; old clients default to the first page.
replace('crates/ocg-core/src/dashboard_v4/types.rs', 'pub struct PlatformKeyImportRequest {', '''pub struct PlatformKeyImportRequest {
    /// One bounded remote page; omission starts at page one.
    #[serde(default)]
    pub page: Option<u32>,''')
replace('crates/ocg-core/src/dashboard_v4/types.rs', 'pub struct PlatformKeyImportResult {', '''pub struct PlatformKeyImportResult {
    /// More remote rows exist, even if every row in this batch was skipped.
    pub next_page: Option<u32>,''')
path = 'crates/ocg-core/src/dashboard_v4/platform_keys.rs'
replace(path, '''    let input = parse_mutation_json::<PlatformKeyImportRequest>(&body)?;''', '''    let input = parse_mutation_json::<PlatformKeyImportRequest>(&body)?;
    let page = input.page.unwrap_or(1);
    if !(1..=import::MAX_PAGE).contains(&page) {
        return Err(V3ApiError::invalid_request_at(&state, "invalid import page"));
    }''')
replace(path, '''    let (secrets, skipped_disabled, mut failed) =
        import::collect_remote_secrets(&client, &origin, &credential)''', '''    let import::RemoteKeyBatch { secrets, skipped_disabled, mut failed, next_page } =
        import::collect_remote_secrets(&client, &origin, &credential, page)''')
replace(path, '''    let config = state.config();
    let mut pending:''', '''    let known_keys = local_custom_keys(&state, &id)?;
    let mut skipped_existing = 0_u32;
    let config = state.config();
    let mut pending:''')
replace(path, '''    for secret in secrets {
        match custom::discover_custom_models(''', '''    for secret in secrets {
        if known_keys.contains(&secret.key) {
            skipped_existing += 1;
            continue;
        }
        match custom::discover_custom_models(''')
replace(path, '''    let mut imported = 0_u32;
    let mut skipped_existing = 0_u32;''', '''    let mut imported = 0_u32;''')
replace(path, 'let mut existing_keys = local_custom_keys(&state)?;', 'let mut existing_keys = local_custom_keys(&state, &id)?;')
replace(path, '''    Ok(Json(PlatformKeyImportResult {
        imported,''', '''    Ok(Json(PlatformKeyImportResult {
        next_page,
        imported,''')
replace(path, '''fn local_custom_keys(state: &CoreState) -> Result<HashSet<String>, V3ApiError> {
    let db = state.db.lock();
    let mut keys = HashSet::new();''', '''fn local_custom_keys(state: &CoreState, parent_id: &str) -> Result<HashSet<String>, V3ApiError> {
    let db = state.db.lock();
    let linked_ids: HashSet<String> = db.list_platform_links().map_err(V3ApiError::internal)?
        .into_iter().filter(|link| link.platform_account_id == parent_id)
        .map(|link| link.account_id).collect();
    let mut keys = HashSet::new();''')
replace(path, '''        if account.provider_id != CUSTOM_PROVIDER_ID {''', '''        if account.provider_id != CUSTOM_PROVIDER_ID || !linked_ids.contains(&account.id) {''')

# Metadata refresh and model-configuration writes are separate explicit actions.
path = 'src/components/PlatformAccountsSection.vue'
replace(path, 'import { accountCapabilities } from "../domain/account-capabilities.ts";', '''import { accountCapabilities } from "../domain/account-capabilities.ts";
import { platformRefreshErrors } from "../domain/platform-refresh.ts";''')
replace(path, '''function notifyRefreshOutcome(parentId: string): void {
  const latest = platformStore.parents.find((item) => item.id === parentId);
  const errors = latest?.snapshot?.errors ?? [];''', '''function notifyRefreshOutcome(parentId: string, accountId?: string): void {
  const errors = platformRefreshErrors(platformStore.parents, platformStore.links, parentId, accountId);''')
replace(path, '''      const ids = new Set(
        platformStore.links
          .filter((link) => link.platformAccountId === parent.id)
          .map((link) => link.accountId),
      );
      await fetchModelsAll(props.accounts.filter((account) => ids.has(account.id)));
''', '')
replace(path, '''      notifyRefreshOutcome(parent.id);
      const account = props.accounts.find((item) => item.id === link.accountId);
      if (account) await fetchModels(account);''', '''      notifyRefreshOutcome(parent.id, link.accountId);''')
replace(path, '''    else if (outcome === "ok") message.success(t("平台账号已删除"));''', '''    else if (outcome === "ok") {
      message.success(t("平台账号已删除"));
      emit("changed");
    }''')
replace(path, '''    if (result.imported === 0 && result.failed.length === 0) {''', '''    if (result.imported === 0 && result.failed.length === 0 && result.nextPage == null) {''')
replace(path, '''    const parts = [t("已导入 {imported} 把 Key", { imported: result.imported })];''', '''    const parts = [t("已导入 {imported} 把 Key", { imported: result.imported })];
    if (result.nextPage != null) parts.push(t("还有更多 Key；再次导入将继续下一批。"));''')
replace(path, '''      account_id: account.id,
    });
    if (discovery.models.length === 0) {''', '''      account_id: account.id,
    });
    if (discovery.truncated) {
      message.warning(t("模型列表被截断，未修改已保存的模型。"));
      return;
    }
    if (discovery.models.length === 0) {''')

# Continuation belongs to the server-state owner, not a component-local ref.
path = 'src/api/platform-accounts.ts'
replace(path, '''  importKeys: (id: string): Promise<PlatformKeyImportResult> =>''', '''  importKeys: (id: string, page?: number): Promise<PlatformKeyImportResult> =>''')
replace(path, '''        body: JSON.stringify({ ...expectation }),''', '''        body: JSON.stringify({ ...expectation, ...(page !== undefined ? { page } : {}) }),''')
replace(path, '''export interface PlatformKeyImportResult {
  imported:''', '''export interface PlatformKeyImportResult {
  /** Absent on older servers; a positive page continues this bounded import. */
  nextPage?: number | null;
  imported:''')
replace(path, '''  return {
    imported: num(dto.imported),''', '''  return {
    nextPage: typeof dto.nextPage === "number" && Number.isInteger(dto.nextPage) && dto.nextPage > 0
      ? dto.nextPage : null,
    imported: num(dto.imported),''')
path = 'src/stores/platformAccounts.ts'
replace(path, '''  const importing = ref<Record<string, boolean>>({});''', '''  const importing = ref<Record<string, boolean>>({});
  const nextImportPage = ref<Record<string, number>>({});''')
replace(path, '''next.links.some((link) => link.accountId === pendingLink.value!.accountId)''', '''next.links.some((link) => link.accountId === pendingLink.value!.accountId && link.platformAccountId === pendingLink.value!.parentId)''')
replace(path, '''      await platformAccountsApi.remove(parentId);
      try {''', '''      await platformAccountsApi.remove(parentId);
      delete nextImportPage.value[parentId];
      try {''')
replace(path, '''    refreshing.value[parentId] = true;
    try {
      acceptView(await platformAccountsApi.refresh(parentId));
      return "ok";
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      refreshing.value[parentId] = false;
    }''', '''    const session = sessionEpoch;
    refreshing.value[parentId] = true;
    try {
      const next = await platformAccountsApi.refresh(parentId);
      if (session !== sessionEpoch) return "error";
      acceptView(next);
      return "ok";
    } catch (e) {
      if (session !== sessionEpoch) return "error";
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      if (session === sessionEpoch) refreshing.value[parentId] = false;
    }''')
replace(path, '''    acceptView(await platformAccountsApi.refresh(parentId, accountId));
  }''', '''    const session = sessionEpoch;
    const next = await platformAccountsApi.refresh(parentId, accountId);
    if (session === sessionEpoch) acceptView(next);
  }''')
replace(path, '''    mutating.value = true;
    importing.value[parentId] = true;
    try {
      const result = await platformAccountsApi.importKeys(parentId);
      acceptView(await platformAccountsApi.list());
      return result;
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      mutating.value = false;
      importing.value[parentId] = false;
    }''', '''    const session = sessionEpoch;
    mutating.value = true;
    importing.value[parentId] = true;
    try {
      const result = await platformAccountsApi.importKeys(parentId, nextImportPage.value[parentId]);
      if (session !== sessionEpoch) return "error";
      if (result.nextPage != null) nextImportPage.value[parentId] = result.nextPage;
      else delete nextImportPage.value[parentId];
      try {
        const next = await platformAccountsApi.list();
        if (session === sessionEpoch) acceptView(next);
      } catch (e) {
        // Import already committed. A failed revalidation must not hide its
        // result or lose the page continuation and invite a duplicate write.
        if (session === sessionEpoch) error.value = dashboardErrorDetail(e);
      }
      return session === sessionEpoch ? result : "error";
    } catch (e) {
      if (session !== sessionEpoch) return "error";
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      if (session === sessionEpoch) {
        mutating.value = false;
        importing.value[parentId] = false;
      }
    }''')
replace(path, '''    importing.value = {};
    refreshing.value = {};''', '''    importing.value = {};
    nextImportPage.value = {};
    refreshing.value = {};''')

path = 'src/i18n/messages/en-US.ts'
text = Path(path).read_text()
match = re.search(r'export const enUSMessages\s*=\s*\{', text)
assert match, path
text = text[:match.end()] + '\n  "还有更多 Key；再次导入将继续下一批。": "More Keys remain; import again to continue with the next batch.",\n  "模型列表被截断，未修改已保存的模型。": "The model list was truncated; saved models were not changed.",' + text[match.end():]
Path(path).write_text(text)

append('docs/user/platform-accounts.md', '''## Refresh isolation and bounded imports

The parent **Refresh** and each Key's **Refresh** update observations only. They do not run model discovery a second time or change saved model routing. Use **Fetch models** explicitly for that configuration change. A truncated discovery result leaves the existing model configuration unchanged; empty and failed results likewise do not replace it.

A Key refresh reports that Key's errors, not an old parent error. Sub2API Key refresh does not fetch the management user's wallet, subscriptions, or available groups. A saved management token may still authenticate the separate model-plaza price lookup. A Key-authenticated balance remains scoped to the Key.

When an endpoint fails, successful components are saved and only rows from failed sources retain their last-known values. A partial snapshot remains globally marked stale for the conservative price estimator, and retained prices never receive a renewed expiry. The snapshot time describes the latest attempt, not proof every retained row was observed then. A parent observation no longer invalidates an in-flight child refresh; origin, management credential, link, and inference Key changes still invalidate it.

New API import copies at most 50 remote rows per action and returns an explicit `nextPage`. Even a page of entirely disabled or already imported Keys can have a continuation. Invoke **Import Keys from site** again to continue in the current session. Completing all pages resets the next import to page one. Reloading or signing out also resets the cursor. This is bounded copying, not remote synchronization; rescan after concurrent remote inventory changes. Duplicate secrets are checked within the selected platform instance, and already imported Keys skip model discovery.
''')
append('docs/user/platform-accounts.zh-CN.md', '''## 刷新隔离与分批导入

父账号和每把 Key 的**刷新**只更新观察数据，不再二次发现模型或改写已保存的模型路由。需要修改模型配置时，明确使用**获取模型**。模型发现结果被截断时保留原配置；空列表和获取失败同样不替换原配置。

Key 刷新只报告这把 Key 的错误，不再显示父账号的旧错误。Sub2API Key 刷新不读取管理用户的钱包、订阅或可用分组；已保存的管理令牌仍可用于独立的模型广场价格查询。通过 Key 自身认证得到的余额保留在该 Key 范围。

某个接口失败时，成功分项仍会保存，只有失败来源的数据保留旧值。部分失败的快照仍整体标记过期，价格估算继续保守停用；保留的价格不会延长有效期。快照时间表示最近一次尝试，不表示每条保留的数据都在该时刻更新。父账号观察刷新不再使进行中的子 Key 刷新冲突；站点地址、管理凭证、关联及推理 Key 的变化仍使旧结果失效。

New API 每次最多复制 50 条远端记录，并明确返回 `nextPage`。即使整批都是停用或已导入的 Key，也可能仍有下一批。在当前会话再次点击**从站点导入 Key**即可继续；全部完成后，下一次从第一页重新扫描。刷新页面或退出登录也会重置游标。这是有界复制，不是远端同步；远端清单并发变化后应重新扫描。重复密钥只在所选平台实例内检查，已导入的 Key 不再重复发现模型。
''')

append('src/stores/platformAccounts.test.ts', r'''
test("platform refresh: a pending link is not completed by another parent", () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = usePlatformAccountsStore();
  store.setPendingLink({ accountId: "key", parentId: "expected" });
  store.acceptView(platformView("other", 1, 99, [platformLink("key", "other")]));
  assert.deepEqual(store.pendingLink, { accountId: "key", parentId: "expected" });
});

test("platform refresh: a parent response after session clear cannot repopulate state", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = usePlatformAccountsStore();
  const pending = store.refreshParent("parent");
  await waitForCalls(calls, 1);
  store.clear();
  calls[0]!.resolve(listBody("parent", 8, 99));
  assert.equal(await pending, "error");
  assert.equal(store.view, null);
  assert.equal(store.loaded, false);
});

test("platform import: a committed continuation survives failed list revalidation", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = usePlatformAccountsStore();
  const first = store.importKeys("parent");
  await waitForCalls(calls, 1);
  assert.equal((calls[0]!.body as { page?: number }).page, undefined);
  calls[0]!.resolve({ imported: 0, skippedExisting: 0, skippedDisabled: 50, failed: [], nextPage: 2,
    revision: { revision: 7, processGeneration: 99, pricingRevision: null } });
  await waitForCalls(calls, 2);
  calls[1]!.reject(new Error("list unavailable"));
  const result = await first;
  assert.notEqual(typeof result, "string");
  assert.equal(typeof result === "object" ? result.nextPage : null, 2);
  assert.equal(store.error, "list unavailable");
  const second = store.importKeys("parent");
  await waitForCalls(calls, 3);
  assert.equal((calls[2]!.body as { page?: number }).page, 2);
  calls[2]!.resolve({ imported: 1, skippedExisting: 0, skippedDisabled: 0, failed: [], nextPage: null,
    revision: { revision: 8, processGeneration: 99, pricingRevision: null } });
  await waitForCalls(calls, 4);
  calls[3]!.resolve(listBody("parent", 8, 99));
  await second;
  assert.equal(store.error, "");
  const third = store.importKeys("parent");
  await waitForCalls(calls, 5);
  assert.equal((calls[4]!.body as { page?: number }).page, undefined);
  calls[4]!.resolve({ imported: 0, skippedExisting: 1, skippedDisabled: 0, failed: [], nextPage: null,
    revision: { revision: 8, processGeneration: 99, pricingRevision: null } });
  await waitForCalls(calls, 6);
  calls[5]!.resolve(listBody("parent", 8, 99));
  await third;
});
''')

print('Applied platform refresh isolation, scoped persistence, bounded import, and UI regressions.')
