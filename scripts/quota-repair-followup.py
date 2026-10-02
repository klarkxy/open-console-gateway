from pathlib import Path


def replace(path, old, new):
    p = Path(path)
    text = p.read_text()
    if (new and new in text) or (not new and old not in text):
        return
    assert text.count(old) == 1, (path, old[:100])
    p.write_text(text.replace(old, new))


def append(path, marker, text):
    p = Path(path)
    if marker not in p.read_text():
        p.write_text(p.read_text() + text)


replace('src/stores/billing.test.ts', '    store.dropSession();', '    store.clear();')
replace('src/domain/useAccountUsage.ts', '    await billing.loadMany(targets);', '''    const current = new Map(targets.map(target => {
      const account = accounts.value.find(account => account.id === target.accountId)!;
      return [target.accountId, requestStillCurrent(account)];
    }));
    await billing.loadMany(targets);
    for (const { accountId } of targets) {
      if (!current.get(accountId)?.()) continue;
      const account = accounts.value.find(account => account.id === accountId);
      if (account && usageCapabilities(account).manual) syncUsageEdits(accountId, getUsage(accountId));
    }''')
replace('crates/ocg-core/src/dashboard_v4/types.rs', '    include_type::<crate::billing_types::BillingSnapshotRequest>(&mut serialize);\n', '')
replace('crates/ocg-core/src/dashboard_v4/types.rs', '    let mut deserialize = SchemaSettings::draft2020_12().into_generator();', '''    let mut deserialize = SchemaSettings::draft2020_12().into_generator();
    include_type::<crate::billing_types::BillingSnapshotRequest>(&mut deserialize);''')

# Future credit buckets are intentionally absent from the public projection.
# Include their activation deadline from persisted state without changing DTOs.
replace('crates/ocg-core/src/dashboard_v4/billing.rs', '        let after = ReadVersion::capture(state, db).map_err(V3ApiError::internal)?;', '''        let next_credit_start = if status.credits.is_some() {
            storage::load_on(&db.conn, id).map_err(V3ApiError::internal)?
                .and_then(|meter| meter.buckets.iter().map(|bucket| bucket.starts_at)
                    .filter(|at| *at > now).min())
        } else { None };
        let after = ReadVersion::capture(state, db).map_err(V3ApiError::internal)?;''')
replace('crates/ocg-core/src/dashboard_v4/billing.rs', '        state.billing_cache.lock().insert(&after, status.clone(), now);', '''        let mut cache = state.billing_cache.lock();
        cache.insert(&after, status.clone(), now);
        if let Some(at) = next_credit_start { cache.shorten_lifetime(id, at); }''')
replace('crates/ocg-core/src/dashboard_v4/billing_cache.rs', '    pub(super) fn insert(&mut self,', '''    pub(super) fn shorten_lifetime(&mut self, id: &str, at: DateTime<Utc>) {
        if let Some(entry) = self.entries.get_mut(id) { entry.valid_until = entry.valid_until.min(at); }
    }
    pub(super) fn insert(&mut self,''')

append('crates/ocg-core/tests/dashboard_v4_official_api.rs', 'concurrent_balance_refreshes_share_one_request_and_cached_reads_stay_local', r'''

#[tokio::test]
async fn concurrent_balance_refreshes_share_one_request_and_cached_reads_stay_local() {
    let h = start_loopback("balance-singleflight").await;
    clock(&h, now());
    let (_, id) = create(&h, "deepseek").await;
    let (base, calls, _stop) = start_fake_upstream_with_delay(HashMap::from([(
        KEY.into(), VecDeque::from([
            FakeReply { status: 200, body: BALANCE },
            FakeReply { status: 500, body: "provider down" },
        ]),
    )]), std::time::Duration::from_millis(500)).await;
    let _guard = install_official_api_endpoint_for_test(
        h.state.process_generation(), BALANCE_URL, &format!("{base}/user/balance"),
    ).unwrap();
    let path = format!("/accounts/{id}/official-api/balance");
    let billing = format!("/accounts/{id}/billing");
    for _ in 0..2 {
        let (status, body) = send(&h, Method::GET, &billing, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_safe(&body);
    }
    assert!(calls.lock().unwrap().is_empty(), "snapshot reads cannot fetch upstream");
    let (left, right) = tokio::join!(
        send(&h, Method::POST, &path, cas(&h)),
        send(&h, Method::POST, &path, cas(&h)),
    );
    assert_eq!(left.0, StatusCode::OK, "{}", left.1);
    assert_eq!(right.0, StatusCode::OK, "{}", right.1);
    assert_eq!(left.1["balances"], right.1["balances"]);
    assert_eq!(calls.lock().unwrap().len(), 1);
    let (status, snapshot) = send(&h, Method::POST, "/billing/snapshots", json!({"accountIds":[id]})).await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");
    assert_eq!(snapshot["statuses"][0]["cash"]["balances"], left.1["balances"]);
    assert_eq!(calls.lock().unwrap().len(), 1);
    clock(&h, now() + Duration::seconds(16));
    let (failed_left, failed_right) = tokio::join!(
        send(&h, Method::POST, &path, cas(&h)),
        send(&h, Method::POST, &path, cas(&h)),
    );
    assert_eq!(failed_left.0, StatusCode::BAD_GATEWAY);
    assert_eq!(failed_right.0, StatusCode::BAD_GATEWAY);
    assert_eq!(calls.lock().unwrap().len(), 2, "failures are also shared");
    let (status, retained) = send(&h, Method::GET, &billing, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{retained}");
    assert_eq!(retained["cash"]["balances"], left.1["balances"], "failure must retain amounts and observation times");
    assert_safe(&retained);
}
''')
append('crates/ocg-core/src/dashboard_v4/billing_cache/tests.rs', 'quota_reset_deadline_expires_before_the_normal_cache_age', r'''

#[test]
fn quota_reset_deadline_expires_before_the_normal_cache_age() {
    let now = Utc::now();
    let reset = now + Duration::seconds(1);
    let mut value = status("timed");
    value.usage = Some(serde_json::from_value(serde_json::json!({
        "accountId":"timed", "providerId":"opencode", "availability":"available",
        "experimental":false, "freeCooldownUntil":reset.to_rfc3339(),
        "quotaWindows":[], "creditBalances":[], "syncState":null,
        "revision":1, "processGeneration":1, "pricingRevision":null
    })).unwrap());
    let mut cache = BillingReadCache::default();
    cache.insert(&version(), value, now);
    assert!(cache.get("timed", &version(), now).is_some());
    assert!(cache.get("timed", &version(), reset).is_none());
}
''')
append('crates/ocg-core/src/dashboard_v4/billing/tests.rs', 'cache_expires_when_a_not_yet_visible_credit_bucket_becomes_active', r'''

#[tokio::test]
async fn cache_expires_when_a_not_yet_visible_credit_bucket_becomes_active() {
    let (dir, state) = state();
    account(&state, "future-credits");
    let start = Utc::now() + chrono::Duration::seconds(10);
    let mut future = bucket(25.0);
    future["startsAt"] = serde_json::json!(start);
    configure(State(state.clone()), Path("future-credits".into()), body(&state, serde_json::json!({
        "configuration": configuration(), "initialBuckets": [future]
    }))).await.unwrap();
    let snapshot = status(&state, "future-credits").unwrap();
    assert!(snapshot.credits.unwrap().buckets.is_empty());
    let version = super::super::billing_cache::ReadVersion::capture(&state, &state.db.lock()).unwrap();
    assert!(state.billing_cache.lock().get("future-credits", &version, start).is_none());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
''')
append('src/stores/billing.test.ts', 'batch loading bounds requests and never overwrites a newer binding', r'''

test("batch loading bounds requests and never overwrites a newer binding", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const oldSnapshots = billingApi.snapshots;
  const oldStatus = billingApi.status;
  try {
    const requests: string[][] = [];
    billingApi.snapshots = async ids => {
      requests.push(ids);
      return { statuses: ids.map(accountId => billingStatus({ accountId })), errors: {}, revision: 3, processGeneration: 99 };
    };
    await store.loadMany(Array.from({ length: 65 }, (_, i) => ({ accountId: `a${i}`, binding: "v1" })));
    assert.deepEqual(requests.map(ids => ids.length), [32, 32, 1]);
    let resolve!: (value: Awaited<ReturnType<typeof billingApi.snapshots>>) => void;
    billingApi.snapshots = () => new Promise(done => { resolve = done; });
    const oldRead = store.loadMany([{ accountId: "a0", binding: "v1" }]);
    billingApi.status = async accountId => billingStatus({ accountId, revision: 7 });
    await store.load("a0", "v2");
    resolve({ statuses: [billingStatus({ accountId: "a0", revision: 3 })], errors: {}, revision: 3, processGeneration: 99 });
    await oldRead;
    assert.equal(store.slotFor("a0").value?.boundVersion, "v2");
    assert.equal(store.slotFor("a0").value?.status?.revision, 7);
  } finally {
    billingApi.snapshots = oldSnapshots;
    billingApi.status = oldStatus;
  }
});
''')
print('Applied additional quota regression coverage.')
