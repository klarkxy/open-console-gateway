from pathlib import Path


def replace(path, old, new):
    p = Path(path)
    text = p.read_text()
    if new in text:
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
