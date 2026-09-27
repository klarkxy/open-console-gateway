from pathlib import Path
import re

path = Path('crates/ocg-core/src/platform/tests.rs')
text = path.read_text()
# These legacy cases made a single request with both credentials and expected
# parent subscriptions alongside a Key's prices. Keep those assertions, but
# exercise the two explicit scopes against the same official-shaped fixtures.
cases = {
    'sub2_user_and_key_data_keep_composite_members_and_bill_once': True,
    'sub2_composite_billing_and_scopes_are_explicit': True,
    'sub2_plaza_uses_official_per_token_payload_and_never_key_auth': False,
    'sub2_user_subscriptions_do_not_zero_missing_windows': False,
}
changed = []
for name, limited in cases.items():
    pattern = r'(?m)^#\[tokio::test\]\nasync fn ' + re.escape(name) + r'\(\) \{\n[\s\S]*?^\}\n'
    replacement = '#[tokio::test]\nasync fn ' + name + '() {\n    assert_sub2_separate_observation_scopes(' + str(limited).lower() + ').await;\n}\n'
    text, count = re.subn(pattern, replacement, text)
    assert count <= 1, name
    if count:
        changed.append(name)
assert changed, 'No mixed-scope regression case was found; refuse an unreviewed edit'
assert 'async fn assert_sub2_separate_observation_scopes' not in text
text += r'''

/// Management observations and Key prices are requested independently. The
/// different wallet values deliberately make accidental parent reuse visible.
async fn assert_sub2_separate_observation_scopes(limited: bool) {
    let mut routes = HashMap::new();
    routes.insert("/api/v1/user/profile".into(), Route::ok(
        json!({"code": 0, "data": {"id": 8, "balance": 100.0}}).to_string(),
    ));
    routes.insert("/api/v1/subscriptions/summary".into(), Route::ok(
        json!({"code": 0, "data": {"subscriptions": [{
            "id": 9, "group_id": 4, "group_name": "composite",
            "daily_used_usd": 7.0, "daily_limit_usd": 20.0,
            "expires_at": "2026-05-01T00:00:00Z"
        }]}}).to_string(),
    ));
    routes.insert("/api/v1/groups/available".into(), Route::ok(
        json!({"code": 0, "data": [{"id": 4, "name": "composite", "platform": "composite"}]}).to_string(),
    ));
    routes.insert("/v1/models".into(), Route::ok(
        json!({"data": [
            {"id": "gpt-a", "platform": "openai"},
            {"id": "claude-b", "platform": "anthropic"}
        ]}).to_string(),
    ));
    let usage = if limited {
        json!({"mode": "quota_limited", "quota": {"used": 2.0, "limit": 10.0, "remaining": 8.0}})
    } else {
        json!({"mode": "unrestricted", "balance": 20.0})
    };
    routes.insert("/v1/usage".into(), Route::ok(usage.to_string()));
    routes.insert("/v1/sub2api/billing".into(), Route::ok(json!({
        "object": "sub2api.key_billing", "schema_version": 1,
        "billing_scope": "token", "effective_rate_multiplier": 2.0,
        "peak_rate_enabled": false
    }).to_string()));
    routes.insert("/api/v1/model-plaza".into(), Route::ok(json!({
        "code": 0, "data": {"groups": [{
            "id": 4, "name": "composite", "platform": "composite",
            "rate_multiplier": 9.0, "peak_rate_enabled": false,
            "models": [
                {"name": "gpt-a",
                 "pricing": {"billing_mode": "token", "input_price": 0.000003,
                    "output_price": 0.000006, "cache_read_price": 0.000001,
                    "cache_write_price": 0.000004, "intervals": []},
                 "official_pricing": {"input_price": 0.000002, "output_price": 0.000004}},
                {"name": "unpermitted",
                 "pricing": {"billing_mode": "token", "input_price": 1.0, "output_price": 1.0},
                 "official_pricing": {"input_price": 1.0, "output_price": 1.0}}
            ]
        }]}
    }).to_string()));
    let (base, client, hits) = spawn_mock(routes).await;
    let group = group_with(Some("4"), &[]);
    let parent = read(&client, &PlatformReadRequest {
        kind: PlatformKind::Sub2api, base_url: &base, user_credential: Some(USER),
        key: None, group: &group, now: now(),
    }).await;
    assert!(parent.errors.is_empty(), "{:?}", parent.errors);
    assert!(!parent.stale);
    let wallet = parent.quotas.iter().find(|q| matches!(q.kind, PlatformQuotaKind::Wallet)).expect("parent wallet");
    assert_eq!(wallet.remaining, Some(100.0));
    assert_eq!(wallet.source, "sub2api.user.profile");
    assert!(wallet.used.is_none());
    let daily = parent.quotas.iter().find(|q| matches!(q.kind, PlatformQuotaKind::Subscription)).expect("parent subscription");
    assert_eq!(daily.used, Some(7.0));
    assert_eq!(daily.limit, Some(20.0));
    assert_eq!(daily.remaining, Some(13.0));
    assert_eq!(daily.unit, "usd");
    assert!(daily.expires_at.is_some());
    assert!(parent.quotas.iter().all(|q| !matches!(q.kind, PlatformQuotaKind::KeyLimit)));
    assert!(parent.quotas.iter().all(|q| !matches!(q.period.as_deref(), Some("weekly" | "monthly"))));
    assert!(parent.groups.iter().any(|g| g.id.as_deref() == Some("4") && g.platform.as_deref() == Some("composite")));
    assert!(parent.models.is_empty());
    assert!(parent.prices.is_empty());
    {
        let mut calls = hits.lock().unwrap();
        assert!(calls.iter().all(|call| !call.path.starts_with("/v1/")));
        assert!(calls.iter().all(|call| call.authorization.as_deref() == Some(format!("Bearer {USER}").as_str())));
        calls.clear();
    }
    let child = read(&client, &PlatformReadRequest {
        kind: PlatformKind::Sub2api, base_url: &base, user_credential: Some(USER),
        key: Some(KEY), group: &group, now: now(),
    }).await;
    assert!(child.errors.is_empty(), "{:?}", child.errors);
    assert!(!child.stale);
    assert!(child.quotas.iter().all(|q| q.source == "sub2api.v1.usage"));
    assert!(!child.quotas.iter().any(|q| matches!(q.kind, PlatformQuotaKind::Subscription)));
    if limited {
        let quota = child.quotas.iter().find(|q| matches!(q.kind, PlatformQuotaKind::KeyLimit)).unwrap();
        assert_eq!(quota.used, Some(2.0));
        assert_eq!(quota.limit, Some(10.0));
        assert_eq!(quota.remaining, Some(8.0));
        assert!(!child.quotas.iter().any(|q| matches!(q.kind, PlatformQuotaKind::Wallet)));
    } else {
        let wallets: Vec<_> = child.quotas.iter().filter(|q| matches!(q.kind, PlatformQuotaKind::Wallet)).collect();
        assert_eq!(wallets.len(), 1);
        assert_eq!(wallets[0].remaining, Some(20.0));
        assert!(wallets[0].used.is_none());
    }
    assert!(child.models.iter().any(|m| m.platform.as_deref() == Some("openai")));
    assert!(child.models.iter().any(|m| m.platform.as_deref() == Some("anthropic")));
    assert!(!child.models.iter().any(|m| m.id == "unpermitted"));
    assert!(child.prices.iter().all(|p| p.model != "unpermitted"));
    let billed = child.prices.iter().find(|p| p.model == "gpt-a" && !p.official_reference).expect("billed price");
    assert_eq!(billed.unavailable_reason, None);
    assert_eq!(billed.input, Some(0.000003 * 2.0));
    assert_eq!(billed.output, Some(0.000006 * 2.0));
    assert_eq!(billed.cache_read, Some(0.000001 * 2.0));
    assert_eq!(billed.cache_write, Some(0.000004 * 2.0));
    let official = child.prices.iter().find(|p| p.model == "gpt-a" && p.official_reference).expect("official price");
    assert_eq!(official.input, Some(0.000002));
    assert_eq!(official.output, Some(0.000004));
    assert_eq!(official.valid_until, now() + SNAPSHOT_TTL_SECS);
    let calls = hits.lock().unwrap();
    assert_eq!(calls.len(), 4);
    for call in calls.iter() {
        if call.path == "/api/v1/model-plaza" {
            assert_eq!(call.authorization.as_deref(), Some(format!("Bearer {USER}").as_str()));
        } else {
            assert!(matches!(call.path.as_str(), "/v1/models" | "/v1/usage" | "/v1/sub2api/billing"));
            assert_eq!(call.authorization.as_deref(), Some(format!("Bearer {KEY}").as_str()));
        }
    }
}
'''
path.write_text(text)
print('Updated explicit parent/Key regression cases:', ', '.join(changed))
