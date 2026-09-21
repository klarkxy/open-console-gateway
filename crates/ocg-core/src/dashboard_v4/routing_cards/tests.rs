use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account, AccountSetupStep, AccountType, ProxyMode, RoutingMode};
use crate::provider::{CredentialKind, ProviderOrigin, QuotaScope, UpstreamProtocolKind};
use crate::state::CoreStateInner;
use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::Utc;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

fn fixture(a_url: &str, b_url: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("ocg-routing-cards-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("card-test"));
    let now = Utc::now();
    for (id, url) in [("supplier-a", a_url), ("supplier-b", b_url)] {
        db.create_dynamic_provider_definition(&DynamicProviderRuntime {
            preset_id: None,
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

fn card_for(snapshot: &RoutingCardList, account: &str) -> routing_cards::RoutingCard {
    let credential = snapshot
        .credentials
        .iter()
        .find(|row| row.legacy_account_id == account)
        .unwrap();
    routing_cards::RoutingCard {
        id: uuid::Uuid::new_v4().to_string(),
        destination_id: credential.destination_id.clone(),
        credential_ids: vec![credential.id.clone()],
    }
}

fn interleaved(snapshot: &RoutingCardList) -> Vec<routing_cards::RoutingCard> {
    let mut cards: Vec<_> = ["a1", "b1", "a2"]
        .iter()
        .map(|id| card_for(snapshot, id))
        .collect();
    let selected: std::collections::HashSet<_> = cards
        .iter()
        .flat_map(|card| card.credential_ids.iter().cloned())
        .collect();
    cards.extend(
        snapshot
            .cards
            .iter()
            .filter(|card| !card.credential_ids.iter().any(|id| selected.contains(id)))
            .cloned(),
    );
    cards
}

async fn put(
    state: &CoreState,
    cards: &[routing_cards::RoutingCard],
) -> Result<RoutingCardList, DestinationsError> {
    replace(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation(),
                "cards": cards,
            }))
            .unwrap(),
        ),
    )
    .await
    .map(|Json(value)| value)
}

#[tokio::test]
async fn card_save_preserves_credentials_adjacent_cards_empty_cards_and_restart() {
    let mut f = fixture("https://a.invalid/v1", "https://b.invalid/v1");
    let state = f.state();
    let before = snapshot(&state).unwrap();
    let mut cards = interleaved(&before);
    let mut empty = cards[0].clone();
    empty.id = uuid::Uuid::new_v4().to_string();
    empty.credential_ids.clear();
    cards.insert(1, empty);
    let saved = put(&state, &cards).await.unwrap();
    assert_eq!(saved.cards, cards);
    for original in &before.credentials {
        let mut after = saved
            .credentials
            .iter()
            .find(|row| row.id == original.id)
            .unwrap()
            .clone();
        after.routing_rank = original.routing_rank;
        assert_eq!(
            &after, original,
            "moving a Key must only change its routing rank"
        );
    }
    // Make the two A cards adjacent. Their individual identities must survive.
    let a2 = cards.remove(3);
    cards.insert(1, a2);
    assert_eq!(put(&state, &cards).await.unwrap().cards, cards);
    let revision = state.settings_revision();
    assert_eq!(list(State(state.clone())).await.unwrap().0.cards, cards);
    assert_eq!(state.settings_revision(), revision, "GET is read-only");
    drop(state);
    f.state.take();
    let db = Database::open(f.dir.clone()).unwrap();
    assert_eq!(routing_cards::load_on(&db.conn).unwrap(), cards);
}

#[tokio::test]
async fn invalid_layout_and_stale_cas_leave_both_rank_and_layout_unchanged() {
    let f = fixture("https://a.invalid/v1", "https://b.invalid/v1");
    let state = f.state();
    let cards = interleaved(&snapshot(&state).unwrap());
    let old_revision = state.settings_revision();
    put(&state, &cards).await.unwrap();
    let before = snapshot(&state).unwrap();
    let mut invalid = cards.clone();
    invalid[0].destination_id = cards[1].destination_id.clone();
    assert_eq!(
        put(&state, &invalid)
            .await
            .unwrap_err()
            .into_response()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).unwrap(), before);
    invalid = cards.clone();
    invalid[0].credential_ids.clear();
    assert_eq!(
        put(&state, &invalid)
            .await
            .unwrap_err()
            .into_response()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).unwrap(), before);
    let stale = replace(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": old_revision,
                "processGeneration": state.process_generation(),
                "cards": cards,
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.into_response().status(), StatusCode::CONFLICT);
    assert_eq!(snapshot(&state).unwrap(), before);
}

#[tokio::test]
async fn regrouping_cards_preserves_existing_conversation_bindings() {
    let f = fixture("https://a.invalid/v1", "https://b.invalid/v1");
    let state = f.state();
    let accounts = state.db.lock().list_accounts().unwrap();
    let selected = state
        .routing
        .select_account(
            &accounts,
            RoutingMode::StrictPriority,
            true,
            Some("existing-conversation"),
            &[],
        )
        .unwrap();
    let binding = state
        .routing
        .sticky_binding("existing-conversation")
        .unwrap();
    assert_eq!(binding.0, selected.id);
    let mut cards = interleaved(&snapshot(&state).unwrap());
    cards.swap(0, 2);
    put(&state, &cards).await.unwrap();
    assert_eq!(
        state.routing.sticky_binding("existing-conversation"),
        Some(binding)
    );
}

#[tokio::test]
async fn committed_card_order_drives_real_a1_b1_a2_http_attempts() {
    let journal = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut upstreams = Vec::new();
    let mut urls = Vec::new();
    for _ in 0..2 {
        let journal = journal.clone();
        let app = Router::new().route("/v1/chat/completions", post(move |headers: HeaderMap| {
            let journal = journal.clone();
            async move {
                let key = headers.get("authorization").unwrap().to_str().unwrap().to_string();
                journal.lock().unwrap().push(key.clone());
                if key.ends_with("a2") {
                    (StatusCode::OK, Json(json!({"id":"ok","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})))
                } else {
                    (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":{"type":"rate_limit_error","message":"test limit"}})))
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        urls.push(format!("http://{}/v1", listener.local_addr().unwrap()));
        upstreams.push(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
    }
    let f = fixture(&urls[0], &urls[1]);
    let state = f.state();
    let gateway = crate::gateway::start_gateway_on(state.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let base = format!("http://127.0.0.1:{}", gateway.port);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let before = snapshot(&state).unwrap();
    let cards = interleaved(&before);
    let response = client.put(format!("{base}/dashboard/api/v4/routing/cards"))
        .json(&json!({"expectedRevision":state.settings_revision(),"processGeneration":state.process_generation(),"cards":cards}))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let receipt = response.text().await.unwrap();
    assert!(!receipt.contains("dummy-a1"));
    assert!(!receipt.contains("keyCipher"));
    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth("dummy-gateway-key")
        .json(&json!({"model":"card-test","messages":[{"role":"user","content":"test"}]}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        *journal.lock().unwrap(),
        ["Bearer dummy-a1", "Bearer dummy-b1", "Bearer dummy-a2"]
    );
    state.set_dashboard_local_mode(false);
    assert_eq!(
        client
            .get(format!("{base}/dashboard/api/v4/routing/cards"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    crate::gateway::stop_gateway_and_wait(gateway).await;
    for upstream in upstreams {
        upstream.abort();
    }
}

#[tokio::test]
async fn invalid_destination_blocks_card_read_and_write_without_changing_ranks() {
    let f = fixture("https://a.invalid/v1", "https://b.invalid/v1");
    let state = f.state();
    let before = snapshot(&state).unwrap();
    let cards = interleaved(&before);
    let revision = state.settings_revision();
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE destinations SET base_url = '', legacy_kind = 'custom_account' WHERE id = ?1",
            [&cards[0].destination_id],
        )
        .unwrap();
    let original_layout = routing_cards::load_on(&state.db.lock().conn).unwrap();
    assert_eq!(
        list(State(state.clone()))
            .await
            .unwrap_err()
            .into_response()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        put(&state, &cards)
            .await
            .unwrap_err()
            .into_response()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(
        routing_cards::load_on(&state.db.lock().conn).unwrap(),
        original_layout
    );
}
