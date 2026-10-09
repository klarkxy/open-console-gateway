use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account, AccountSetupStep, AccountType, ProxyMode, RoutingMode};
use crate::provider::{CredentialKind, ProviderOrigin, QuotaScope, UpstreamProtocolKind};
use crate::state::CoreStateInner;
use axum::body::Bytes;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use serde_json::json;
use std::path::PathBuf;

#[test]
fn platform_summary_selects_saved_native_amounts_without_inventing_zero() {
    use crate::platform::{PlatformQuota, PlatformQuotaKind, PlatformSnapshot};
    let quota =
        |kind, period: Option<&str>, unit: &str, remaining, used, unlimited| PlatformQuota {
            kind,
            scope_id: " Key name ".into(),
            unit: unit.into(),
            used,
            remaining,
            limit: None,
            unlimited,
            period: period.map(str::to_string),
            resets_at: None,
            expires_at: None,
            source: "official".into(),
        };
    let snapshot = PlatformSnapshot {
        observed_at: 100,
        quotas: vec![
            quota(
                PlatformQuotaKind::Wallet,
                Some("month"),
                "USD",
                None,
                Some(8.0),
                false,
            ),
            quota(
                PlatformQuotaKind::Wallet,
                None,
                "CNY",
                Some(0.0),
                Some(10.0),
                false,
            ),
            quota(
                PlatformQuotaKind::KeyLimit,
                None,
                "tokens",
                None,
                None,
                false,
            ),
        ],
        ..Default::default()
    };
    let projected = PlatformSnapshotSummary::from(&snapshot);
    let wallet = projected.wallet.unwrap();
    assert_eq!(wallet.remaining, Some(0.0));
    assert_eq!(wallet.history_used, Some(10.0));
    assert_eq!(
        wallet.month_used, None,
        "different native units cannot share one amount label"
    );
    assert_eq!(wallet.unit, "CNY");
    assert!(projected.key_remaining.is_none());
    assert_eq!(projected.key_name.as_deref(), Some("Key name"));
    let mut unlimited = snapshot;
    unlimited.quotas[1].unlimited = true;
    let wallet = PlatformSnapshotSummary::from(&unlimited).wallet.unwrap();
    assert!(wallet.remaining_unlimited);
    assert!(wallet.remaining.is_none() && wallet.history_used.is_none());
    assert!(
        PlatformSnapshotSummary::from(&PlatformSnapshot::default())
            .wallet
            .is_none()
    );
}

#[test]
fn provider_editor_uses_the_same_saved_protocol_projection_as_the_page() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let d = s
        .destinations
        .iter()
        .find(|d| d.legacy.id == "supplier-a")
        .unwrap();
    let key = format!("d:{}", d.id);
    let edit = providers::edit_detail(&state, &s, &key).unwrap();
    let page = providers::models(&state, &s, &key, &PageQuery::default()).unwrap();
    let scope = edit.scope.unwrap();
    assert_eq!(scope.summary.scope_id, d.id);
    assert_eq!(scope.summary.revision, edit.revision.revision);
    let presentation = d.presentation.as_ref().unwrap();
    assert_eq!(presentation.total, page.total);
    assert_eq!(presentation.all_disabled, page.all_disabled);
    let mut page_wire = serde_json::to_value(&page.models).unwrap();
    for row in page_wire.as_array_mut().unwrap() {
        let row = row.as_object_mut().unwrap();
        row.remove("metadata");
        row.remove("metadataSource");
    }
    assert_eq!(
        serde_json::to_value(&presentation.models).unwrap(),
        page_wire
    );
    assert_eq!(
        serde_json::to_value(scope.models).unwrap(),
        serde_json::to_value(page.models).unwrap()
    );
}
struct Fixture {
    state: Option<CoreState>,
    dir: PathBuf,
}

impl Fixture {
    fn state(&self) -> CoreState {
        self.state.as_ref().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.take();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn fixture() -> Fixture {
    fixture_at(
        "https://a.example.test/v1",
        "https://b.example.test/v1",
        None,
    )
}
fn fixture_at(a_url: &str, b_url: &str, preset: Option<&str>) -> Fixture {
    let dir = std::env::temp_dir().join(format!("ocg-management-pages-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("card-test"));
    let now = Utc::now();
    for (id, url) in [("supplier-a", a_url), ("supplier-b", b_url)] {
        db.create_dynamic_provider_definition(&DynamicProviderRuntime {
            preset_id: preset.map(str::to_string),
            id: id.into(),
            name: id.into(),
            endpoint_url: url.into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            auth_kind: DynamicAuthKind::Bearer,
            mappings: vec![DynamicModelMapping {
                public_model: "card-test".into(),
                upstream_model: format!("{id}-upstream"),
                upstream_override: None,
            }],
            created_at: now,
            updated_at: now,
            origin: ProviderOrigin::Custom,
            offering: "api".into(),
        })
        .unwrap();
    }
    for (id, provider) in [
        ("a1", "supplier-a"),
        ("a2", "supplier-a"),
        ("b1", "supplier-b"),
    ] {
        db.create_account(&Account {
            id: id.into(),
            provider_id: provider.into(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: cipher.encrypt(&format!("dummy-{id}")).unwrap(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: Some(format!("preserve-{id}")),
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    }
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = "dummy-gateway-key".into();
    config.proxy_mode = ProxyMode::Direct;
    config.routing_mode = RoutingMode::StrictPriority;
    config.conversation_sticky = false;
    state.set_config(config).unwrap();
    Fixture {
        state: Some(state),
        dir,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_coalesces_bound_work_obeys_global_limit_and_automatic_freshness() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = fixture_at(
        "https://api.deepseek.com/chat/completions",
        "https://api.deepseek.com/chat/completions",
        Some("deepseek"),
    );
    let state = f.state();
    for n in 0..5 {
        let mut a = state.db.lock().get_account("a1").unwrap().unwrap();
        a.id = format!("balance-{n}");
        state.db.lock().create_account(&a).unwrap();
    }
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_active = active.clone();
    let handler_max = maximum.clone();
    let handler_calls = calls.clone();
    let router=axum::Router::new().route("/balance",axum::routing::get(move||{let active=handler_active.clone();let maximum=handler_max.clone();let calls=handler_calls.clone();async move{
        calls.fetch_add(1,Ordering::SeqCst);let current=active.fetch_add(1,Ordering::SeqCst)+1;maximum.fetch_max(current,Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;active.fetch_sub(1,Ordering::SeqCst);
        Json(json!({"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"9","granted_balance":"0","topped_up_balance":"9"}]}))
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let _guard = crate::official_api::install_official_api_endpoint_for_test(
        state.process_generation(),
        crate::official_api::BALANCE_URL,
        &format!("http://{addr}/balance"),
    )
    .unwrap();
    let expectation = ControlRevision::from_state(&state);
    let automatic_body=json!({"expectedRevision":expectation.revision,"processGeneration":expectation.process_generation,"mode":"automatic"}).to_string();
    let ids = [
        "a1",
        "a1",
        "a2",
        "b1",
        "balance-0",
        "balance-1",
        "balance-2",
        "balance-3",
        "balance-4",
    ];
    let jobs = ids
        .into_iter()
        .map(|id| {
            let state = state.clone();
            let body = automatic_body.clone();
            tokio::spawn(async move {
                refresh_account(State(state), Path(id.into()), Bytes::from(body)).await
            })
        })
        .collect::<Vec<_>>();
    for job in jobs {
        assert_eq!(job.await.unwrap().unwrap().0.outcome, "refreshed");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    assert!(maximum.load(Ordering::SeqCst) <= 4);
    let fresh = refresh_account(
        State(state.clone()),
        Path("a1".into()),
        Bytes::from(automatic_body),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(fresh.outcome, "fresh");
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    let manual=refresh_account(State(state),Path("a1".into()),Bytes::from(json!({"expectedRevision":expectation.revision,"processGeneration":expectation.process_generation,"mode":"manual"}).to_string())).await.unwrap().0;
    assert_eq!(manual.outcome, "throttled");
    server.abort();
}

#[test]
fn automatic_throttle_is_bounded_and_binding_changes_get_a_new_attempt() {
    let f = fixture();
    let state = f.state();
    let mut cache = AutomaticRefreshCache::default();
    let now = Utc::now();
    let before = refresh::binding_key_for_test(&state, "a1");
    assert!(cache.admit(before.clone(), now));
    assert!(!cache.admit(before.clone(), now + Duration::minutes(4)));
    let rotated = state.encrypt_key("dummy-rotated").unwrap();
    state.db.lock().conn.execute("UPDATE credentials SET key_cipher=?1,credential_version=credential_version+1 WHERE legacy_account_id='a1'",[rotated]).unwrap();
    let after = refresh::binding_key_for_test(&state, "a1");
    assert_ne!(before, after);
    assert!(cache.admit(after, now));
    assert!(cache.admit(before, now + Duration::minutes(5)));
    for n in 0..300 {
        assert!(cache.admit(format!("account-{n}"), now + Duration::minutes(5)));
    }
    assert!(cache.admit("replacement".into(), now + Duration::minutes(6)));
}

#[test]
fn accounts_are_bounded_and_search_is_global_case_insensitive() {
    let f = fixture();
    let state = f.state();
    for n in 0..110 {
        let mut a = state.db.lock().get_account("a1").unwrap().unwrap();
        a.id = format!("extra-{n}");
        a.name = format!("Credential {n}");
        state.db.lock().create_account(&a).unwrap();
    }
    let s = snapshot(&state).unwrap();
    let page = accounts::project(
        &s,
        &PageQuery {
            limit: Some(7),
            ..Default::default()
        },
    );
    assert!(page.cards.len() <= 7);
    assert!(page.cards.iter().map(|c| c.rows.len()).sum::<usize>() <= 7);
    assert!(page.has_more);
    let selected = accounts::project(
        &s,
        &PageQuery {
            search: Some("CREDENTIAL 109".into()),
            limit: Some(2),
            ..Default::default()
        },
    );
    assert_eq!(selected.matched_credentials, 1);
    assert_eq!(
        selected.cards[0].rows[0].credential.legacy_account_id,
        "extra-109"
    );
    assert!(selected.total_credentials > 110);
    let encoded = serde_json::to_value(&page).unwrap();
    let row = &encoded["cards"][0]["rows"][0];
    assert!(row["account"].get("modelCapabilities").is_none());
    assert!(row["account"].get("customConfig").is_none());
    assert!(encoded["cards"][0]["destination"].get("catalog").is_none());
    assert!(!serde_json::to_string(&page).unwrap().contains("dummy-a1"));
}

#[test]
fn available_and_auth_error_filters_match_full_membership_eligibility() {
    let f = fixture();
    let state = f.state();
    state.db.lock().conn.execute("UPDATE credentials SET auth_error='fixture failure',auth_state='invalid' WHERE legacy_account_id='a1'",[]).unwrap();
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET binding_enabled=0 WHERE legacy_account_id='b1'",
            [],
        )
        .unwrap();
    let s = snapshot(&state).unwrap();
    let errors = accounts::project(
        &s,
        &PageQuery {
            status: Some("auth-error".into()),
            ..Default::default()
        },
    );
    assert_eq!(errors.matched_credentials, 1);
    assert_eq!(errors.cards[0].rows[0].credential.legacy_account_id, "a1");
    let available = accounts::project(
        &s,
        &PageQuery {
            status: Some("available".into()),
            ..Default::default()
        },
    );
    let ids = available
        .cards
        .iter()
        .flat_map(|c| {
            c.rows
                .iter()
                .map(|r| r.credential.legacy_account_id.as_str())
        })
        .collect::<Vec<_>>();
    assert!(ids.contains(&"a2"));
    assert!(!ids.contains(&"a1"));
    assert!(!ids.contains(&"b1"));
    let all = accounts::project(&s, &PageQuery::default());
    assert_eq!(
        all.cards
            .iter()
            .find(|c| c
                .rows
                .iter()
                .any(|r| r.credential.legacy_account_id == "b1"))
            .unwrap()
            .availability,
        "no_available_keys"
    );
}

#[tokio::test]
async fn failed_platform_observation_returns_partial_fixed_codes_and_records_partial_receipt() {
    let f = fixture();
    let state = f.state();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(axum::routing::any(|| async {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            "reflected-dummy-linked-key",
        )
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut account = state.db.lock().get_account("a1").unwrap().unwrap();
    account.id = "linked-key".into();
    account.provider_id = ocg_domain::ids::CUSTOM_PROVIDER_ID.into();
    account.name = "Linked Key".into();
    let endpoint = format!("http://{address}/v1/chat/completions");
    state
        .db
        .lock()
        .create_account_with_contract(
            &account,
            Some(&crate::models::AccountCustomConfigInput {
                endpoint_url: endpoint,
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[crate::models::AccountModelCapabilityInput {
                public_model: "linked-model".into(),
                upstream_model: "linked-model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    state
        .db
        .lock()
        .create_platform_account(
            "platform",
            crate::platform::PlatformKind::NewApi,
            "Site",
            &format!("http://{address}"),
            None,
        )
        .unwrap();
    state
        .db
        .lock()
        .link_platform_account(
            "linked-key",
            "platform",
            &crate::platform::PlatformGroup::default(),
        )
        .unwrap();
    let revision = state.settings_revision();
    let response=refresh_account(State(state.clone()),Path("linked-key".into()),Bytes::from(json!({"expectedRevision":revision,"processGeneration":state.process_generation(),"mode":"manual"}).to_string())).await.unwrap().0;
    assert_eq!(response.outcome, "partial");
    assert!(!response.errors.is_empty());
    assert!(response.errors.iter().all(|e| e.resource == "platform"
        && e.id.as_deref() == Some("linked-key")
        && !e.code.contains("reflected")));
    assert!(
        !serde_json::to_string(&response)
            .unwrap()
            .contains("dummy-a1")
    );
    assert_eq!(response.revision.revision, revision);
    let observation = state
        .db
        .lock()
        .list_platform_links()
        .unwrap()
        .into_iter()
        .find(|l| l.account_id == "linked-key")
        .unwrap()
        .snapshot
        .unwrap();
    assert!(observation.stale);
    assert!(!observation.errors.is_empty());
    assert!(response.refresh.fresh_until.is_none());
    let outcome:String=state.db.lock().conn.query_row("SELECT outcome FROM operation_logs WHERE action='platform.refresh' ORDER BY rowid DESC LIMIT 1",[],|row|row.get(0)).unwrap();
    assert_eq!(outcome, "partial");
    server.abort();
}

#[test]
fn repeated_destination_cards_and_empty_cards_keep_saved_identity() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let a1 = s
        .credentials
        .iter()
        .find(|c| c.legacy_account_id == "a1")
        .unwrap();
    let a2 = s
        .credentials
        .iter()
        .find(|c| c.legacy_account_id == "a2")
        .unwrap();
    let b = s
        .credentials
        .iter()
        .find(|c| c.legacy_account_id == "b1")
        .unwrap();
    let cards = vec![
        RoutingCard {
            id: "first".into(),
            destination_id: a1.destination_id.clone(),
            credential_ids: vec![a1.id.clone()],
        },
        RoutingCard {
            id: "second".into(),
            destination_id: a2.destination_id.clone(),
            credential_ids: vec![a2.id.clone()],
        },
        RoutingCard {
            id: "empty".into(),
            destination_id: a2.destination_id.clone(),
            credential_ids: vec![],
        },
        RoutingCard {
            id: "third".into(),
            destination_id: b.destination_id.clone(),
            credential_ids: vec![b.id.clone()],
        },
    ];
    state
        .db
        .lock()
        .conn
        .execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES ('routing_cards_v1',?1)",
            [json!({"version":1,"cards":cards}).to_string()],
        )
        .unwrap();
    let s = snapshot(&state).unwrap();
    let page = accounts::project(&s, &PageQuery::default());
    let ids = page
        .cards
        .iter()
        .map(|c| c.card_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.into_iter()
            .filter(|id| ["first", "second", "empty", "third"].contains(id))
            .collect::<Vec<_>>(),
        vec!["first", "second", "empty", "third"]
    );
    assert_eq!(
        page.cards
            .iter()
            .find(|c| c.card_id == "empty")
            .unwrap()
            .total_credentials,
        0
    );
    let second = accounts::credentials(&state, &s, "second", &PageQuery::default()).unwrap();
    assert_eq!(second.rows[0].credential.legacy_account_id, "a2");
}

#[test]
fn models_global_search_deep_links_and_selected_edits_preserve_complete_scope() {
    let f = fixture();
    let state = f.state();
    let mut definition = state
        .db
        .lock()
        .get_dynamic_provider("supplier-a")
        .unwrap()
        .unwrap();
    definition.mappings = (0..130)
        .map(|n| DynamicModelMapping {
            public_model: format!("Model {n}"),
            upstream_model: format!("up-{n}"),
            upstream_override: None,
        })
        .collect();
    state
        .db
        .lock()
        .replace_dynamic_provider(&definition, false, false, None)
        .unwrap();
    let s = snapshot(&state).unwrap();
    let detail = providers::detail(&state, &s, "p:supplier-a").unwrap();
    assert_eq!(detail.destination.as_ref().unwrap().catalog_count, 130);
    assert_eq!(detail.scope.as_ref().unwrap().account_count, 2);
    let wire = serde_json::to_value(&detail).unwrap();
    assert!(wire["scope"].get("accounts").is_none());
    assert!(wire["scope"].get("models").is_none());
    assert!(wire["catalogEntry"].get("modelAliases").is_none());
    let page = providers::models(
        &state,
        &s,
        &detail.item.rail_key,
        &PageQuery {
            limit: Some(10),
            model: Some("Model 129".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page.total, 130);
    assert_eq!(page.models.len(), 10);
    assert!(page.models.iter().any(|m| m.public_model == "Model 129"));
    assert_eq!(page.offset, 120);
    let search = providers::models(
        &state,
        &s,
        &detail.item.rail_key,
        &PageQuery {
            search: Some("UP-129".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(search.filtered_total, 1);
    let edit = providers::edit_detail(&state, &s, &detail.item.rail_key).unwrap();
    assert_eq!(edit.definition.unwrap().models.len(), 130);
    assert_eq!(edit.credentials.len(), 2);
    assert_eq!(edit.accounts.len(), 2);
    assert!(edit.accounts.iter().all(|a| a.provider_id == "supplier-a"));
}

#[test]
fn aliases_bound_mapping_rows_with_global_group_counts_and_ranks() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let page = aliases::project(
        &s,
        &PageQuery {
            limit: Some(1),
            ..Default::default()
        },
    );
    let group = page
        .groups
        .iter()
        .find(|g| g.public_model == "card-test")
        .unwrap();
    assert_eq!(group.total_rows, 2);
    assert_eq!(group.matching_rows, 2);
    assert_eq!(group.rows.len(), 1);
    assert!(group.continued);
    assert!(page.has_more);
    let next = aliases::project(
        &s,
        &PageQuery {
            offset: Some(1),
            limit: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(next.groups[0].public_model, "card-test");
    assert!(next.groups[0].continued);
    let row = &group.rows[0];
    let d = s
        .destinations
        .iter()
        .find(|d| Some(&d.id) == row.destination_id.as_ref())
        .unwrap();
    let expected = s
        .credentials
        .iter()
        .filter(|c| c.destination_id == d.id && c.enabled)
        .map(|c| c.routing_rank)
        .collect::<Vec<_>>();
    assert_eq!(row.routing_ranks, expected);
    let filtered = aliases::project(
        &s,
        &PageQuery {
            search: Some("SUPPLIER-B-UPSTREAM".into()),
            ..Default::default()
        },
    );
    assert_eq!(filtered.filtered_rows, 1);
    assert_eq!(filtered.groups[0].total_rows, 2);
}

#[test]
fn alias_rows_sort_by_complete_serving_rank_before_pagination() {
    let f = fixture();
    let state = f.state();
    state.db.lock().conn.execute("UPDATE credentials SET routing_rank=CASE legacy_account_id WHEN 'b1' THEN 0 WHEN 'a1' THEN 8 WHEN 'a2' THEN 9 ELSE routing_rank END",[]).unwrap();
    let s = snapshot(&state).unwrap();
    let first = aliases::project(
        &s,
        &PageQuery {
            limit: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(first.groups[0].rows[0].provider_id, "supplier-b");
    assert_eq!(first.groups[0].rows[0].routing_ranks, vec![0]);
}

#[tokio::test]
async fn custom_shared_names_match_published_catalog_without_admitting_raw_pins() {
    use crate::models::{AccountCustomConfigInput, AccountModelCapabilityInput};
    use crate::provider_contracts::ContractScope;
    let f = fixture();
    let state = f.state();
    let now = Utc::now();
    let shared = "shared-fixture";
    let raw = "fixture/raw-fixture";
    state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(crate::provider::COMMAND_CODE_PROVIDER_ID),
            &[shared.into(), raw.into()],
            Some(now),
            "test",
            "https://example.test/models",
            now,
        )
        .unwrap();
    let mut account = state.db.lock().get_account("a1").unwrap().unwrap();
    account.id = "custom-shared".into();
    account.provider_id = ocg_domain::ids::CUSTOM_PROVIDER_ID.into();
    state
        .db
        .lock()
        .create_account_with_contract(
            &account,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://custom.example.test/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[shared, raw].map(|public| AccountModelCapabilityInput {
                public_model: public.into(),
                upstream_model: format!("custom-{public}"),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }),
        )
        .unwrap();
    let published = crate::gateway::handler::published_models_data_locked(&state).unwrap();
    assert!(published.iter().any(|r| r["id"] == shared));
    assert!(!published.iter().any(|r| r["id"] == raw));
    let s = snapshot(&state).unwrap();
    let page = aliases::project(&s, &PageQuery::default());
    let group = page
        .groups
        .iter()
        .find(|g| g.public_model == shared)
        .unwrap();
    assert!(
        group
            .rows
            .iter()
            .any(|r| r.custom_account_id.as_deref() == Some("custom-shared"))
    );
    assert!(!page.groups.iter().any(|g| g.public_model == raw));
}

#[test]
fn alias_inventory_respects_destination_protocols_and_disabled_binding_ranks() {
    let f = fixture();
    let state = f.state();
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET binding_enabled=0 WHERE legacy_account_id='a1'",
            [],
        )
        .unwrap();
    let captured = snapshot(&state).unwrap();
    *state.management_page_cache.lock() = Default::default();
    let mut s = Arc::try_unwrap(captured).ok().unwrap();
    let inventory = aliases::project(&s, &PageQuery::default());
    let a = inventory
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .find(|r| r.provider_id == "supplier-a")
        .unwrap();
    let a2_rank = s
        .credentials
        .iter()
        .find(|c| c.legacy_account_id == "a2")
        .unwrap()
        .routing_rank;
    assert_eq!(a.routing_ranks, vec![a2_rank]);
    let d = s
        .destinations
        .iter_mut()
        .find(|d| d.legacy.id == "supplier-a")
        .unwrap();
    d.protocols = vec![super::super::types::ProtocolDto::ChatCompletions];
    d.protocol_routes.clear();
    d.catalog[0].protocols = vec![super::super::types::ProtocolDto::Responses];
    let incompatible = aliases::project(&s, &PageQuery::default());
    assert!(
        !incompatible
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .any(|r| r.provider_id == "supplier-a")
    );
    assert!(
        incompatible
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .any(|r| r.provider_id == "supplier-b")
    );
}

#[test]
fn account_actions_preserve_key_workflows_and_singleton_exclusions() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let page = accounts::project(&s, &PageQuery::default());
    let row = page
        .cards
        .iter()
        .flat_map(|card| &card.rows)
        .find(|r| r.credential.legacy_account_id == "a1")
        .unwrap();
    let allowed = |key: &str| row.actions.iter().any(|a| a.key == key && a.allowed);
    for key in [
        "toggle",
        "test-connection",
        "rotate-key",
        "edit-binding",
        "add-key",
        "edit",
        "delete",
        "move-to-card",
    ] {
        assert!(allowed(key), "missing {key}");
    }
    assert!(!allowed("open-cpa"));
    assert!(!allowed("import-keys"));
    if let Some(zen) = page.cards.iter().flat_map(|card| &card.rows).find(|r| {
        r.account
            .as_ref()
            .is_some_and(|a| a.provider_id == ocg_domain::ids::OPENCODE_ZEN_FREE_PROVIDER_ID)
    }) {
        assert!(zen.actions.iter().any(|a| a.key == "toggle"));
        assert!(!zen.actions.iter().any(|a| {
            [
                "rotate-key",
                "edit-binding",
                "add-key",
                "delete",
                "edit",
                "open-cpa",
            ]
            .contains(&a.key.as_str())
        }));
    }
    let card = page
        .cards
        .iter()
        .find(|c| {
            c.rows
                .iter()
                .any(|r| r.credential.legacy_account_id == "a1")
        })
        .unwrap();
    assert!(
        card.actions
            .iter()
            .any(|a| a.key == "add-card" && a.allowed)
    );
    assert!(!card.actions.iter().any(|a| a.key == "delete-group"));
}

#[test]
fn managed_cpa_restart_status_uses_persisted_owner_and_invalidates_on_manifest_change() {
    let f = fixture();
    let state = f.state();
    let initial = snapshot(&state).unwrap();
    let mut managed = crate::cpa_runtime::ManagedCpa {
        current_version: "6.1.0".into(),
        previous_version: None,
        asset_sha256: "a".repeat(64),
        port: 8317,
        desired_running: false,
    };
    crate::cpa_runtime::save_managed(&state.data_dir(), &managed).unwrap();
    let runtime = state.cpa_runtime_snapshot();
    assert!(runtime.owned);
    assert!(runtime.installed);
    assert!(!runtime.running);
    assert_eq!(cpa_status(&runtime, true).as_deref(), Some("stopped"));
    let installed = snapshot(&state).unwrap();
    assert_eq!(installed.cpa_status.as_deref(), Some("stopped"));
    assert_ne!(installed.read_version, initial.read_version);
    managed.current_version = "6.1.1".into();
    crate::cpa_runtime::save_managed(&state.data_dir(), &managed).unwrap();
    let updated = snapshot(&state).unwrap();
    assert_ne!(installed.read_version, updated.read_version);
    std::fs::write(
        crate::cpa_runtime::managed_path(&state.data_dir()),
        "{invalid}",
    )
    .unwrap();
    let failed = snapshot(&state).unwrap();
    assert_eq!(failed.cpa_status.as_deref(), Some("failed"));
    assert!(
        failed
            .errors
            .iter()
            .any(|e| e.resource == "cpa" && e.code == "read_failed")
    );
}

#[test]
fn partial_billing_failures_and_legacy_secret_errors_do_not_hide_other_rows() {
    let f = fixture();
    let state = f.state();
    state.db.lock().conn.execute("UPDATE credentials SET credit_meter_json='{',last_error='rejected dummy-a1' WHERE legacy_account_id='a1'",[]).unwrap();
    let s = snapshot(&state).unwrap();
    let page = accounts::project(&s, &PageQuery::default());
    assert!(page.errors.iter().any(|e| e.resource == "billing"
        && e.id.as_deref() == Some("a1")
        && e.code == "read_failed"));
    let rows = page.cards.iter().flat_map(|c| &c.rows).collect::<Vec<_>>();
    assert!(
        rows.iter()
            .any(|r| r.credential.legacy_account_id == "a2" && r.billing.is_some())
    );
    assert!(rows.iter().any(|r| r.credential.legacy_account_id == "a1"
        && r.account.is_some()
        && r.billing.is_none()));
    assert!(!serde_json::to_string(&page).unwrap().contains("dummy-a1"));
}

#[test]
fn page_cache_rebuilds_actual_sources_after_external_database_writes_and_time_changes() {
    let f = fixture();
    let state = f.state();
    let before = snapshot(&state).unwrap();
    let same = snapshot(&state).unwrap();
    assert!(Arc::ptr_eq(&before, &same));
    let external = Database::open(f.dir.clone()).unwrap();
    external
        .conn
        .execute(
            "UPDATE credentials SET name='external rename' WHERE legacy_account_id='a1'",
            [],
        )
        .unwrap();
    let after = snapshot(&state).unwrap();
    assert_ne!(before.read_version, after.read_version);
    assert_eq!(
        after.accounts.iter().find(|a| a.id == "a1").unwrap().name,
        "external rename"
    );
    let version = cache::ReadVersion::capture(&state).unwrap();
    assert!(
        state
            .management_page_cache
            .lock()
            .get(&version, after.as_of - Duration::seconds(1))
            .is_none()
    );
    let renewed = snapshot(&state).unwrap();
    assert!(
        state
            .management_page_cache
            .lock()
            .get(&version, renewed.valid_until)
            .is_none()
    );
}

#[test]
fn sustained_telemetry_appends_do_not_rebuild_or_reject_management_reads() {
    let f = fixture();
    let state = f.state();
    let before = snapshot(&state).unwrap();
    let external = Database::open(f.dir.clone()).unwrap();
    for _ in 0..32 {
        external.conn.execute("INSERT INTO forward_logs(timestamp,model,account_id,account_name,status) VALUES (?1,'telemetry','a1','a1','success')",[Utc::now().to_rfc3339()]).unwrap();
        let after = snapshot(&state).unwrap();
        assert!(Arc::ptr_eq(&before, &after));
        assert_eq!(before.read_version, after.read_version);
    }
}

#[test]
fn card_credentials_carry_the_materialized_snapshot_time_when_cooldowns_expire() {
    let mut f = fixture();
    f.state.take();
    let now = Utc::now();
    let wall = Arc::new(parking_lot::Mutex::new(now));
    let clock_wall = wall.clone();
    let mono = std::time::Instant::now();
    let state = Arc::new(
        CoreStateInner::new_with_test_gateway_clock(
            Database::open(f.dir.clone()).unwrap(),
            f.dir.clone(),
            Arc::new(StaticKeyCipher::new("card-test")),
            move || *clock_wall.lock(),
            move || mono,
        )
        .unwrap(),
    );
    f.state = Some(state.clone());
    let deadline = now + Duration::seconds(4);
    state.db.lock().conn.execute("UPDATE credentials SET cooldown_generic_until=?1 WHERE legacy_account_id IN ('a1','a2')",[deadline.to_rfc3339()]).unwrap();
    let before = snapshot(&state).unwrap();
    let header = accounts::project(&before, &PageQuery::default())
        .cards
        .into_iter()
        .find(|c| {
            c.rows
                .iter()
                .any(|r| r.credential.legacy_account_id == "a1")
        })
        .unwrap();
    assert_eq!(header.availability, "no_available_keys");
    let old =
        accounts::credentials(&state, &before, &header.card_id, &PageQuery::default()).unwrap();
    assert!(old.rows.iter().all(|r| r.status == "cooling"));
    assert_eq!(old.as_of, before.as_of.to_rfc3339());
    assert_eq!(old.valid_until, Some(deadline.to_rfc3339()));
    *wall.lock() = deadline + Duration::seconds(1);
    let after = snapshot(&state).unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(before.read_version, after.read_version);
    let new =
        accounts::credentials(&state, &after, &header.card_id, &PageQuery::default()).unwrap();
    assert_eq!(old.read_version, new.read_version);
    assert_ne!(old.as_of, new.as_of);
    assert_eq!(new.as_of, after.as_of.to_rfc3339());
    assert_eq!(new.valid_until, Some(after.valid_until.to_rfc3339()));
    assert!(
        new.rows
            .iter()
            .all(|r| r.status == "enabled" && r.route_available)
    );
    let updated_header = accounts::project(&after, &PageQuery::default())
        .cards
        .into_iter()
        .find(|c| c.card_id == header.card_id)
        .unwrap();
    assert_eq!(updated_header.availability, "available");
}

#[tokio::test]
async fn refresh_requires_cas_and_unsupported_accounts_never_start_network() {
    let f = fixture();
    let state = f.state();
    let revision = ControlRevision::from_state(&state);
    let body=Bytes::from(json!({"expectedRevision":revision.revision,"processGeneration":revision.process_generation,"mode":"automatic"}).to_string());
    let value = refresh_account(State(state.clone()), Path("a1".into()), body)
        .await
        .unwrap()
        .0;
    assert_eq!(value.outcome, "unavailable");
    let stale=Bytes::from(json!({"expectedRevision":revision.revision+1,"processGeneration":revision.process_generation,"mode":"manual"}).to_string());
    assert!(
        refresh_account(State(state), Path("a1".into()), stale)
            .await
            .is_err()
    );
}
