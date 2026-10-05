//! Synthetic SQLite fixtures. Ciphertext is produced in the test process.

use super::{
    ProjectionInput, ProjectionRevisions, RuntimeEnvelope, project, render_canonical_yaml,
    render_standard_yaml,
};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
    AppConfig, ProxyListDirection, ProxyMode, RoutingMode,
};
use crate::provider::{
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, builtin_provider,
};
use crate::routing_snapshot::RoutingSnapshot;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{
    ConnectionId, EndpointOperation, endpoint_id_for, endpoint_id_for_route,
};
use ocg_domain::credential::ModelScope;
use ocg_domain::destination::{
    AuthScheme, CatalogModel, HttpProtocolRoute, destination_id_for_builtin,
    destination_id_for_custom_account,
};
use ocg_domain::dynamic::DynamicModelUpstreamOverride;
use ocg_domain::ids::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    CPA_ACCOUNT_ID, OLLAMA_CLOUD_BASE_URL, OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH, ZEN_FREE_ACCOUNT_ID,
};
use ocg_domain::provider::{
    COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH,
    COMMAND_CODE_GOAT_MESSAGES_PATH, COMMAND_CODE_GOAT_RESPONSES_PATH, KIMI_CN_BASE_URL,
    KIMI_CN_CHAT_COMPLETIONS_PATH, KIMI_CN_MESSAGES_PATH, MINIMAX_CN_ANTHROPIC_BASE_URL,
    MINIMAX_CN_BASE_URL, MINIMAX_CN_CHAT_COMPLETIONS_PATH, MINIMAX_CN_MESSAGES_PATH,
    MINIMAX_CN_RESPONSES_PATH, OPENCODE_GO_BASE_URL, OPENCODE_ZEN_BASE_URL,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

const CIPHER_SECRET: &str = "cpa-projection-test-cipher";
const GATEWAY_SENTINEL: &str = "synthetic-gateway-client-key";
const OWNED_LISTENER: &str = "http://127.0.0.1:8317";
const OAUTH_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HOP_SECRET: &str = "synthetic-private-hop";
const READY_KEY: &str = "synthetic-ready-key";
const POLICY_TOKEN: &str = "synthetic-policy-token";
const POLICY_URL: &str = "http://127.0.0.1:9042/_internal/ocg/cpa-policy";
const POLICY_ORIGIN: &str = "http://127.0.0.1:9042";

fn runtime() -> RuntimeEnvelope<'static> {
    RuntimeEnvelope {
        process_generation: 17,
        port: 8317,
        auth_dir: "owned-auth",
        policy_url: POLICY_URL,
        policy_origin: POLICY_ORIGIN,
        ready_key: READY_KEY,
        hop_secret: HOP_SECRET,
        policy_token: POLICY_TOKEN,
    }
}

struct Fixture {
    db: Option<Database>,
    dir: PathBuf,
    cipher: StaticKeyCipher,
}

impl Fixture {
    fn open(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ocg-cpa-proj-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = Database::open(dir.clone()).expect("database");
        Self {
            db: Some(db),
            dir,
            cipher: StaticKeyCipher::new(CIPHER_SECRET),
        }
    }

    fn db(&self) -> &Database {
        self.db.as_ref().expect("database")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.db.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn account(cipher: &StaticKeyCipher, id: &str, provider_id: &str, material: &str) -> Account {
    let plan = builtin_provider(provider_id);
    Account {
        id: id.into(),
        provider_id: provider_id.into(),
        credential_kind: plan
            .map(|plan| plan.credential_kind)
            .unwrap_or_else(crate::provider::default_credential_kind),
        quota_scope: plan
            .map(|plan| plan.quota_scope)
            .unwrap_or_else(crate::provider::default_quota_scope),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt(material).expect("encrypt"),
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
        notes: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn create_keyed(fx: &Fixture, id: &str, provider_id: &str, material: &str) {
    fx.db()
        .create_account(&account(&fx.cipher, id, provider_id, material))
        .unwrap_or_else(|error| panic!("create {id}: {error}"));
}

fn catalog_model(public_name: &str, upstream: &str) -> CatalogModel {
    CatalogModel {
        public_model: public_name.into(),
        upstream_model: upstream.into(),
        protocols: Vec::new(),
        preferred: None,
        enabled: true,
        upstream_override: None,
    }
}

fn set_catalog(fx: &Fixture, destination_id: &str, models: &[CatalogModel]) {
    crate::db::destination_store::replace_destination_catalog(
        &fx.db().conn,
        destination_id,
        models,
    )
    .unwrap_or_else(|error| panic!("catalog {destination_id}: {error}"));
}

fn set_rank(fx: &Fixture, legacy_account_id: &str, rank: i64) {
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET routing_rank = ?2 WHERE legacy_account_id = ?1",
            rusqlite::params![legacy_account_id, rank],
        )
        .unwrap();
}

fn disable_zen(fx: &Fixture) {
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0 WHERE legacy_account_id = ?1",
            [ZEN_FREE_ACCOUNT_ID],
        )
        .unwrap();
}

fn config() -> AppConfig {
    let mut config = AppConfig::default();
    config.gateway_key = GATEWAY_SENTINEL.to_string();
    config
}

fn project_at(
    fx: &Fixture,
    config: &AppConfig,
    oauth: &[super::OAuthFileRef],
    presets: &BTreeMap<String, String>,
    revisions: ProjectionRevisions,
) -> Result<super::ProductProjection, super::ProjectionError> {
    let snapshot = RoutingSnapshot::load(fx.db()).expect("snapshot");
    project(ProjectionInput {
        snapshot: &snapshot,
        config,
        cipher: &fx.cipher,
        revisions,
        owned_listener: OWNED_LISTENER,
        owned_origins: &[],
        owned_destination_ids: &[],
        oauth_refs: oauth,
        preset_ids: presets,
        validation: super::ValidationSidecar::none(),
        runtime: runtime(),
    })
}

fn preserved_cpa_bytes(fx: &Fixture) -> (String, String, String) {
    let integration = fx
        .db()
        .cpa_integration()
        .expect("integration read")
        .expect("integration");
    let key_cipher: String = fx
        .db()
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            [CPA_ACCOUNT_ID],
            |row| row.get(0),
        )
        .expect("inference cipher");
    (
        integration.base_url,
        integration.management_key_cipher,
        key_cipher,
    )
}

fn project_fx(fx: &Fixture) -> super::ProductProjection {
    project_at(
        fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 4,
            applied: 1,
        },
    )
    .expect("projection")
}

fn auth<'a>(
    projection: &'a super::ProductProjection,
    legacy_account_id: &str,
) -> &'a super::ProjectedAuth {
    projection
        .auths
        .iter()
        .find(|auth| auth.legacy_account_id == legacy_account_id)
        .unwrap_or_else(|| panic!("missing auth {legacy_account_id}"))
}

fn assert_route(auth: &super::ProjectedAuth, protocol: &str, url: &str, scheme: &str) {
    let route = auth
        .models
        .iter()
        .flat_map(|model| model.routes.iter())
        .find(|route| route.protocol == protocol)
        .unwrap_or_else(|| panic!("missing {protocol}"));
    assert_eq!(route.endpoint_url, url, "{protocol}");
    assert_eq!(route.auth, scheme, "{protocol}");
}

fn joined(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[test]
fn sealed_providers_keep_one_auth_and_official_routes() {
    let fx = Fixture::open("sealed");
    let material = "synthetic-sealed-material";
    create_keyed(&fx, "go-key", OPENCODE_PROVIDER_ID, material);
    create_keyed(&fx, "goat-key", COMMAND_CODE_PROVIDER_ID, material);
    create_keyed(&fx, "minimax-key", MINIMAX_PROVIDER_ID, material);
    create_keyed(&fx, "kimi-key", KIMI_PROVIDER_ID, material);
    create_keyed(&fx, "ollama-key", OLLAMA_PROVIDER_ID, material);
    for (id, rank) in [
        (ZEN_FREE_ACCOUNT_ID, 0),
        ("go-key", 1),
        ("goat-key", 2),
        ("minimax-key", 3),
        ("kimi-key", 4),
        ("ollama-key", 5),
    ] {
        set_rank(&fx, id, rank);
    }
    set_catalog(
        &fx,
        &destination_id_for_builtin(crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID),
        &[catalog_model("zen-public", "zen-upstream")],
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID),
        &[catalog_model(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        )],
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(MINIMAX_PROVIDER_ID),
        &[catalog_model("mm-public", "mm-upstream")],
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(KIMI_PROVIDER_ID),
        &[catalog_model("kimi-public", "kimi-upstream")],
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(OLLAMA_PROVIDER_ID),
        &[catalog_model("ollama-public", "ollama-upstream")],
    );

    let projection = project_fx(&fx);
    assert_eq!(projection.revisions.desired, 4);
    assert_eq!(projection.revisions.applied, 1);
    assert_ne!(projection.revisions.desired, projection.revisions.applied);
    assert_eq!(projection.auths.len(), 6);
    assert!(
        projection
            .auths
            .iter()
            .all(|auth| auth.credential_version > 0)
    );
    let ids: std::collections::BTreeSet<_> = projection
        .auths
        .iter()
        .map(|auth| auth.credential_id.as_str())
        .collect();
    assert_eq!(ids.len(), projection.auths.len());
    assert_eq!(
        projection
            .auths
            .iter()
            .map(|auth| auth.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5]
    );

    let zen = auth(&projection, ZEN_FREE_ACCOUNT_ID);
    assert!(zen.material.is_none());
    assert_eq!(zen.provenance.quota_scope, "egress-ip");
    assert_eq!(zen.provenance.adapter, "zen");
    assert_eq!(
        zen.request_identity,
        super::RequestIdentityFact::OpenCodeSession
    );
    assert_eq!(zen.models[0].routes.len(), 3);
    assert_route(
        zen,
        "chat_completions",
        &joined(OPENCODE_ZEN_BASE_URL, "/v1/chat/completions"),
        AuthScheme::None.as_str(),
    );
    assert_route(
        zen,
        "messages",
        &joined(OPENCODE_ZEN_BASE_URL, "/v1/messages"),
        AuthScheme::None.as_str(),
    );

    let go = auth(&projection, "go-key");
    assert_eq!(
        go.material.as_ref().map(|secret| secret.expose()),
        Some(material)
    );
    assert_eq!(go.provenance.provider_id, OPENCODE_PROVIDER_ID);
    assert_eq!(go.provenance.legacy_kind, "builtin");
    assert_eq!(go.provenance.quota_scope, "key");
    assert_eq!(
        go.request_identity,
        super::RequestIdentityFact::OpenCodeSession
    );
    assert_eq!(go.routing_rank, 1);
    assert_eq!(
        go.models[0]
            .routes
            .iter()
            .map(|route| route.protocol.as_str())
            .collect::<Vec<_>>(),
        vec!["chat_completions", "responses", "messages"]
    );
    assert_route(
        go,
        "chat_completions",
        &joined(OPENCODE_GO_BASE_URL, "/v1/chat/completions"),
        "bearer",
    );
    assert_route(
        go,
        "responses",
        &joined(OPENCODE_GO_BASE_URL, "/v1/responses"),
        "bearer",
    );
    assert_route(
        go,
        "messages",
        &joined(OPENCODE_GO_BASE_URL, "/v1/messages"),
        "x_api_key",
    );

    let goat = auth(&projection, "goat-key");
    assert_eq!(goat.models.len(), 1);
    assert_eq!(
        goat.models[0].public_alias,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS
    );
    assert_eq!(
        goat.models[0].upstream_name,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
    );
    assert_route(
        goat,
        "chat_completions",
        &joined(
            COMMAND_CODE_GOAT_BASE_URL,
            COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH,
        ),
        "bearer",
    );
    assert_route(
        goat,
        "responses",
        &joined(COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_RESPONSES_PATH),
        "bearer",
    );
    assert_route(
        goat,
        "messages",
        &joined(COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_MESSAGES_PATH),
        "bearer",
    );

    let minimax = auth(&projection, "minimax-key");
    assert_route(
        minimax,
        "chat_completions",
        &joined(MINIMAX_CN_BASE_URL, MINIMAX_CN_CHAT_COMPLETIONS_PATH),
        "bearer",
    );
    assert_route(
        minimax,
        "responses",
        &joined(MINIMAX_CN_BASE_URL, MINIMAX_CN_RESPONSES_PATH),
        "bearer",
    );
    assert_route(
        minimax,
        "messages",
        &joined(MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_MESSAGES_PATH),
        "bearer",
    );

    let kimi = auth(&projection, "kimi-key");
    assert_eq!(kimi.models[0].routes.len(), 2);
    assert!(
        kimi.models[0]
            .routes
            .iter()
            .all(|route| route.protocol != "responses")
    );
    assert_route(
        kimi,
        "chat_completions",
        &joined(KIMI_CN_BASE_URL, KIMI_CN_CHAT_COMPLETIONS_PATH),
        "bearer",
    );
    assert_route(
        kimi,
        "messages",
        &joined(KIMI_CN_BASE_URL, KIMI_CN_MESSAGES_PATH),
        "bearer",
    );

    let ollama = auth(&projection, "ollama-key");
    assert_eq!(ollama.wire, super::WireFact::OllamaReasoning);
    assert_eq!(ollama.models[0].routes.len(), 1);
    assert_route(
        ollama,
        "chat_completions",
        &joined(OLLAMA_CLOUD_BASE_URL, OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH),
        "bearer",
    );

    let rendered = format!("{projection:?}");
    assert!(!rendered.contains(material), "debug leaked material");
    assert!(
        !rendered.contains(GATEWAY_SENTINEL),
        "debug leaked gateway key"
    );
    assert!(!rendered.contains(HOP_SECRET));
    assert!(!rendered.contains(READY_KEY));
    assert!(!rendered.contains(POLICY_TOKEN));
    assert!(rendered.contains("[redacted]"));
    assert_eq!(projection.digest.len(), 64);
    let canonical = render_canonical_yaml(&projection).expect("sealed yaml");
    assert_eq!(canonical.wire_digest, super::wire_digest(&canonical.yaml));
    assert!(!canonical.yaml.contains(&canonical.wire_digest));
    assert!(!canonical.yaml.contains(GATEWAY_SENTINEL));
    assert_eq!(canonical.yaml.matches(material).count(), 5);
    let doc: serde_json::Value = serde_yaml_ng::from_str(&canonical.yaml).unwrap();
    assert_eq!(doc["api-keys"], serde_json::json!([HOP_SECRET]));
    assert_eq!(doc["ocg"]["process-generation"], serde_json::json!("17"));
    assert_eq!(doc["ocg"]["projection-revision"], serde_json::json!("4"));
    assert_eq!(doc["ocg"]["policy"]["url"], serde_json::json!(POLICY_URL));
    assert_eq!(
        doc["ocg"]["policy"]["token"],
        serde_json::json!(POLICY_TOKEN)
    );
    assert_eq!(doc["ocg"]["ready-key"], serde_json::json!(READY_KEY));
    let credentials = doc["ocg"]["credentials"].as_array().unwrap();
    let zen = credentials
        .iter()
        .find(|item| {
            item["provider-id"] == serde_json::json!(crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID)
        })
        .expect("zen credential");
    assert_eq!(zen["material-revision"], serde_json::json!("no-material"));
    let zen_entry = doc["openai-compatibility"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == zen["namespace"])
        .expect("zen openai entry");
    assert_eq!(
        zen_entry["api-key-entries"][0]["api-key"],
        serde_json::json!("")
    );
    assert!(
        zen["routes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|route| route["auth-scheme"] == serde_json::json!("none"))
    );
    assert!(!canonical.yaml.contains("ocg-keyless"));
    let go = credentials
        .iter()
        .find(|item| item["provider-id"] == serde_json::json!(OPENCODE_PROVIDER_ID))
        .expect("go credential");
    assert_eq!(
        go["routes"][0]["request-identity"],
        serde_json::json!("opencode-session")
    );
    assert_ne!(go["namespace"], go["provider-id"]);
    let ocg = serde_json::to_string(&doc["ocg"]).unwrap();
    assert!(!ocg.contains(material));
    assert!(!ocg.contains(HOP_SECRET));
    assert!(!ocg.contains("ocg-keyless"));
    assert!(!canonical.yaml.contains("ocg-keyless"));
    assert!(doc["ocg"].get("proxy-list").is_none());
}

#[test]
fn stored_loopback_is_not_rewritten_to_the_official_host() {
    let fx = Fixture::open("ollama-origin");
    disable_zen(&fx);
    let material = "synthetic-ollama-material";
    create_keyed(&fx, "ollama-key", OLLAMA_PROVIDER_ID, material);
    let destination_id = destination_id_for_builtin(OLLAMA_PROVIDER_ID);
    set_catalog(
        &fx,
        &destination_id,
        &[catalog_model("ollama-public", "ollama-upstream")],
    );
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET base_url = 'http://127.0.0.1:9' WHERE id = ?1",
            [&destination_id],
        )
        .unwrap();
    let outcome = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    );
    #[cfg(not(feature = "ollama-cloud-loopback-test"))]
    {
        let error = outcome.expect_err("feature-off loopback");
        assert_eq!(error.code, "invalid_endpoint");
        let rendered = error.to_string();
        assert!(rendered.contains("ollama-cloud-loopback-test"));
        assert!(!rendered.contains("ollama.com"));
        assert!(!rendered.contains("127.0.0.1"));
        assert!(!rendered.contains(material));
    }
    #[cfg(feature = "ollama-cloud-loopback-test")]
    {
        let projection = outcome.expect("feature-on loopback stays off the official host");
        let ollama = auth(&projection, "ollama-key");
        assert_route(
            ollama,
            "chat_completions",
            "http://127.0.0.1:9/v1/chat/completions",
            "bearer",
        );
        let canonical = render_canonical_yaml(&projection).expect("loopback yaml");
        assert!(
            canonical
                .yaml
                .contains("http://127.0.0.1:9/v1/chat/completions")
        );
        assert!(!canonical.yaml.contains("ollama.com"));
        assert!(!format!("{projection:?}").contains(material));
    }

    let fx = Fixture::open("foreign-sealed");
    disable_zen(&fx);
    create_keyed(&fx, "go-key", OPENCODE_PROVIDER_ID, "synthetic-go-material");
    let destination_id = destination_id_for_builtin(OPENCODE_PROVIDER_ID);
    set_catalog(
        &fx,
        &destination_id,
        &[catalog_model("go-public", "go-upstream")],
    );
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET base_url = 'https://not-official.example' WHERE id = ?1",
            [&destination_id],
        )
        .unwrap();
    let error = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("foreign sealed host");
    assert_eq!(error.code, "invalid_endpoint");
    let rendered = error.to_string();
    assert!(!rendered.contains("not-official.example"));
    assert!(!rendered.contains("synthetic-go-material"));
}

#[test]
fn one_credential_keeps_ordered_protocol_routes_and_aliases() {
    let fx = Fixture::open("protocols");
    disable_zen(&fx);
    let material = "synthetic-custom-material";
    fx.db()
        .create_account_with_contract(
            &account(&fx.cipher, "custom-key", CUSTOM_PROVIDER_ID, material),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "seed".into(),
                upstream_model: "seed-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .expect("custom account");
    let destination_id = destination_id_for_custom_account("custom-key");
    let routes = vec![
        HttpProtocolRoute {
            protocol: UpstreamProtocolKind::ChatCompletions,
            endpoint_url: "https://lab.example/v1/chat/completions".into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: UpstreamProtocolKind::Messages,
            endpoint_url: "https://lab.example/anthropic/v1/messages".into(),
            auth_scheme: AuthScheme::XApiKey,
        },
        HttpProtocolRoute {
            protocol: UpstreamProtocolKind::Responses,
            endpoint_url: "https://lab.example/v1/responses".into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let protocols: Vec<_> = routes.iter().map(|route| route.protocol).collect();
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3, base_url = ?4 WHERE id = ?1",
            rusqlite::params![
                &destination_id,
                serde_json::to_string(&routes).unwrap(),
                serde_json::to_string(&protocols).unwrap(),
                routes[0].endpoint_url,
            ],
        )
        .unwrap();
    grant_operation(&fx, "custom-key", EndpointOperation::MessageCreate);
    grant_operation(&fx, "custom-key", EndpointOperation::ResponseCreate);
    grant_route(
        &fx,
        "custom-key",
        EndpointOperation::ResponseCreate,
        "https://lab.example/v1/responses?api-version=2024-10-01",
    );
    set_catalog(
        &fx,
        &destination_id,
        &[
            CatalogModel {
                public_model: "public-multi".into(),
                upstream_model: "upstream-multi".into(),
                protocols: vec![
                    UpstreamProtocolKind::ChatCompletions,
                    UpstreamProtocolKind::Messages,
                ],
                preferred: None,
                enabled: true,
                upstream_override: None,
            },
            CatalogModel {
                public_model: "public-override".into(),
                upstream_model: "upstream-override".into(),
                protocols: vec![UpstreamProtocolKind::Responses],
                preferred: None,
                enabled: true,
                upstream_override: Some(DynamicModelUpstreamOverride {
                    protocol: UpstreamProtocolKind::Responses,
                    endpoint_url: "https://lab.example/v1/responses?api-version=2024-10-01".into(),
                }),
            },
            CatalogModel {
                public_model: "hidden-from-discovery".into(),
                upstream_model: "hidden-upstream".into(),
                protocols: vec![UpstreamProtocolKind::ChatCompletions],
                preferred: None,
                enabled: true,
                upstream_override: None,
            },
            CatalogModel {
                public_model: "disabled-public".into(),
                upstream_model: "disabled-upstream".into(),
                protocols: vec![UpstreamProtocolKind::ChatCompletions],
                preferred: None,
                enabled: false,
                upstream_override: None,
            },
        ],
    );
    let binding = binding_of(&fx, "custom-key");
    fx.db()
        .update_credential_binding(&binding, Some(&ModelScope::All), None, None, None)
        .unwrap();
    let mut presets = BTreeMap::new();
    presets.insert(destination_id.clone(), "preset-template".into());
    let projection = project_at(
        &fx,
        &config(),
        &[],
        &presets,
        ProjectionRevisions {
            desired: 2,
            applied: 2,
        },
    )
    .expect("multi-protocol projection");
    assert_eq!(projection.auths.len(), 1);
    let auth = &projection.auths[0];
    assert_eq!(
        auth.provenance.preset_id.as_deref(),
        Some("preset-template")
    );
    assert_eq!(auth.provenance.adapter, "http");
    assert_eq!(auth.provenance.legacy_kind, "custom_account");
    assert_eq!(auth.models.len(), 3);
    assert_eq!(
        auth.models
            .iter()
            .map(|model| model.public_alias.as_str())
            .collect::<Vec<_>>(),
        vec!["public-multi", "public-override", "hidden-from-discovery"]
    );
    assert_eq!(
        auth.models[0]
            .routes
            .iter()
            .map(|route| (
                route.protocol.as_str(),
                route.auth.as_str(),
                route.endpoint_url.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "chat_completions",
                "bearer",
                "https://lab.example/v1/chat/completions",
            ),
            (
                "messages",
                "x_api_key",
                "https://lab.example/anthropic/v1/messages",
            ),
        ]
    );
    assert_eq!(auth.models[1].routes.len(), 1);
    assert_eq!(auth.models[1].routes[0].protocol, "responses");
    assert_eq!(
        auth.models[1].routes[0].endpoint_url,
        "https://lab.example/v1/responses?api-version=2024-10-01"
    );
    assert_eq!(auth.models[1].upstream_name, "upstream-override");
    assert_ne!(
        auth.models[1].routes[0].endpoint_id,
        auth.models[0].routes[0].endpoint_id
    );
    let chat_and_messages_same_auth = auth.models[0].routes.len() == 2;
    assert!(chat_and_messages_same_auth);
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "model_disabled"
                && item.model.as_deref() == Some("disabled-public"))
    );
    let canonical = render_canonical_yaml(&projection).expect("multi-protocol yaml");
    assert!(!canonical.yaml.contains(&canonical.wire_digest));
    assert!(!canonical.yaml.contains(GATEWAY_SENTINEL));
    let doc: serde_json::Value = serde_yaml_ng::from_str(&canonical.yaml).unwrap();
    assert_eq!(doc["openai-compatibility"].as_array().unwrap().len(), 1);
    assert_eq!(
        doc["openai-compatibility"][0]["name"],
        serde_json::json!(auth.auth_id)
    );
    assert_eq!(
        doc["openai-compatibility"][0]["priority"],
        serde_json::json!(1)
    );
    assert_eq!(
        doc["openai-compatibility"][0]["base-url"],
        serde_json::json!("https://lab.example/v1")
    );
    assert_eq!(
        doc["ocg"]["credentials"][0]["namespace"],
        serde_json::json!(auth.auth_id)
    );
    assert_eq!(
        doc["ocg"]["credentials"][0]["provider-id"],
        serde_json::json!(CUSTOM_PROVIDER_ID)
    );
    assert_ne!(
        doc["ocg"]["credentials"][0]["namespace"],
        doc["ocg"]["credentials"][0]["provider-id"]
    );
    let routes = doc["ocg"]["credentials"][0]["routes"].as_array().unwrap();
    assert_eq!(
        routes
            .iter()
            .map(|route| route["protocol"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "chat_completions",
            "messages",
            "responses",
            "chat_completions"
        ]
    );
    assert_eq!(routes[1]["auth-scheme"], serde_json::json!("x-api-key"));
    assert_eq!(
        routes[2]["endpoint"],
        serde_json::json!("https://lab.example/v1/responses?api-version=2024-10-01")
    );
    let ocg = serde_json::to_string(&doc["ocg"]).unwrap();
    assert!(!ocg.contains(material));
    assert!(!ocg.contains(HOP_SECRET));
    assert!(canonical.yaml.contains(material));

    let mut conflicted = project_at(
        &fx,
        &config(),
        &[],
        &presets,
        ProjectionRevisions {
            desired: 2,
            applied: 2,
        },
    )
    .unwrap();
    let alias = conflicted.auths[0].models[0].public_alias.clone();
    let routes = conflicted.auths[0].models[0].routes.clone();
    conflicted.auths[0]
        .models
        .push(super::types::ProjectedModel {
            public_alias: alias,
            upstream_name: "other-upstream".into(),
            routes,
        });
    let error = render_canonical_yaml(&conflicted).expect_err("conflicting alias");
    assert_eq!(error.code, "invalid_alias");
    assert!(!error.to_string().contains(material));
}

fn store_exact_http_route(fx: &Fixture, legacy: &str, endpoint: &str, auth: AuthScheme) {
    let destination_id = destination_id_for_custom_account(legacy);
    let routes = vec![HttpProtocolRoute {
        protocol: UpstreamProtocolKind::ChatCompletions,
        endpoint_url: endpoint.into(),
        auth_scheme: auth,
    }];
    let protocols = vec![UpstreamProtocolKind::ChatCompletions];
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3, base_url = ?4, auth_scheme = ?5 WHERE id = ?1",
            rusqlite::params![
                destination_id,
                serde_json::to_string(&routes).unwrap(),
                serde_json::to_string(&protocols).unwrap(),
                endpoint,
                auth.as_str(),
            ],
        )
        .unwrap();
}

fn clear_key(fx: &Fixture, legacy: &str) {
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET key_cipher = '' WHERE legacy_account_id = ?1",
            [legacy],
        )
        .unwrap();
}

fn grant_route(fx: &Fixture, legacy_account_id: &str, operation: EndpointOperation, url: &str) {
    let connection: String = fx
        .db()
        .conn
        .query_row(
            "SELECT authorization_connection_id FROM credentials WHERE legacy_account_id = ?1",
            [legacy_account_id],
            |row| row.get(0),
        )
        .unwrap();
    let connection: ConnectionId =
        serde_json::from_value(serde_json::Value::String(connection)).unwrap();
    let endpoint = endpoint_id_for_route(&connection, operation, url);
    fx.db()
        .conn
        .execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             SELECT id, 'endpoint_id', ?2 FROM credentials WHERE legacy_account_id = ?1",
            rusqlite::params![legacy_account_id, endpoint.to_string()],
        )
        .unwrap();
}

fn grant_operation(fx: &Fixture, legacy_account_id: &str, operation: EndpointOperation) {
    let connection: String = fx
        .db()
        .conn
        .query_row(
            "SELECT authorization_connection_id FROM credentials WHERE legacy_account_id = ?1",
            [legacy_account_id],
            |row| row.get(0),
        )
        .unwrap();
    let connection: ConnectionId =
        serde_json::from_value(serde_json::Value::String(connection)).unwrap();
    let endpoint = endpoint_id_for(&connection, operation);
    fx.db()
        .conn
        .execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             SELECT id, 'endpoint_id', ?2 FROM credentials WHERE legacy_account_id = ?1",
            rusqlite::params![legacy_account_id, endpoint.to_string()],
        )
        .unwrap();
}

#[test]
fn auth_id_tracks_version_binding_and_secret_without_exposing_it() {
    let fx = Fixture::open("identity");
    disable_zen(&fx);
    let material = "synthetic-rotation-material";
    create_keyed(&fx, "go-key", OPENCODE_PROVIDER_ID, material);
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    let first = project_fx(&fx);
    let first_auth = auth(&first, "go-key");
    let first_id = first_auth.auth_id.clone();
    let first_digest = first.digest.clone();
    let version = first_auth.credential_version;
    assert_eq!(first_id.len(), 64);

    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET credential_version = credential_version + 1 WHERE legacy_account_id = 'go-key'",
            [],
        )
        .unwrap();
    let bumped = project_fx(&fx);
    let bumped_auth = auth(&bumped, "go-key");
    assert_eq!(bumped_auth.credential_version, version + 1);
    assert_ne!(bumped_auth.auth_id, first_id);
    assert_ne!(bumped.digest, first_digest);

    let rebound = "rebind-binding-id";
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET binding_id = ?1 WHERE legacy_account_id = 'go-key'",
            [rebound],
        )
        .unwrap();
    let rebound_projection = project_fx(&fx);
    let rebound_auth = auth(&rebound_projection, "go-key");
    assert_eq!(rebound_auth.binding_id, rebound);
    assert_eq!(rebound_auth.credential_version, version + 1);
    assert_ne!(rebound_auth.auth_id, bumped_auth.auth_id);

    let rotated = "synthetic-rotated-material";
    let cipher = fx.cipher.encrypt(rotated).unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET key_cipher = ?1 WHERE legacy_account_id = 'go-key'",
            [cipher],
        )
        .unwrap();
    let rotated_projection = project_fx(&fx);
    let rotated_auth = auth(&rotated_projection, "go-key");
    assert_eq!(rotated_auth.credential_version, version + 1);
    assert_ne!(rotated_auth.auth_id, rebound_auth.auth_id);
    assert_ne!(rotated_projection.digest, rebound_projection.digest);
    assert_eq!(
        rotated_auth.material.as_ref().map(|secret| secret.expose()),
        Some(rotated)
    );
    let debug = format!("{rotated_projection:?}{rotated_auth:?}");
    assert!(!debug.contains(rotated));
    assert!(!debug.contains(material));

    let same_content = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 99,
            applied: 7,
        },
    )
    .unwrap();
    assert_eq!(same_content.digest, rotated_projection.digest);
    assert_eq!(same_content.revisions.desired, 99);
    assert_eq!(same_content.revisions.applied, 7);
    let first_yaml = render_canonical_yaml(&rotated_projection).unwrap();
    let revised_yaml = render_canonical_yaml(&same_content).unwrap();
    assert_ne!(first_yaml.wire_digest, revised_yaml.wire_digest);
    assert!(!revised_yaml.yaml.contains(&revised_yaml.wire_digest));
    assert_eq!(
        serde_yaml_ng::from_str::<serde_json::Value>(&revised_yaml.yaml).unwrap()["ocg"]["projection-revision"],
        serde_json::json!("99")
    );
    let mut generated = runtime();
    generated.process_generation = 18;
    let snapshot = RoutingSnapshot::load(fx.db()).unwrap();
    let other_generation = project(ProjectionInput {
        snapshot: &snapshot,
        config: &config(),
        cipher: &fx.cipher,
        revisions: ProjectionRevisions {
            desired: 99,
            applied: 7,
        },
        owned_listener: OWNED_LISTENER,
        owned_origins: &[],
        owned_destination_ids: &[],
        oauth_refs: &[],
        preset_ids: &BTreeMap::new(),
        validation: super::ValidationSidecar::none(),
        runtime: generated,
    })
    .unwrap();
    assert_eq!(other_generation.digest, same_content.digest);
    let other_yaml = render_canonical_yaml(&other_generation).unwrap();
    assert_ne!(other_yaml.wire_digest, revised_yaml.wire_digest);
}

#[test]
fn filters_omit_disabled_rows_and_keep_quota_recovery_projected() {
    let fx = Fixture::open("filters");
    disable_zen(&fx);
    let material = "synthetic-filter-material";
    for id in ["live-key", "off-key", "bind-off", "draft-key", "scope-key"] {
        create_keyed(&fx, id, OPENCODE_PROVIDER_ID, material);
    }
    fx.db()
        .create_account_with_contract(
            &account(&fx.cipher, "dest-off", CUSTOM_PROVIDER_ID, material),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "custom-public".into(),
                upstream_model: "custom-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[
            catalog_model("kept-public", "kept-upstream"),
            CatalogModel {
                enabled: false,
                ..catalog_model("paused-public", "paused-upstream")
            },
        ],
    );
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0 WHERE legacy_account_id = 'off-key'",
            [],
        )
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET enabled = 0 WHERE id = ?1",
            [destination_id_for_custom_account("dest-off")],
        )
        .unwrap();
    let binding = binding_of(&fx, "bind-off");
    fx.db()
        .update_credential_binding(&binding, None, Some(false), None, None)
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET setup_step = 'key_verification' WHERE legacy_account_id = 'draft-key'",
            [],
        )
        .unwrap();
    let scope_binding = binding_of(&fx, "scope-key");
    fx.db()
        .update_credential_binding(
            &scope_binding,
            Some(&ModelScope::Only {
                models: vec!["kept-public".into()],
            }),
            None,
            None,
            None,
        )
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET cooldown_generic_until = '2099-01-01T00:00:00Z', quota_recovery_json = ?1 WHERE legacy_account_id = 'live-key'",
            [r#"{"epoch":1,"reason":"quota_exhausted","windows":{"five_hours":"2099-01-01T00:00:00Z"},"observed_at":"2026-10-04T00:00:00Z","next_retry_at":"2099-01-01T00:00:00Z","failure_count":2}"#],
        )
        .unwrap();

    let projection = project_fx(&fx);
    assert!(
        projection
            .auths
            .iter()
            .any(|auth| auth.legacy_account_id == "live-key")
    );
    assert!(
        projection
            .auths
            .iter()
            .any(|auth| auth.legacy_account_id == "scope-key")
    );
    assert!(
        projection
            .auths
            .iter()
            .all(|auth| auth.legacy_account_id != "off-key")
    );
    assert!(
        projection
            .auths
            .iter()
            .all(|auth| auth.legacy_account_id != "dest-off")
    );
    assert!(
        projection
            .auths
            .iter()
            .all(|auth| auth.legacy_account_id != "bind-off")
    );
    assert!(
        projection
            .auths
            .iter()
            .all(|auth| auth.legacy_account_id != "draft-key")
    );
    let live = auth(&projection, "live-key");
    assert_eq!(live.models.len(), 1);
    assert_eq!(live.models[0].public_alias, "kept-public");
    let scope = auth(&projection, "scope-key");
    assert_eq!(scope.models.len(), 1);
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "credential_disabled")
    );
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "destination_disabled")
    );
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "binding_disabled")
    );
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "draft")
    );
    assert!(projection.omissions.iter().any(|item| {
        item.reason == "model_disabled" && item.model.as_deref() == Some("paused-public")
    }));

    let empty = Fixture::open("empty");
    disable_zen(&empty);
    let projection = project_fx(&empty);
    assert!(projection.auths.is_empty());
    assert!(
        projection
            .omissions
            .iter()
            .all(|item| item.reason != "fallback")
    );

    let scoped = Fixture::open("scope");
    disable_zen(&scoped);
    create_keyed(&scoped, "scope-key", OPENCODE_PROVIDER_ID, material);
    set_catalog(
        &scoped,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("kept-public", "kept-upstream")],
    );
    let binding = binding_of(&scoped, "scope-key");
    scoped
        .db()
        .update_credential_binding(
            &binding,
            Some(&ModelScope::Only { models: vec![] }),
            None,
            None,
            None,
        )
        .unwrap();
    let projection = project_fx(&scoped);
    assert!(projection.auths.is_empty());
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "scope_empty")
    );

    scoped
        .db()
        .update_credential_binding(
            &binding,
            Some(&ModelScope::Only {
                models: vec!["missing-model".into()],
            }),
            None,
            None,
            None,
        )
        .unwrap();
    let error = project_at(
        &scoped,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("unknown scope");
    assert_eq!(error.code, "invalid_scope");
    assert!(!error.to_string().contains(material));
}

fn binding_of(fx: &Fixture, legacy_account_id: &str) -> String {
    fx.db()
        .conn
        .query_row(
            "SELECT binding_id FROM credentials WHERE legacy_account_id = ?1",
            [legacy_account_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn invalid_endpoint_grant_and_proxy_do_not_echo_secrets() {
    let fx = Fixture::open("invalid");
    disable_zen(&fx);
    let material = "synthetic-invalid-material";
    fx.db()
        .create_account_with_contract(
            &account(&fx.cipher, "custom-key", CUSTOM_PROVIDER_ID, material),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "public".into(),
                upstream_model: "upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET base_url = 'https://user:pass@bad.example/v1', protocol_routes_json = NULL WHERE legacy_kind = 'custom_account'",
            [],
        )
        .unwrap();
    let error = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("bad endpoint");
    assert_eq!(error.code, "invalid_endpoint");
    let rendered = error.to_string();
    assert!(!rendered.contains("user:pass"));
    assert!(!rendered.contains("bad.example"));
    assert!(!rendered.contains(material));

    let fx = Fixture::open("grant");
    disable_zen(&fx);
    create_keyed(&fx, "go-key", OPENCODE_PROVIDER_ID, material);
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    fx.db()
        .conn
        .execute(
            "DELETE FROM credential_grants WHERE kind = 'endpoint_id' AND credential_id = (SELECT id FROM credentials WHERE legacy_account_id = 'go-key')",
            [],
        )
        .unwrap();
    let error = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("missing grant");
    assert_eq!(error.code, "invalid_grant");
    assert!(!error.to_string().contains(material));

    let fx = Fixture::open("proxy");
    disable_zen(&fx);
    create_keyed(&fx, "ollama-key", OLLAMA_PROVIDER_ID, material);
    set_catalog(
        &fx,
        &destination_id_for_builtin(OLLAMA_PROVIDER_ID),
        &[catalog_model("ollama-public", "ollama-upstream")],
    );
    let mut manual = config();
    manual.proxy_mode = ProxyMode::Manual;
    manual.proxy_url = "http://user:pass@127.0.0.1:8080".into();
    let error = project_at(
        &fx,
        &manual,
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("proxy credentials");
    assert_eq!(error.code, "invalid_proxy");
    assert!(!error.to_string().contains("user:pass"));
    let mut socks = config();
    socks.proxy_mode = ProxyMode::Manual;
    socks.proxy_url = "socks5://127.0.0.1:1080".into();
    let error = project_at(
        &fx,
        &socks,
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("socks proxy");
    assert_eq!(error.code, "invalid_proxy");
    assert!(!error.to_string().contains("socks5"));
    let mut list = config();
    list.proxy_mode = ProxyMode::List;
    list.proxy_url = "http://127.0.0.1:8080".into();
    list.proxy_list_models.clear();
    let error = project_at(
        &fx,
        &list,
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("empty list");
    assert_eq!(error.code, "invalid_proxy");
}

#[test]
fn remote_cpa_is_omitted_and_owned_pool_is_not_an_upstream() {
    let fx = Fixture::open("remote");
    disable_zen(&fx);
    let inference = "synthetic-remote-inference";
    let management = "synthetic-remote-management";
    let mut remote_account = account(&fx.cipher, CPA_ACCOUNT_ID, CPA_PROVIDER_ID, inference);
    remote_account.id = CPA_ACCOUNT_ID.into();
    let management_cipher = fx.cipher.encrypt(management).unwrap();
    fx.db()
        .upsert_cpa_integration(
            &remote_account,
            "https://remote.example",
            &management_cipher,
        )
        .expect("remote cpa");
    set_catalog(
        &fx,
        &destination_id_for_builtin(CPA_PROVIDER_ID),
        &[catalog_model("remote-public", "remote-upstream")],
    );
    let snapshot = RoutingSnapshot::load(fx.db()).unwrap();
    assert_eq!(
        snapshot
            .credentials
            .iter()
            .filter(|credential| credential.provider_id == CPA_PROVIDER_ID)
            .count(),
        1
    );
    let before = preserved_cpa_bytes(&fx);
    let projection = project_fx(&fx);
    assert!(projection.auths.is_empty());
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "migration_required")
    );
    assert!(
        !projection
            .omissions
            .iter()
            .any(|item| item.reason == "owned_oauth_pool")
    );
    let debug = format!("{projection:?}");
    assert!(!debug.contains(inference));
    assert!(!debug.contains(management));
    let canonical = render_canonical_yaml(&projection).expect("remote yaml");
    let doc: serde_json::Value = serde_yaml_ng::from_str(&canonical.yaml).unwrap();
    assert!(doc["openai-compatibility"].as_array().unwrap().is_empty());
    assert!(!canonical.yaml.contains(inference));
    assert!(!canonical.yaml.contains(management));
    assert!(!canonical.yaml.contains("remote.example"));
    assert!(!canonical.yaml.contains("opaque-remote: true"));
    assert_eq!(canonical.wire_digest, super::wire_digest(&canonical.yaml));
    assert!(!canonical.yaml.contains(&canonical.wire_digest));
    assert_eq!(preserved_cpa_bytes(&fx), before);

    fx.db()
        .create_account_with_contract(
            &account(
                &fx.cipher,
                "custom-beside-remote",
                CUSTOM_PROVIDER_ID,
                "synthetic-custom-beside",
            ),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "custom-public".into(),
                upstream_model: "custom-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    set_catalog(
        &fx,
        &destination_id_for_custom_account("custom-beside-remote"),
        &[catalog_model("custom-public", "custom-upstream")],
    );
    let mixed = project_fx(&fx);
    assert_eq!(mixed.auths.len(), 1);
    assert_eq!(mixed.auths[0].provenance.adapter, "http");
    assert_eq!(mixed.auths[0].remote_inner, super::RemoteInner::NotRemote);
    assert!(
        mixed
            .omissions
            .iter()
            .any(|item| item.reason == "migration_required")
    );
    let mixed_yaml = render_canonical_yaml(&mixed).expect("custom beside remote");
    assert!(
        mixed_yaml
            .yaml
            .contains("https://lab.example/v1/chat/completions")
    );
    assert!(!mixed_yaml.yaml.contains("remote.example"));
    assert!(!mixed_yaml.yaml.contains(inference));
    assert!(!mixed_yaml.yaml.contains(management));
    assert_eq!(mixed_yaml.wire_digest, super::wire_digest(&mixed_yaml.yaml));
    assert_eq!(preserved_cpa_bytes(&fx), before);

    let fx = Fixture::open("owned-listener");
    disable_zen(&fx);
    let listener_account = account(&fx.cipher, CPA_ACCOUNT_ID, CPA_PROVIDER_ID, inference);
    let destination_id = destination_id_for_builtin(CPA_PROVIDER_ID);
    fx.db()
        .upsert_cpa_integration(&listener_account, OWNED_LISTENER, &management_cipher)
        .unwrap();
    set_catalog(
        &fx,
        &destination_id,
        &[catalog_model("remote-public", "remote-upstream")],
    );
    let revisions = ProjectionRevisions {
        desired: 1,
        applied: 0,
    };
    let owned =
        project_at(&fx, &config(), &[], &BTreeMap::new(), revisions).expect("exact restart");
    assert!(owned.auths.is_empty());
    assert!(
        owned
            .omissions
            .iter()
            .any(|item| item.reason == "owned_oauth_pool")
    );
    let owned_yaml = render_canonical_yaml(&owned).expect("owned yaml");
    let owned_doc: serde_json::Value = serde_yaml_ng::from_str(&owned_yaml.yaml).unwrap();
    assert!(
        owned_doc["openai-compatibility"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!owned_yaml.yaml.contains(inference));

    fx.db()
        .upsert_cpa_integration(&listener_account, "http://127.0.0.1:9", &management_cipher)
        .unwrap();
    let other_before = preserved_cpa_bytes(&fx);
    let other_port = project_at(&fx, &config(), &[], &BTreeMap::new(), revisions)
        .expect("different port is historical");
    assert!(other_port.auths.is_empty());
    assert!(
        other_port
            .omissions
            .iter()
            .any(|item| item.reason == "migration_required")
    );
    assert!(
        !other_port
            .omissions
            .iter()
            .any(|item| item.reason == "owned_oauth_pool")
    );
    let other_yaml = render_canonical_yaml(&other_port).expect("other port yaml");
    // The policy origin is 127.0.0.1:9042. Match the historical origin itself.
    let yaml_without_policy_origin = other_yaml.yaml.replace(POLICY_ORIGIN, "");
    assert!(
        !yaml_without_policy_origin.contains("127.0.0.1:9"),
        "{yaml_without_policy_origin}"
    );
    assert!(!other_yaml.yaml.contains(inference));
    assert_eq!(other_yaml.wire_digest, super::wire_digest(&other_yaml.yaml));
    assert_eq!(preserved_cpa_bytes(&fx), other_before);

    fx.db()
        .upsert_cpa_integration(
            &listener_account,
            "http://127.0.0.1:8400",
            &management_cipher,
        )
        .unwrap();
    let snapshot = RoutingSnapshot::load(fx.db()).unwrap();
    let migrated = project(ProjectionInput {
        snapshot: &snapshot,
        config: &config(),
        cipher: &fx.cipher,
        revisions,
        owned_listener: OWNED_LISTENER,
        owned_origins: &["http://127.0.0.1:8400"],
        owned_destination_ids: &[],
        oauth_refs: &[],
        preset_ids: &BTreeMap::new(),
        validation: super::ValidationSidecar::none(),
        runtime: runtime(),
    })
    .expect("caller-supplied previous origin");
    assert!(migrated.auths.is_empty());
    assert!(
        migrated
            .omissions
            .iter()
            .any(|item| item.reason == "owned_oauth_pool")
    );
    let unlisted_before = preserved_cpa_bytes(&fx);
    let unlisted = project_at(&fx, &config(), &[], &BTreeMap::new(), revisions)
        .expect("unlisted previous port is historical");
    assert!(unlisted.auths.is_empty());
    assert!(
        unlisted
            .omissions
            .iter()
            .any(|item| item.reason == "migration_required")
    );
    assert_eq!(preserved_cpa_bytes(&fx), unlisted_before);

    fx.db()
        .upsert_cpa_integration(&listener_account, "http://cpa:8317", &management_cipher)
        .unwrap();
    let named_before = preserved_cpa_bytes(&fx);
    let named_host = project_at(&fx, &config(), &[], &BTreeMap::new(), revisions)
        .expect("hostname cpa is historical");
    assert!(named_host.auths.is_empty());
    assert!(
        named_host
            .omissions
            .iter()
            .any(|item| item.reason == "migration_required")
    );
    let named_yaml = render_canonical_yaml(&named_host).expect("named host yaml");
    assert!(!named_yaml.yaml.contains("cpa:8317"));
    assert!(!named_yaml.yaml.contains(inference));
    assert_eq!(preserved_cpa_bytes(&fx), named_before);

    fx.db()
        .upsert_cpa_integration(&listener_account, "http://127.0.0.1:9", &management_cipher)
        .unwrap();
    let snapshot = RoutingSnapshot::load(fx.db()).unwrap();
    let id = destination_id.as_str();
    let error = project(ProjectionInput {
        snapshot: &snapshot,
        config: &config(),
        cipher: &fx.cipher,
        revisions,
        owned_listener: OWNED_LISTENER,
        owned_origins: &[],
        owned_destination_ids: &[id],
        oauth_refs: &[],
        preset_ids: &BTreeMap::new(),
        validation: super::ValidationSidecar::none(),
        runtime: runtime(),
    })
    .expect_err("ambiguous owned reference");
    assert_eq!(error.code, "ambiguous_owned_pool");
    let rendered = error.to_string();
    assert!(!rendered.contains("127.0.0.1"));
    assert!(!rendered.contains(inference));

    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET key_cipher = '' WHERE legacy_account_id = ?1",
            [CPA_ACCOUNT_ID],
        )
        .unwrap();
    let keyless_before = preserved_cpa_bytes(&fx);
    let keyless_remote = project_at(&fx, &config(), &[], &BTreeMap::new(), revisions)
        .expect("historical remote key is not authority");
    assert!(keyless_remote.auths.is_empty());
    assert!(
        keyless_remote
            .omissions
            .iter()
            .any(|item| item.reason == "migration_required")
    );
    assert!(
        !keyless_remote
            .omissions
            .iter()
            .any(|item| item.reason == "missing_material")
    );
    assert!(
        !keyless_remote
            .omissions
            .iter()
            .any(|item| item.reason == "owned_oauth_pool")
    );
    assert!(keyless_before.2.is_empty());
    assert_eq!(preserved_cpa_bytes(&fx), keyless_before);
    fx.db()
        .upsert_cpa_integration(&listener_account, "not-a-url", &management_cipher)
        .unwrap();
    let error = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("invalid owned base");
    assert_eq!(error.code, "invalid_endpoint");
    assert!(!error.to_string().contains(inference));
    assert!(!error.to_string().contains("not-a-url"));

    let fx = Fixture::open("owned-pool");
    disable_zen(&fx);
    let pool_account = account(&fx.cipher, CPA_ACCOUNT_ID, CPA_PROVIDER_ID, inference);
    fx.db()
        .upsert_cpa_integration(&pool_account, "", &management_cipher)
        .unwrap();
    set_catalog(
        &fx,
        &destination_id_for_builtin(CPA_PROVIDER_ID),
        &[catalog_model("pool-public", "pool-upstream")],
    );
    let oauth = [super::OAuthFileRef {
        provider: crate::cpa::CpaOAuthProvider::Codex,
        product_provider_id: "cpa".into(),
        relative_path: "codex/token.json".into(),
        material_revision: OAUTH_SHA.into(),
        auth_id: "codex/token.json".into(),
        credential_id: "oauth-codex-ref".into(),
        credential_version: 3,
        routing_rank: 1,
        models: vec!["pool-public".into()],
        raw_provider_label: String::new(),
        native_mode: String::new(),
        reported_base: String::new(),
    }];
    let projection = project_at(
        &fx,
        &config(),
        &oauth,
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 8,
            applied: 3,
        },
    )
    .expect("owned pool");
    assert!(projection.auths.is_empty());
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "owned_oauth_pool")
    );
    assert_eq!(projection.oauth_refs.len(), 1);
    assert_eq!(projection.oauth_refs[0].relative_path, "codex/token.json");
    let owned_debug = format!("{projection:?}");
    assert!(!owned_debug.contains(inference));
    assert!(!owned_debug.contains(management));
    let canonical = render_canonical_yaml(&projection).expect("owned pool yaml");
    let doc: serde_json::Value = serde_yaml_ng::from_str(&canonical.yaml).unwrap();
    assert!(doc["openai-compatibility"].as_array().unwrap().is_empty());
    assert_eq!(
        doc["ocg"]["oauth-bindings"][0]["relative-path"],
        serde_json::json!("codex/token.json")
    );
    assert_eq!(
        doc["ocg"]["oauth-bindings"][0]["provider-id"],
        serde_json::json!("cpa")
    );
    assert_eq!(
        doc["ocg"]["oauth-bindings"][0]["native-provider"],
        serde_json::json!("codex")
    );
    assert_eq!(
        doc["ocg"]["oauth-bindings"][0]["credential-version"],
        serde_json::json!("3")
    );
    assert_eq!(
        doc["ocg"]["oauth-bindings"][0]["material-revision"],
        serde_json::json!(OAUTH_SHA)
    );
    assert_eq!(
        doc["ocg"]["oauth-bindings"][0]["priority"],
        serde_json::json!(1)
    );
    assert!(
        doc["ocg"]["oauth-bindings"][0]
            .get("native-route-fingerprint")
            .is_none()
    );
    assert_eq!(projection.oauth_refs[0].routing_rank, 1);
    let mut annotated = project_at(
        &fx,
        &config(),
        &oauth,
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 8,
            applied: 3,
        },
    )
    .expect("annotation projection");
    let (pin, targets) = super::native_route_targets(
        &super::normalize_native_label("codex"),
        "pool-public",
        "chat_completions",
        &owned_native_connection(),
    )
    .expect("codex pins");
    let route = super::NormalizedRoute {
        public_model: "pool-public".into(),
        upstream_model: "pool-public".into(),
        protocol: pin.protocol.clone(),
        endpoint_id: pin.endpoint_id.clone(),
        origin: pin.origin.clone(),
        endpoint_fingerprint: pin.endpoint_fingerprint.clone(),
        validation_only: false,
        native_targets: targets,
    };
    let fingerprint = super::credential_route_fingerprint(
        "codex/token.json",
        "oauth-codex-ref",
        3,
        "bind-codex",
        OAUTH_SHA,
        1,
        &[route.clone()],
    );
    annotated.route_sets.push(super::CredentialRouteSet {
        auth_id: "codex/token.json".into(),
        credential_id: "oauth-codex-ref".into(),
        credential_version: 3,
        binding_id: "bind-codex".into(),
        material_fingerprint: OAUTH_SHA.into(),
        routing_rank: 1,
        routes: vec![route],
        fingerprint: fingerprint.clone(),
    });
    let before_digest = annotated.digest.clone();
    super::refresh_logical_digest(&mut annotated).expect("refresh");
    let with_targets = annotated.digest.clone();
    assert_ne!(with_targets, before_digest);
    annotated.route_sets[0].routes[0].native_targets.clear();
    super::refresh_logical_digest(&mut annotated).expect("cleared targets");
    assert_ne!(annotated.digest, with_targets);
    annotated.route_sets[0].routes[0].native_targets = super::native_route_targets(
        &super::normalize_native_label("codex"),
        "pool-public",
        "chat_completions",
        &owned_native_connection(),
    )
    .expect("restore pins")
    .1;
    super::refresh_logical_digest(&mut annotated).expect("restored targets");
    assert_eq!(annotated.digest, with_targets);
    let rendered = render_canonical_yaml(&annotated).expect("annotated yaml");
    assert_eq!(rendered.wire_digest, super::wire_digest(&rendered.yaml));
    assert!(!rendered.yaml.contains(&rendered.wire_digest));
    assert!(rendered.yaml.contains(&fingerprint));
    let annotated_doc: serde_json::Value = serde_yaml_ng::from_str(&rendered.yaml).unwrap();
    assert_eq!(
        annotated_doc["ocg"]["oauth-bindings"][0]["native-route-fingerprint"],
        serde_json::json!(fingerprint)
    );
    let refreshed_generation = "b".repeat(64);
    let refreshed = [super::OAuthFileRef {
        material_revision: refreshed_generation.clone(),
        ..oauth[0].clone()
    }];
    let refreshed_projection = project_at(
        &fx,
        &config(),
        &refreshed,
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 8,
            applied: 3,
        },
    )
    .expect("reported refresh");
    assert_eq!(
        refreshed_projection.oauth_refs[0].credential_id,
        "oauth-codex-ref"
    );
    assert_eq!(refreshed_projection.oauth_refs[0].credential_version, 3);
    assert_eq!(refreshed_projection.oauth_refs[0].routing_rank, 1);
    assert_eq!(
        refreshed_projection.oauth_refs[0].material_revision,
        refreshed_generation
    );
    assert_ne!(refreshed_projection.digest, projection.digest);
    let refreshed_yaml = render_canonical_yaml(&refreshed_projection).expect("refresh yaml");
    let refreshed_doc: serde_json::Value = serde_yaml_ng::from_str(&refreshed_yaml.yaml).unwrap();
    assert_eq!(
        refreshed_doc["ocg"]["oauth-bindings"][0]["credential-id"],
        serde_json::json!("oauth-codex-ref")
    );
    assert_eq!(
        refreshed_doc["ocg"]["oauth-bindings"][0]["credential-version"],
        serde_json::json!("3")
    );
    assert_eq!(
        refreshed_doc["ocg"]["oauth-bindings"][0]["material-revision"],
        serde_json::json!(refreshed_generation)
    );
    assert_eq!(
        refreshed_doc["ocg"]["oauth-bindings"][0]["priority"],
        serde_json::json!(1)
    );
    assert!(!canonical.yaml.contains(inference));
    assert!(!canonical.yaml.contains(management));
    assert!(!canonical.yaml.contains(OWNED_LISTENER));

    let bad = [super::OAuthFileRef {
        provider: crate::cpa::CpaOAuthProvider::Kimi,
        product_provider_id: "cpa".into(),
        relative_path: "../token.json".into(),
        material_revision: OAUTH_SHA.into(),
        auth_id: "kimi/token.json".into(),
        credential_id: "oauth-kimi-ref".into(),
        credential_version: 1,
        routing_rank: 1,
        models: Vec::new(),
        raw_provider_label: String::new(),
        native_mode: String::new(),
        reported_base: String::new(),
    }];
    let error = project_at(
        &fx,
        &config(),
        &bad,
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("oauth path");
    assert_eq!(error.code, "invalid_oauth_ref");
}

#[test]
fn standard_yaml_covers_single_chat_and_refuses_unmapped_facts() {
    let fx = Fixture::open("yaml");
    disable_zen(&fx);
    let ollama_material = "synthetic-yaml-ollama";
    let custom_material = "synthetic-yaml-custom";
    create_keyed(&fx, "ollama-key", OLLAMA_PROVIDER_ID, ollama_material);
    set_rank(&fx, "ollama-key", 1);
    set_catalog(
        &fx,
        &destination_id_for_builtin(OLLAMA_PROVIDER_ID),
        &[catalog_model("ollama-public", "ollama-upstream")],
    );
    fx.db()
        .create_account_with_contract(
            &account(
                &fx.cipher,
                "custom-key",
                CUSTOM_PROVIDER_ID,
                custom_material,
            ),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "custom-public".into(),
                upstream_model: "custom-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    set_rank(&fx, "custom-key", 2);
    let mut direct = config();
    direct.proxy_mode = ProxyMode::Direct;
    let projection = project_at(
        &fx,
        &direct,
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 5,
            applied: 2,
        },
    )
    .unwrap();
    let rendered: super::RenderedYaml = render_canonical_yaml(&projection).expect("canonical yaml");
    assert_eq!(
        render_standard_yaml(&projection).expect("standard yaml"),
        rendered.yaml
    );
    assert_eq!(rendered.wire_digest, super::wire_digest(&rendered.yaml));
    assert!(!rendered.yaml.contains(&rendered.wire_digest));
    let doc: serde_json::Value = serde_yaml_ng::from_str(&rendered.yaml).unwrap();
    assert_eq!(doc["host"], serde_json::json!("127.0.0.1"));
    assert_eq!(doc["port"], serde_json::json!(8317));
    assert_eq!(doc["auth-dir"], serde_json::json!("owned-auth"));
    assert_eq!(doc["api-keys"], serde_json::json!([HOP_SECRET]));
    assert_eq!(
        doc["remote-management"]["secret-key"],
        serde_json::json!("")
    );
    assert_eq!(
        doc["remote-management"]["allow-remote"],
        serde_json::json!(false)
    );
    assert_eq!(doc["routing"]["strategy"], serde_json::json!("fill-first"));
    assert_eq!(
        doc["openai-compatibility"][0]["priority"],
        serde_json::json!(2)
    );
    assert_eq!(
        doc["openai-compatibility"][1]["priority"],
        serde_json::json!(1)
    );
    assert_ne!(
        doc["openai-compatibility"][0]["name"],
        doc["openai-compatibility"][1]["name"]
    );
    assert_eq!(
        doc["openai-compatibility"][0]["name"],
        doc["ocg"]["credentials"][0]["namespace"]
    );
    assert_eq!(
        doc["ocg"]["credentials"][0]["provider-id"],
        serde_json::json!(OLLAMA_PROVIDER_ID)
    );
    assert_ne!(
        doc["ocg"]["credentials"][0]["provider-id"],
        doc["ocg"]["credentials"][0]["namespace"]
    );
    assert_eq!(
        doc["openai-compatibility"][0]["base-url"],
        serde_json::json!("https://ollama.com/v1")
    );
    assert_eq!(
        doc["openai-compatibility"][0]["api-key-entries"][0]["api-key"],
        serde_json::json!(ollama_material)
    );
    assert_eq!(
        doc["openai-compatibility"][0]["api-key-entries"][0]["proxy-url"],
        serde_json::json!("direct")
    );
    assert_eq!(
        doc["openai-compatibility"][0]["models"][0]["alias"],
        serde_json::json!("ollama-public")
    );
    assert_eq!(
        doc["openai-compatibility"][0]["models"][0]["name"],
        serde_json::json!("ollama-upstream")
    );
    assert_eq!(
        doc["ocg"]["credentials"][0]["routes"][0]["wire"],
        serde_json::json!("ollama-reasoning")
    );
    assert!(!rendered.yaml.contains(GATEWAY_SENTINEL));
    assert!(!format!("{projection:?}").contains(ollama_material));
    let ocg = serde_json::to_string(&doc["ocg"]).unwrap();
    assert!(!ocg.contains(ollama_material));
    assert!(!ocg.contains(custom_material));
    assert!(!ocg.contains(HOP_SECRET));
    assert!(doc["ocg"].get("proxy-list").is_none());

    let mut round = direct.clone();
    round.routing_mode = RoutingMode::RoundRobin;
    let round_projection = project_at(
        &fx,
        &round,
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 5,
            applied: 2,
        },
    )
    .unwrap();
    let round_yaml = render_canonical_yaml(&round_projection).unwrap();
    let round_doc: serde_json::Value = serde_yaml_ng::from_str(&round_yaml.yaml).unwrap();
    assert_eq!(
        round_doc["routing"]["strategy"],
        serde_json::json!("round-robin")
    );
    assert_eq!(
        round_doc["openai-compatibility"][0]["priority"],
        serde_json::json!(1)
    );
    assert_eq!(
        round_doc["openai-compatibility"][1]["priority"],
        serde_json::json!(1)
    );
    assert_eq!(round_projection.auths[0].sequence, 0);
    assert_eq!(round_projection.auths[1].sequence, 1);
    assert_ne!(round_projection.digest, projection.digest);

    let mut sticky = config();
    sticky.routing_mode = RoutingMode::StickyGlobal;
    sticky.conversation_sticky = true;
    sticky.proxy_mode = ProxyMode::List;
    sticky.proxy_url = "http://127.0.0.1:8080".into();
    sticky.proxy_list_direction = ProxyListDirection::Blacklist;
    sticky.proxy_list_models = vec!["ollama-public".into()];
    let sticky_projection = project_at(
        &fx,
        &sticky,
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 5,
            applied: 2,
        },
    )
    .unwrap();
    assert_eq!(sticky_projection.routing_mode, RoutingMode::StickyGlobal);
    assert!(sticky_projection.conversation_sticky);
    assert_eq!(sticky_projection.conversation_ttl_secs, 30 * 60);
    assert_eq!(sticky_projection.proxy_mode, ProxyMode::List);
    assert_eq!(
        sticky_projection.proxy_list_direction,
        ProxyListDirection::Blacklist
    );
    assert_eq!(
        sticky_projection.proxy_list_models,
        vec!["ollama-public".to_string()]
    );
    let sticky_yaml = render_canonical_yaml(&sticky_projection).expect("sticky yaml");
    let sticky_doc: serde_json::Value = serde_yaml_ng::from_str(&sticky_yaml.yaml).unwrap();
    assert_eq!(
        sticky_doc["routing"]["strategy"],
        serde_json::json!("fill-first")
    );
    assert_eq!(
        sticky_doc["ocg"]["routing"]["sticky-global"],
        serde_json::json!(true)
    );
    assert_eq!(
        sticky_doc["ocg"]["routing"]["conversation-sticky"],
        serde_json::json!(true)
    );
    assert_eq!(
        sticky_doc["ocg"]["routing"]["conversation-ttl-seconds"],
        serde_json::json!(1800)
    );
    assert_eq!(
        sticky_doc["ocg"]["proxy-list"]["direction"],
        serde_json::json!("blacklist")
    );
    assert_eq!(
        sticky_doc["ocg"]["proxy-list"]["models"],
        serde_json::json!(["ollama-public"])
    );
    assert_eq!(
        sticky_doc["ocg"]["proxy-list"]["proxy-url"],
        serde_json::json!("http://127.0.0.1:8080")
    );
    assert_eq!(
        sticky_doc["openai-compatibility"][0]["api-key-entries"][0]["proxy-url"],
        serde_json::json!("http://127.0.0.1:8080")
    );
    let sticky_ocg = serde_json::to_string(&sticky_doc["ocg"]).unwrap();
    assert!(!sticky_ocg.contains(ollama_material));
    assert!(!sticky_yaml.yaml.contains(GATEWAY_SENTINEL));

    let second = "synthetic-yaml-ollama-two";
    create_keyed(&fx, "ollama-two", OLLAMA_PROVIDER_ID, second);
    let mut strict = project_fx(&fx);
    let strict_yaml = render_canonical_yaml(&strict).expect("same provider");
    let strict_doc: serde_json::Value = serde_yaml_ng::from_str(&strict_yaml.yaml).unwrap();
    let entries = strict_doc["openai-compatibility"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    let namespaces: Vec<_> = strict_doc["ocg"]["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["namespace"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(namespaces.len(), 3);
    assert_eq!(
        namespaces
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    let ollama_rows: Vec<_> = strict_doc["ocg"]["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["provider-id"] == serde_json::json!(OLLAMA_PROVIDER_ID))
        .collect();
    assert_eq!(ollama_rows.len(), 2);
    assert_ne!(ollama_rows[0]["namespace"], ollama_rows[1]["namespace"]);
    let priorities: Vec<_> = entries
        .iter()
        .map(|item| item["priority"].as_i64().unwrap())
        .collect();
    assert_eq!(
        priorities
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        priorities.len()
    );
    let strict_ocg = serde_json::to_string(&strict_doc["ocg"]).unwrap();
    assert!(!strict_ocg.contains(second));
    assert!(strict_yaml.yaml.contains(second));
    assert!(!strict_yaml.yaml.contains(&strict_yaml.wire_digest));
    strict.auths.push(strict.auths[0].clone());
    let error = render_canonical_yaml(&strict).expect_err("duplicate namespace");
    assert_eq!(error.code, "duplicate_namespace");
    assert!(!error.to_string().contains(second));
}

#[test]
fn selection_tiers_share_round_robin_and_proxy_legs_stay_distinct() {
    let fx = Fixture::open("tiers");
    disable_zen(&fx);
    create_keyed(&fx, "go-key", OPENCODE_PROVIDER_ID, "synthetic-tier-go");
    create_keyed(
        &fx,
        "ollama-key",
        OLLAMA_PROVIDER_ID,
        "synthetic-tier-ollama",
    );
    set_rank(&fx, "go-key", 1);
    set_rank(&fx, "ollama-key", 2);
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(OLLAMA_PROVIDER_ID),
        &[catalog_model("ollama-public", "ollama-upstream")],
    );
    let revisions = ProjectionRevisions {
        desired: 1,
        applied: 0,
    };

    let mut round = config();
    round.routing_mode = RoutingMode::RoundRobin;
    let round_projection = project_at(&fx, &round, &[], &BTreeMap::new(), revisions).unwrap();
    assert_eq!(
        round_projection
            .auths
            .iter()
            .map(|auth| auth.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let round_doc = yaml_value(&round_projection);
    assert_eq!(
        round_doc["routing"]["strategy"],
        serde_json::json!("round-robin")
    );
    assert_eq!(priorities(&round_doc), vec![1, 1]);
    assert_ne!(
        round_doc["openai-compatibility"][0]["name"],
        round_doc["openai-compatibility"][1]["name"]
    );
    assert_eq!(
        round_doc["ocg"]["routing"]["sticky-global"],
        serde_json::json!(false)
    );

    let strict = project_at(&fx, &config(), &[], &BTreeMap::new(), revisions).unwrap();
    let strict_doc = yaml_value(&strict);
    assert_eq!(
        strict_doc["routing"]["strategy"],
        serde_json::json!("fill-first")
    );
    assert_eq!(priorities(&strict_doc), vec![2, 1]);
    assert_eq!(
        strict_doc["ocg"]["routing"]["sticky-global"],
        serde_json::json!(false)
    );
    assert!(strict_doc["ocg"].get("proxy-list").is_none());
    assert!(
        strict_doc["openai-compatibility"][0]["api-key-entries"][0]
            .get("proxy-url")
            .is_none()
    );

    let mut sticky = config();
    sticky.routing_mode = RoutingMode::StickyGlobal;
    let sticky_projection = project_at(&fx, &sticky, &[], &BTreeMap::new(), revisions).unwrap();
    let sticky_doc = yaml_value(&sticky_projection);
    assert_eq!(
        sticky_doc["routing"]["strategy"],
        serde_json::json!("fill-first")
    );
    assert_eq!(priorities(&sticky_doc), vec![2, 1]);
    assert_eq!(
        sticky_doc["ocg"]["routing"]["sticky-global"],
        serde_json::json!(true)
    );

    let mut direct = config();
    direct.proxy_mode = ProxyMode::Direct;
    let direct_doc =
        yaml_value(&project_at(&fx, &direct, &[], &BTreeMap::new(), revisions).unwrap());
    assert_eq!(
        direct_doc["openai-compatibility"][0]["api-key-entries"][0]["proxy-url"],
        serde_json::json!("direct")
    );
    assert!(direct_doc["ocg"].get("proxy-list").is_none());

    let mut manual = config();
    manual.proxy_mode = ProxyMode::Manual;
    manual.proxy_url = "http://127.0.0.1:8080".into();
    let manual_doc =
        yaml_value(&project_at(&fx, &manual, &[], &BTreeMap::new(), revisions).unwrap());
    assert_eq!(
        manual_doc["openai-compatibility"][0]["api-key-entries"][0]["proxy-url"],
        serde_json::json!("http://127.0.0.1:8080")
    );
    assert!(manual_doc["ocg"].get("proxy-list").is_none());

    let mut allow = config();
    allow.proxy_mode = ProxyMode::List;
    allow.proxy_url = "http://127.0.0.1:8080".into();
    allow.proxy_list_direction = ProxyListDirection::Whitelist;
    allow.proxy_list_models = vec!["ollama-public".into()];
    let allow_doc = yaml_value(&project_at(&fx, &allow, &[], &BTreeMap::new(), revisions).unwrap());
    assert_eq!(
        allow_doc["openai-compatibility"][0]["api-key-entries"][0]["proxy-url"],
        serde_json::json!("direct")
    );
    assert_eq!(
        allow_doc["ocg"]["proxy-list"]["direction"],
        serde_json::json!("whitelist")
    );
    assert_eq!(
        allow_doc["ocg"]["proxy-list"]["proxy-url"],
        serde_json::json!("http://127.0.0.1:8080")
    );
    assert_eq!(
        allow_doc["ocg"]["proxy-list"]["models"],
        serde_json::json!(["ollama-public"])
    );
}

fn yaml_value(projection: &super::ProductProjection) -> serde_json::Value {
    let rendered = render_canonical_yaml(projection).expect("yaml");
    serde_yaml_ng::from_str(&rendered.yaml).unwrap()
}

fn priorities(doc: &serde_json::Value) -> Vec<i64> {
    doc["openai-compatibility"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["priority"].as_i64().unwrap())
        .collect()
}

#[test]
fn oauth_rank_shares_the_api_priority_tier() {
    let fx = Fixture::open("oauth-rank");
    disable_zen(&fx);
    create_keyed(
        &fx,
        "go-key",
        OPENCODE_PROVIDER_ID,
        "synthetic-oauth-rank-go",
    );
    set_rank(&fx, "go-key", 5);
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    let revisions = ProjectionRevisions {
        desired: 2,
        applied: 1,
    };
    let reference = super::OAuthFileRef {
        provider: crate::cpa::CpaOAuthProvider::Codex,
        product_provider_id: "cpa".into(),
        relative_path: "codex/token.json".into(),
        material_revision: OAUTH_SHA.into(),
        auth_id: "codex/token.json".into(),
        credential_id: "oauth-codex-ref".into(),
        credential_version: 4,
        routing_rank: 1,
        models: vec!["oauth-alias".into()],
        raw_provider_label: String::new(),
        native_mode: String::new(),
        reported_base: String::new(),
    };
    let strict = project_at(
        &fx,
        &config(),
        &[reference.clone()],
        &BTreeMap::new(),
        revisions,
    )
    .expect("strict mix");
    assert_eq!(strict.auths[0].sequence, 0);
    assert_eq!(strict.auths[0].routing_rank, 5);
    let strict_doc = yaml_value(&strict);
    assert_eq!(
        strict_doc["routing"]["strategy"],
        serde_json::json!("fill-first")
    );
    assert_eq!(priorities(&strict_doc), vec![1]);
    assert_eq!(
        strict_doc["ocg"]["oauth-bindings"][0]["priority"],
        serde_json::json!(2)
    );
    assert_eq!(
        strict_doc["ocg"]["oauth-bindings"][0]["models"],
        serde_json::json!(["oauth-alias"])
    );
    assert_eq!(
        strict_doc["ocg"]["oauth-bindings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let tied = super::OAuthFileRef {
        routing_rank: 5,
        ..reference.clone()
    };
    let tied_doc = yaml_value(
        &project_at(&fx, &config(), &[tied], &BTreeMap::new(), revisions).expect("tied rank"),
    );
    assert_eq!(priorities(&tied_doc), vec![2]);
    assert_eq!(
        tied_doc["ocg"]["oauth-bindings"][0]["priority"],
        serde_json::json!(1)
    );

    let mut round = config();
    round.routing_mode = RoutingMode::RoundRobin;
    let round_doc = yaml_value(
        &project_at(
            &fx,
            &round,
            &[reference.clone()],
            &BTreeMap::new(),
            revisions,
        )
        .expect("round robin"),
    );
    assert_eq!(priorities(&round_doc), vec![1]);
    assert_eq!(
        round_doc["ocg"]["oauth-bindings"][0]["priority"],
        serde_json::json!(1)
    );

    let mut sticky = config();
    sticky.routing_mode = RoutingMode::StickyGlobal;
    let sticky_doc = yaml_value(
        &project_at(
            &fx,
            &sticky,
            &[reference.clone()],
            &BTreeMap::new(),
            revisions,
        )
        .expect("sticky"),
    );
    assert_eq!(
        sticky_doc["routing"]["strategy"],
        serde_json::json!("fill-first")
    );
    assert_eq!(
        sticky_doc["ocg"]["routing"]["sticky-global"],
        serde_json::json!(true)
    );
    assert_eq!(priorities(&sticky_doc), vec![1]);
    assert_eq!(
        sticky_doc["ocg"]["oauth-bindings"][0]["priority"],
        serde_json::json!(2)
    );

    let refreshed = super::OAuthFileRef {
        material_revision: "b".repeat(64),
        ..reference.clone()
    };
    let refreshed_projection =
        project_at(&fx, &config(), &[refreshed], &BTreeMap::new(), revisions).expect("refresh");
    assert_eq!(
        refreshed_projection.oauth_refs[0].credential_id,
        "oauth-codex-ref"
    );
    assert_eq!(refreshed_projection.oauth_refs[0].credential_version, 4);
    assert_eq!(refreshed_projection.oauth_refs[0].routing_rank, 1);
    assert_ne!(refreshed_projection.digest, strict.digest);
    let refreshed_doc = yaml_value(&refreshed_projection);
    assert_eq!(priorities(&refreshed_doc), vec![1]);
    assert_eq!(
        refreshed_doc["ocg"]["oauth-bindings"][0]["priority"],
        serde_json::json!(2)
    );
}

#[test]
fn version_zero_is_rejected_by_the_loader_and_the_builder() {
    let fx = Fixture::open("version");
    disable_zen(&fx);
    let material = "synthetic-version-material";
    create_keyed(&fx, "go-key", OPENCODE_PROVIDER_ID, material);
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    let mut snapshot = RoutingSnapshot::load(fx.db()).unwrap();
    snapshot
        .credentials
        .iter_mut()
        .find(|credential| credential.id == "go-key")
        .expect("go credential")
        .credential_version = 0;
    let error = project(ProjectionInput {
        snapshot: &snapshot,
        config: &config(),
        cipher: &fx.cipher,
        revisions: ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
        owned_listener: OWNED_LISTENER,
        owned_origins: &[],
        owned_destination_ids: &[],
        oauth_refs: &[],
        preset_ids: &BTreeMap::new(),
        validation: super::ValidationSidecar::none(),
        runtime: runtime(),
    })
    .expect_err("builder fence");
    assert_eq!(error.code, "invalid_version");
    assert!(!error.to_string().contains(material));

    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET credential_version = 0 WHERE legacy_account_id = 'go-key'",
            [],
        )
        .unwrap();
    let loaded = RoutingSnapshot::load(fx.db());
    assert!(loaded.is_err());
    let rendered = format!("{loaded:?}");
    assert!(!rendered.contains(material));
}

/// Product builds refuse a stored loopback. This feature is the existing seam:
/// `cargo test -p ocg-core --lib cpa_projection --locked --features ollama-cloud-loopback-test`.
/// It accepts an HTTP loopback origin only. HTTPS loopback stays refused, and
/// TLS verification is unchanged.
#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn http_loopback_feature_keeps_the_stored_origin() {
    let fx = Fixture::open("ollama-loopback-feature");
    disable_zen(&fx);
    let material = "synthetic-ollama-loopback";
    create_keyed(&fx, "ollama-key", OLLAMA_PROVIDER_ID, material);
    let destination_id = destination_id_for_builtin(OLLAMA_PROVIDER_ID);
    set_catalog(
        &fx,
        &destination_id,
        &[catalog_model("ollama-public", "ollama-upstream")],
    );
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET base_url = 'http://127.0.0.1:9' WHERE id = ?1",
            [&destination_id],
        )
        .unwrap();
    let projection = project_fx(&fx);
    assert_route(
        auth(&projection, "ollama-key"),
        "chat_completions",
        "http://127.0.0.1:9/v1/chat/completions",
        "bearer",
    );
    let canonical = render_canonical_yaml(&projection).unwrap();
    assert!(
        canonical
            .yaml
            .contains("http://127.0.0.1:9/v1/chat/completions")
    );
    assert!(!canonical.yaml.contains("ollama.com"));
    assert!(!canonical.yaml.contains(&canonical.wire_digest));

    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET base_url = 'https://127.0.0.1:9' WHERE id = ?1",
            [&destination_id],
        )
        .unwrap();
    let error = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("https loopback");
    assert_eq!(error.code, "invalid_endpoint");
    assert!(!error.to_string().contains("ollama.com"));
    assert!(!error.to_string().contains(material));
}

fn credential_identity(fx: &Fixture, legacy: &str) -> (String, u64) {
    fx.db()
        .conn
        .query_row(
            "SELECT id, credential_version FROM credentials WHERE legacy_account_id = ?1",
            [legacy],
            |row| {
                let version: i64 = row.get(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    u64::try_from(version).unwrap_or(0),
                ))
            },
        )
        .unwrap()
}

fn candidate(
    fx: &Fixture,
    legacy: &str,
    kind: super::ValidationCandidateKind,
    setup_step: &str,
    account_type: &str,
    material_present: bool,
    destination_draft: bool,
    pending_routes: Vec<super::PendingApprovedRoute>,
) -> super::ValidationCandidate {
    let (credential_id, credential_version) = credential_identity(fx, legacy);
    super::ValidationCandidate {
        credential_id,
        credential_version,
        kind,
        setup_step: setup_step.into(),
        account_type: account_type.into(),
        material_present,
        destination_draft,
        pending_routes,
    }
}

fn project_with(
    fx: &Fixture,
    candidates: &[super::ValidationCandidate],
) -> Result<super::ProductProjection, super::ProjectionError> {
    let snapshot = RoutingSnapshot::load(fx.db()).expect("snapshot");
    project(ProjectionInput {
        snapshot: &snapshot,
        config: &config(),
        cipher: &fx.cipher,
        revisions: ProjectionRevisions {
            desired: 4,
            applied: 1,
        },
        owned_listener: OWNED_LISTENER,
        owned_origins: &[],
        owned_destination_ids: &[],
        oauth_refs: &[],
        preset_ids: &BTreeMap::new(),
        validation: super::ValidationSidecar { candidates },
        runtime: runtime(),
    })
}

fn route_set<'a>(
    projection: &'a super::ProductProjection,
    credential_id: &str,
) -> &'a super::CredentialRouteSet {
    projection
        .route_sets
        .iter()
        .find(|set| set.credential_id == credential_id)
        .unwrap_or_else(|| panic!("missing route set {credential_id}"))
}

fn hex64(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

#[test]
fn raw_goat_catalog_publishes_the_curated_alias_on_the_same_auth() {
    let fx = Fixture::open("goat-raw");
    disable_zen(&fx);
    create_keyed(
        &fx,
        "goat-raw",
        COMMAND_CODE_PROVIDER_ID,
        "synthetic-goat-raw",
    );
    let raw = COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM;
    let alias = COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS;
    set_catalog(
        &fx,
        &destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID),
        &[catalog_model(raw, raw)],
    );
    let projection = project_fx(&fx);
    assert_eq!(projection.auths.len(), 1);
    assert_eq!(projection.route_sets.len(), 1);
    let goat = auth(&projection, "goat-raw");
    assert_eq!(
        goat.models
            .iter()
            .map(|model| model.public_alias.as_str())
            .collect::<Vec<_>>(),
        vec![raw, alias]
    );
    assert!(goat.models.iter().all(|model| model.upstream_name == raw));
    assert_eq!(goat.models[0].routes, goat.models[1].routes);
    assert!(
        goat.models
            .iter()
            .all(|model| { model.routes.iter().all(|route| !route.validation_only) })
    );
    let set = route_set(&projection, &goat.credential_id);
    assert_eq!(
        set.routes.len(),
        goat.models.len() * goat.models[0].routes.len()
    );
    assert!(
        set.routes
            .iter()
            .all(|route| { !route.validation_only && route.origin.contains("api.commandcode.ai") })
    );
    let goat_alias = goat.models[0].public_alias.clone();
    for model_route in &goat.models[0].routes {
        let normalized = set
            .routes
            .iter()
            .find(|route| {
                route.public_model == goat_alias && route.protocol == model_route.protocol
            })
            .unwrap();
        assert_eq!(
            normalized.endpoint_fingerprint,
            super::endpoint_fingerprint(&model_route.endpoint_url)
        );
        assert!(hex64(&normalized.endpoint_fingerprint));
        assert!(
            !normalized
                .endpoint_fingerprint
                .contains(&model_route.endpoint_url)
        );
        assert!(!set.fingerprint.contains(&model_route.endpoint_url));
    }
    assert!(hex64(&set.fingerprint));
    assert!(!set.fingerprint.contains("synthetic-goat-raw"));
    let again = project_fx(&fx);
    assert_eq!(
        route_set(&again, &goat.credential_id).fingerprint,
        set.fingerprint
    );
    let doc = yaml_value(&projection);
    let aliases: Vec<_> = doc["openai-compatibility"][0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["alias"].as_str().unwrap())
        .collect();
    assert_eq!(aliases, vec![raw, alias]);
    let routes = doc["ocg"]["credentials"][0]["routes"].as_array().unwrap();
    assert!(
        routes
            .iter()
            .all(|route| route.get("validation-only").is_none())
    );
    assert_eq!(doc["openai-compatibility"].as_array().unwrap().len(), 1);
}

#[test]
fn alias_scope_stays_on_one_auth_and_rejects_unknown_or_ambiguous_names() {
    let raw = COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM;
    let alias = COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS;

    let fx = Fixture::open("goat-alias-scope");
    disable_zen(&fx);
    create_keyed(
        &fx,
        "goat-alias",
        COMMAND_CODE_PROVIDER_ID,
        "synthetic-goat-alias-scope",
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID),
        &[catalog_model(raw, raw)],
    );
    let binding = binding_of(&fx, "goat-alias");
    fx.db()
        .update_credential_binding(
            &binding,
            Some(&ModelScope::Only {
                models: vec![alias.into()],
            }),
            None,
            None,
            None,
        )
        .unwrap();
    let projection = project_fx(&fx);
    assert_eq!(projection.auths.len(), 1);
    let goat = auth(&projection, "goat-alias");
    assert_eq!(goat.models.len(), 1);
    assert_eq!(goat.models[0].public_alias, alias);
    assert_eq!(goat.models[0].upstream_name, raw);

    fx.db()
        .update_credential_binding(
            &binding,
            Some(&ModelScope::Only {
                models: vec![raw.into()],
            }),
            None,
            None,
            None,
        )
        .unwrap();
    set_catalog(
        &fx,
        &destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID),
        &[catalog_model(alias, raw)],
    );
    let projection = project_fx(&fx);
    let goat = auth(&projection, "goat-alias");
    assert_eq!(projection.auths.len(), 1);
    assert_eq!(goat.models.len(), 1);
    assert_eq!(goat.models[0].public_alias, raw);
    assert_eq!(goat.models[0].upstream_name, raw);

    fx.db()
        .update_credential_binding(
            &binding,
            Some(&ModelScope::Only {
                models: vec!["missing-goat-model".into()],
            }),
            None,
            None,
            None,
        )
        .unwrap();
    let error = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("unknown scope");
    assert_eq!(error.code, "invalid_scope");

    let fx = Fixture::open("goat-no-alias");
    disable_zen(&fx);
    create_keyed(
        &fx,
        "goat-other",
        COMMAND_CODE_PROVIDER_ID,
        "synthetic-goat-other",
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID),
        &[catalog_model("other-public", "other-upstream")],
    );
    let projection = project_fx(&fx);
    assert!(
        projection.auths[0]
            .models
            .iter()
            .all(|model| model.public_alias != alias)
    );

    let fx = Fixture::open("goat-ambiguous");
    disable_zen(&fx);
    create_keyed(
        &fx,
        "goat-clash",
        COMMAND_CODE_PROVIDER_ID,
        "synthetic-goat-clash",
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID),
        &[
            catalog_model(alias, "other-upstream"),
            catalog_model(raw, raw),
        ],
    );
    let error = project_at(
        &fx,
        &config(),
        &[],
        &BTreeMap::new(),
        ProjectionRevisions {
            desired: 1,
            applied: 0,
        },
    )
    .expect_err("ambiguous alias");
    assert_eq!(error.code, "invalid_alias");
    assert!(
        error
            .to_string()
            .contains("one public alias maps to two upstream names")
    );

    let fx = Fixture::open("custom-no-alias");
    disable_zen(&fx);
    fx.db()
        .create_account_with_contract(
            &account(
                &fx.cipher,
                "custom-one",
                CUSTOM_PROVIDER_ID,
                "synthetic-custom-one",
            ),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "custom-public".into(),
                upstream_model: "custom-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    set_catalog(
        &fx,
        &destination_id_for_custom_account("custom-one"),
        &[catalog_model("custom-public", "custom-upstream")],
    );
    let projection = project_fx(&fx);
    assert_eq!(projection.auths.len(), 1);
    assert_eq!(projection.auths[0].models.len(), 1);
    assert_eq!(projection.auths[0].models[0].public_alias, "custom-public");
    assert_eq!(
        projection.auths[0].models[0].upstream_name,
        "custom-upstream"
    );
}

#[test]
fn pending_alternate_stays_on_the_client_auth() {
    let fx = Fixture::open("pending-protocol");
    disable_zen(&fx);
    let material = "synthetic-pending-material";
    fx.db()
        .create_account_with_contract(
            &account(&fx.cipher, "pending-key", CUSTOM_PROVIDER_ID, material),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "chat-public".into(),
                upstream_model: "chat-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    let destination_id = destination_id_for_custom_account("pending-key");
    let routes = vec![
        HttpProtocolRoute {
            protocol: UpstreamProtocolKind::ChatCompletions,
            endpoint_url: "https://lab.example/v1/chat/completions".into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: UpstreamProtocolKind::Responses,
            endpoint_url: "https://lab.example/v1/responses".into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let protocols: Vec<_> = routes.iter().map(|route| route.protocol).collect();
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3, base_url = ?4 WHERE id = ?1",
            rusqlite::params![
                &destination_id,
                serde_json::to_string(&routes).unwrap(),
                serde_json::to_string(&protocols).unwrap(),
                routes[0].endpoint_url,
            ],
        )
        .unwrap();
    set_catalog(
        &fx,
        &destination_id,
        &[CatalogModel {
            public_model: "chat-public".into(),
            upstream_model: "chat-upstream".into(),
            protocols: vec![UpstreamProtocolKind::ChatCompletions],
            preferred: Some(UpstreamProtocolKind::ChatCompletions),
            enabled: true,
            upstream_override: None,
        }],
    );
    let pending = vec![super::PendingApprovedRoute {
        public_model: "chat-public".into(),
        upstream_model: "chat-upstream".into(),
        protocol: "responses".into(),
    }];
    let pending_candidate = candidate(
        &fx,
        "pending-key",
        super::ValidationCandidateKind::CompletePending,
        "ready",
        "key",
        true,
        false,
        pending,
    );
    let withheld = project_with(&fx, &[pending_candidate.clone()]).expect("withheld grant");
    assert_eq!(withheld.auths.len(), 1);
    assert_eq!(withheld.auths[0].models[0].routes.len(), 1);
    assert!(!withheld.auths[0].models[0].routes[0].validation_only);
    assert!(withheld.omissions.iter().any(|item| {
        item.reason == "unapproved_grant" && item.model.as_deref() == Some("chat-public")
    }));
    let client_only = project_fx(&fx);
    grant_operation(&fx, "pending-key", EndpointOperation::ResponseCreate);
    grant_route(
        &fx,
        "pending-key",
        EndpointOperation::ResponseCreate,
        "https://lab.example/v1/responses",
    );
    let pending_candidate = candidate(
        &fx,
        "pending-key",
        super::ValidationCandidateKind::CompletePending,
        "ready",
        "key",
        true,
        false,
        vec![super::PendingApprovedRoute {
            public_model: "chat-public".into(),
            upstream_model: "chat-upstream".into(),
            protocol: "responses".into(),
        }],
    );
    let admitted = project_with(&fx, &[pending_candidate]).expect("granted pending");
    assert_eq!(admitted.auths.len(), 1);
    let model = &admitted.auths[0].models[0];
    assert_eq!(model.routes.len(), 2);
    assert!(!model.routes[0].validation_only);
    assert_eq!(model.routes[0].protocol, "chat_completions");
    assert!(model.routes[1].validation_only);
    assert_eq!(model.routes[1].protocol, "responses");
    assert_ne!(admitted.digest, client_only.digest);
    assert_ne!(
        route_set(&admitted, &admitted.auths[0].credential_id).fingerprint,
        route_set(&client_only, &client_only.auths[0].credential_id).fingerprint
    );
    let doc = yaml_value(&admitted);
    let yaml_routes = doc["ocg"]["credentials"][0]["routes"].as_array().unwrap();
    assert!(yaml_routes.iter().any(|route| {
        route["protocol"].as_str() == Some("chat_completions")
            && route.get("validation-only").is_none()
    }));
    assert!(yaml_routes.iter().any(|route| {
        route["protocol"].as_str() == Some("responses")
            && route["validation-only"] == serde_json::json!(true)
    }));

    let invalid = candidate(
        &fx,
        "pending-key",
        super::ValidationCandidateKind::CompletePending,
        "ready",
        "key",
        true,
        false,
        vec![super::PendingApprovedRoute {
            public_model: "chat-public".into(),
            upstream_model: "chat-upstream".into(),
            protocol: "not-a-protocol".into(),
        }],
    );
    let error = project_with(&fx, &[invalid]).expect_err("unknown protocol");
    assert_eq!(error.code, "unsupported_protocol");
    assert!(!error.to_string().contains(material));
}

#[test]
fn managed_key_verification_is_validation_only() {
    let fx = Fixture::open("managed-validation");
    disable_zen(&fx);
    create_keyed(
        &fx,
        "managed-key",
        OPENCODE_PROVIDER_ID,
        "synthetic-managed-material",
    );
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0, setup_step = 'key_verification', account_type = 'managed'
             WHERE legacy_account_id = 'managed-key'",
            [],
        )
        .unwrap();
    let pending = candidate(
        &fx,
        "managed-key",
        super::ValidationCandidateKind::ManagedKeyVerification,
        "key_verification",
        "managed",
        true,
        false,
        Vec::new(),
    );
    let projection = project_with(&fx, &[pending]).expect("managed candidate");
    assert_eq!(projection.auths.len(), 1);
    let managed = auth(&projection, "managed-key");
    assert!(!managed.models.is_empty());
    assert!(
        managed
            .models
            .iter()
            .all(|model| { model.routes.iter().all(|route| route.validation_only) })
    );
    assert!(!projection.omissions.iter().any(|item| {
        item.credential_id == managed.credential_id && item.reason == "credential_disabled"
    }));
    let doc = yaml_value(&projection);
    let routes = doc["ocg"]["credentials"][0]["routes"].as_array().unwrap();
    assert!(
        routes
            .iter()
            .all(|route| route["validation-only"] == serde_json::json!(true))
    );
}

#[test]
fn validation_sidecar_cannot_revive_blocked_rows() {
    let fx = Fixture::open("blocked-validation");
    disable_zen(&fx);
    let material = "synthetic-blocked-material";
    for id in [
        "off-ready",
        "payment-key",
        "bind-key",
        "unrelated-key",
        "empty-key",
    ] {
        create_keyed(&fx, id, OPENCODE_PROVIDER_ID, material);
    }
    set_catalog(
        &fx,
        &destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        &[catalog_model("go-public", "go-upstream")],
    );
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0 WHERE legacy_account_id = 'off-ready'",
            [],
        )
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET setup_step = 'payment' WHERE legacy_account_id = 'payment-key'",
            [],
        )
        .unwrap();
    let binding = binding_of(&fx, "bind-key");
    fx.db()
        .update_credential_binding(&binding, None, Some(false), None, None)
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0, setup_step = 'key_verification', account_type = 'key'
             WHERE legacy_account_id = 'unrelated-key'",
            [],
        )
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE credentials SET enabled = 0, setup_step = 'key_verification', account_type = 'managed', key_cipher = ''
             WHERE legacy_account_id = 'empty-key'",
            [],
        )
        .unwrap();
    fx.db()
        .create_account_with_contract(
            &account(&fx.cipher, "dest-off", CUSTOM_PROVIDER_ID, material),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "custom-public".into(),
                upstream_model: "custom-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET enabled = 0 WHERE id = ?1",
            [destination_id_for_custom_account("dest-off")],
        )
        .unwrap();
    let candidates = vec![
        candidate(
            &fx,
            "off-ready",
            super::ValidationCandidateKind::CompletePending,
            "ready",
            "key",
            true,
            false,
            Vec::new(),
        ),
        candidate(
            &fx,
            "payment-key",
            super::ValidationCandidateKind::CompletePending,
            "payment",
            "key",
            true,
            false,
            Vec::new(),
        ),
        candidate(
            &fx,
            "bind-key",
            super::ValidationCandidateKind::CompletePending,
            "ready",
            "key",
            true,
            false,
            Vec::new(),
        ),
        candidate(
            &fx,
            "unrelated-key",
            super::ValidationCandidateKind::ManagedKeyVerification,
            "key_verification",
            "key",
            true,
            false,
            Vec::new(),
        ),
        candidate(
            &fx,
            "empty-key",
            super::ValidationCandidateKind::ManagedKeyVerification,
            "key_verification",
            "managed",
            true,
            false,
            Vec::new(),
        ),
        candidate(
            &fx,
            "dest-off",
            super::ValidationCandidateKind::CompletePending,
            "ready",
            "key",
            true,
            false,
            Vec::new(),
        ),
    ];
    let projection = project_with(&fx, &candidates).expect("blocked rows");
    for id in [
        "off-ready",
        "payment-key",
        "bind-key",
        "unrelated-key",
        "empty-key",
        "dest-off",
    ] {
        assert!(
            projection
                .auths
                .iter()
                .all(|auth| auth.legacy_account_id != id),
            "{id} was projected"
        );
    }
    for reason in [
        "credential_disabled",
        "draft",
        "binding_disabled",
        "destination_disabled",
    ] {
        assert!(
            projection
                .omissions
                .iter()
                .any(|item| item.reason == reason),
            "missing {reason}"
        );
    }
}

#[test]
fn draft_destination_is_validation_only() {
    let fx = Fixture::open("draft-destination");
    disable_zen(&fx);
    fx.db()
        .create_account_with_contract(
            &account(
                &fx.cipher,
                "draft-custom",
                CUSTOM_PROVIDER_ID,
                "synthetic-draft-material",
            ),
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://lab.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "draft-public".into(),
                upstream_model: "draft-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    let destination_id = destination_id_for_custom_account("draft-custom");
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET onboarding_draft = 1 WHERE id = ?1",
            [destination_id],
        )
        .unwrap();
    let omitted = project_fx(&fx);
    assert!(
        omitted
            .auths
            .iter()
            .all(|auth| auth.legacy_account_id != "draft-custom")
    );
    assert!(omitted.omissions.iter().any(|item| item.reason == "draft"));
    let pending = candidate(
        &fx,
        "draft-custom",
        super::ValidationCandidateKind::CompletePending,
        "ready",
        "key",
        true,
        true,
        Vec::new(),
    );
    let projection = project_with(&fx, &[pending]).expect("draft candidate");
    let draft = auth(&projection, "draft-custom");
    assert!(
        draft
            .models
            .iter()
            .all(|model| { model.routes.iter().all(|route| route.validation_only) })
    );
    let doc = yaml_value(&projection);
    assert!(
        doc["ocg"]["credentials"][0]["routes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|route| route["validation-only"] == serde_json::json!(true))
    );
}

#[test]
fn keyless_http_pending_sidecar_projects_without_a_key() {
    let fx = Fixture::open("keyless-http");
    disable_zen(&fx);
    let endpoint = "https://lab.example/v1/chat/completions";
    let legacy = "keyless-http";
    fx.db()
        .create_account_with_contract(
            &account(
                &fx.cipher,
                legacy,
                CUSTOM_PROVIDER_ID,
                "synthetic-replaced-material",
            ),
            Some(&AccountCustomConfigInput {
                endpoint_url: endpoint.into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "keyless-public".into(),
                upstream_model: "keyless-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    store_exact_http_route(&fx, legacy, endpoint, AuthScheme::None);
    clear_key(&fx, legacy);

    let ready = project_fx(&fx);
    let client = auth(&ready, legacy);
    assert!(client.material.is_none());
    assert_route(
        client,
        "chat_completions",
        endpoint,
        AuthScheme::None.as_str(),
    );
    assert!(
        client
            .models
            .iter()
            .all(|model| { model.routes.iter().all(|route| !route.validation_only) })
    );
    assert_eq!(
        route_set(&ready, &client.credential_id).material_fingerprint,
        "no-material"
    );

    let destination_id = destination_id_for_custom_account(legacy);
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET onboarding_draft = 1 WHERE id = ?1",
            [destination_id],
        )
        .unwrap();
    let omitted = project_fx(&fx);
    assert!(
        omitted
            .auths
            .iter()
            .all(|item| item.legacy_account_id != legacy)
    );
    assert!(omitted.omissions.iter().any(|item| item.reason == "draft"));

    let pending = candidate(
        &fx,
        legacy,
        super::ValidationCandidateKind::CompletePending,
        "ready",
        "key",
        false,
        true,
        Vec::new(),
    );
    let projection = project_with(&fx, &[pending]).expect("keyless sidecar");
    let draft = auth(&projection, legacy);
    assert!(draft.material.is_none());
    assert_route(draft, "chat_completions", endpoint, "none");
    assert!(draft.models.iter().all(|model| {
        model
            .routes
            .iter()
            .all(|route| route.validation_only && route.auth == "none")
    }));
    assert_eq!(
        route_set(&projection, &draft.credential_id).material_fingerprint,
        "no-material"
    );
    let doc = yaml_value(&projection);
    assert_eq!(
        doc["openai-compatibility"][0]["api-key-entries"][0]["api-key"],
        serde_json::json!("")
    );
    assert!(
        doc["ocg"]["credentials"][0]["routes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|route| {
                route["validation-only"] == serde_json::json!(true)
                    && route["auth-scheme"] == serde_json::json!("none")
            })
    );
    let rendered = format!("{projection:?}");
    assert!(!rendered.contains("synthetic-replaced-material"));
}

#[test]
fn keyed_http_pending_sidecar_refuses_missing_material() {
    let fx = Fixture::open("keyed-http-empty");
    disable_zen(&fx);
    let endpoint = "https://lab.example/v1/chat/completions";
    let legacy = "keyed-http";
    fx.db()
        .create_account_with_contract(
            &account(
                &fx.cipher,
                legacy,
                CUSTOM_PROVIDER_ID,
                "synthetic-keyed-material",
            ),
            Some(&AccountCustomConfigInput {
                endpoint_url: endpoint.into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "keyed-public".into(),
                upstream_model: "keyed-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    store_exact_http_route(&fx, legacy, endpoint, AuthScheme::Bearer);
    clear_key(&fx, legacy);

    let ready = project_fx(&fx);
    assert!(
        ready
            .auths
            .iter()
            .all(|item| item.legacy_account_id != legacy)
    );
    assert!(
        ready
            .omissions
            .iter()
            .any(|item| item.reason == "missing_material")
    );

    let destination_id = destination_id_for_custom_account(legacy);
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET onboarding_draft = 1 WHERE id = ?1",
            [destination_id],
        )
        .unwrap();
    let pending = candidate(
        &fx,
        legacy,
        super::ValidationCandidateKind::CompletePending,
        "ready",
        "key",
        true,
        true,
        Vec::new(),
    );
    let projection = project_with(&fx, &[pending]).expect("keyed empty sidecar");
    assert!(
        projection
            .auths
            .iter()
            .all(|item| item.legacy_account_id != legacy)
    );
    assert!(
        projection
            .omissions
            .iter()
            .any(|item| item.reason == "draft")
    );
    let rendered = format!("{projection:?}");
    assert!(!rendered.contains("synthetic-keyed-material"));
}

fn admission_row(
    enabled: bool,
    binding_enabled: bool,
    setup_step: &str,
    account_type: &str,
    material_present: bool,
    destination_enabled: bool,
    destination_draft: bool,
) -> super::ValidationRow {
    super::ValidationRow {
        credential_id: "cred".into(),
        credential_version: 1,
        enabled,
        binding_enabled,
        setup_step: setup_step.into(),
        account_type: account_type.into(),
        material_present,
        destination_enabled,
        destination_draft,
        registration_complete: super::registration_is_complete(account_type, setup_step),
    }
}

#[test]
fn classify_validation_and_native_inclusion_follow_the_admission_table() {
    assert!(super::registration_is_complete("key", ""));
    assert!(super::registration_is_complete("key", "ready"));
    assert!(super::registration_is_complete(
        "managed",
        "key_verification"
    ));
    assert!(!super::registration_is_complete("key", "key_verification"));
    assert!(!super::registration_is_complete("key", "payment"));
    assert!(!super::registration_is_complete(
        "managed",
        "google_account"
    ));

    let managed = admission_row(
        false,
        true,
        "key_verification",
        "managed",
        true,
        true,
        false,
    );
    assert_eq!(
        super::classify_validation(&managed),
        Some(super::ValidationCandidateKind::ManagedKeyVerification)
    );
    assert_eq!(
        super::native_inclusion(&managed),
        Some(super::NativeInclusion::ValidationOnly)
    );

    let ready = admission_row(true, true, "ready", "key", true, true, false);
    assert_eq!(
        super::classify_validation(&ready),
        Some(super::ValidationCandidateKind::CompletePending)
    );
    assert_eq!(
        super::native_inclusion(&ready),
        Some(super::NativeInclusion::Client)
    );
    let blank = admission_row(true, true, "", "key", true, true, false);
    assert_eq!(
        super::classify_validation(&blank),
        Some(super::ValidationCandidateKind::CompletePending)
    );
    assert_eq!(
        super::native_inclusion(&blank),
        Some(super::NativeInclusion::Client)
    );

    let draft = admission_row(true, true, "ready", "key", true, true, true);
    assert_eq!(
        super::classify_validation(&draft),
        Some(super::ValidationCandidateKind::CompletePending)
    );
    assert_eq!(
        super::native_inclusion(&draft),
        Some(super::NativeInclusion::ValidationOnly)
    );

    let keyless_client = admission_row(true, true, "ready", "key", false, true, false);
    assert_eq!(super::classify_validation(&keyless_client), None);
    assert_eq!(
        super::native_inclusion(&keyless_client),
        Some(super::NativeInclusion::Client)
    );
    let keyless_draft = admission_row(true, true, "ready", "key", false, true, true);
    assert_eq!(super::classify_validation(&keyless_draft), None);
    assert_eq!(super::native_inclusion(&keyless_draft), None);

    for (step, account) in [
        ("google_account", "key"),
        ("opencode_registration", "key"),
        ("payment", "key"),
        ("key_verification", "key"),
        ("key_verification", "managed"),
    ] {
        let enabled = step != "key_verification" || account == "managed";
        let row = admission_row(enabled, true, step, account, true, true, false);
        if step == "key_verification" && account == "managed" {
            continue;
        }
        assert_eq!(super::classify_validation(&row), None, "{account} {step}");
        assert_eq!(super::native_inclusion(&row), None, "{account} {step}");
    }
    let enabled_key_verification =
        admission_row(true, true, "key_verification", "key", true, true, false);
    assert_eq!(super::classify_validation(&enabled_key_verification), None);
    assert_eq!(super::native_inclusion(&enabled_key_verification), None);
    let enabled_managed =
        admission_row(true, true, "key_verification", "managed", true, true, false);
    assert_eq!(super::classify_validation(&enabled_managed), None);
    assert_eq!(super::native_inclusion(&enabled_managed), None);

    let disabled_ready = admission_row(false, true, "ready", "key", true, true, false);
    assert_eq!(super::classify_validation(&disabled_ready), None);
    assert_eq!(super::native_inclusion(&disabled_ready), None);
    let unbound = admission_row(true, false, "ready", "key", true, true, false);
    assert_eq!(super::classify_validation(&unbound), None);
    assert_eq!(super::native_inclusion(&unbound), None);
    let destination_off = admission_row(true, true, "ready", "key", true, false, false);
    assert_eq!(super::classify_validation(&destination_off), None);
    assert_eq!(super::native_inclusion(&destination_off), None);
    let drafted_managed =
        admission_row(false, true, "key_verification", "managed", true, true, true);
    assert_eq!(super::classify_validation(&drafted_managed), None);
    assert_eq!(super::native_inclusion(&drafted_managed), None);
    let mut registration_off = admission_row(true, true, "ready", "key", true, true, false);
    registration_off.registration_complete = false;
    assert_eq!(super::classify_validation(&registration_off), None);
    assert_eq!(
        super::native_inclusion(&registration_off),
        Some(super::NativeInclusion::Client)
    );
}

#[test]
fn classify_validation_with_auth_accepts_satisfied_keyless_rows_and_refuses_unsatisfied_keyed_rows()
{
    let keyless = admission_row(true, true, "ready", "key", false, true, false);
    assert_eq!(super::classify_validation(&keyless), None);
    assert_eq!(
        super::classify_validation_with_auth(&keyless, true),
        Some(super::ValidationCandidateKind::CompletePending)
    );
    assert_eq!(
        super::native_inclusion(&keyless),
        Some(super::NativeInclusion::Client)
    );
    let keyless_draft = admission_row(true, true, "ready", "key", false, true, true);
    assert_eq!(super::classify_validation(&keyless_draft), None);
    assert_eq!(
        super::classify_validation_with_auth(&keyless_draft, true),
        Some(super::ValidationCandidateKind::CompletePending)
    );
    assert_eq!(super::native_inclusion(&keyless_draft), None);

    let keyed = admission_row(true, true, "ready", "key", true, true, false);
    assert_eq!(super::classify_validation_with_auth(&keyed, false), None);
    assert_eq!(
        super::classify_validation_with_auth(&keyed, true),
        Some(super::ValidationCandidateKind::CompletePending)
    );

    let managed = admission_row(
        false,
        true,
        "key_verification",
        "managed",
        false,
        true,
        false,
    );
    assert_eq!(super::classify_validation(&managed), None);
    assert_eq!(
        super::classify_validation_with_auth(&managed, true),
        Some(super::ValidationCandidateKind::ManagedKeyVerification)
    );
    let enabled_managed = admission_row(
        true,
        true,
        "key_verification",
        "managed",
        false,
        true,
        false,
    );
    assert_eq!(
        super::classify_validation_with_auth(&enabled_managed, true),
        None
    );
    let drafted_managed = admission_row(
        false,
        true,
        "key_verification",
        "managed",
        false,
        true,
        true,
    );
    assert_eq!(
        super::classify_validation_with_auth(&drafted_managed, true),
        None
    );

    let unbound = admission_row(true, false, "ready", "key", false, true, false);
    assert_eq!(super::classify_validation_with_auth(&unbound, true), None);
    let destination_off = admission_row(true, true, "ready", "key", false, false, false);
    assert_eq!(
        super::classify_validation_with_auth(&destination_off, true),
        None
    );
    let payment = admission_row(true, true, "payment", "key", false, true, false);
    assert_eq!(super::classify_validation_with_auth(&payment, true), None);
    let disabled_ready = admission_row(false, true, "ready", "key", false, true, false);
    assert_eq!(
        super::classify_validation_with_auth(&disabled_ready, true),
        None
    );
    let mut registration_off = admission_row(true, true, "ready", "key", false, true, false);
    registration_off.registration_complete = false;
    assert_eq!(
        super::classify_validation_with_auth(&registration_off, true),
        None
    );

    for row in [
        keyless,
        keyless_draft,
        keyed,
        managed,
        enabled_managed,
        drafted_managed,
        unbound,
        destination_off,
        payment,
        disabled_ready,
        registration_off,
    ] {
        assert_eq!(
            super::classify_validation(&row),
            super::classify_validation_with_auth(&row, row.material_present)
        );
    }
}

#[test]
fn same_origin_query_change_changes_endpoint_fingerprint() {
    let fx = Fixture::open("endpoint-fingerprint");
    disable_zen(&fx);
    let original = "https://lab.example/v1/chat/completions";
    let changed = "https://lab.example/v1/chat/completions?probe=1";
    fx.db()
        .create_account_with_contract(
            &account(
                &fx.cipher,
                "query-key",
                CUSTOM_PROVIDER_ID,
                "synthetic-query-material",
            ),
            Some(&AccountCustomConfigInput {
                endpoint_url: original.into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "chat-public".into(),
                upstream_model: "chat-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    let destination_id = destination_id_for_custom_account("query-key");
    set_catalog(
        &fx,
        &destination_id,
        &[catalog_model("chat-public", "chat-upstream")],
    );
    let first = project_fx(&fx);
    let first_auth = auth(&first, "query-key");
    let first_url = first_auth.models[0].routes[0].endpoint_url.clone();
    let first_set = route_set(&first, &first_auth.credential_id);
    let first_route = &first_set.routes[0];
    assert_eq!(
        first_route.endpoint_fingerprint,
        super::endpoint_fingerprint(&first_url)
    );
    assert!(hex64(&first_route.endpoint_fingerprint));
    assert!(!first_set.fingerprint.contains(original));
    assert!(!first_route.endpoint_fingerprint.contains(original));

    let routes = vec![HttpProtocolRoute {
        protocol: UpstreamProtocolKind::ChatCompletions,
        endpoint_url: changed.into(),
        auth_scheme: AuthScheme::Bearer,
    }];
    let protocols = vec![UpstreamProtocolKind::ChatCompletions];
    fx.db()
        .conn
        .execute(
            "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3, base_url = ?4 WHERE id = ?1",
            rusqlite::params![
                &destination_id,
                serde_json::to_string(&routes).unwrap(),
                serde_json::to_string(&protocols).unwrap(),
                changed,
            ],
        )
        .unwrap();
    grant_route(&fx, "query-key", EndpointOperation::ChatCreate, changed);
    let second = project_fx(&fx);
    let second_auth = auth(&second, "query-key");
    let second_url = second_auth.models[0].routes[0].endpoint_url.clone();
    let second_set = route_set(&second, &second_auth.credential_id);
    let second_route = &second_set.routes[0];
    assert_ne!(second_url, first_url);
    assert!(second_url.contains("probe=1"));
    assert_eq!(second_route.origin, first_route.origin);
    assert_ne!(
        second_route.endpoint_fingerprint,
        first_route.endpoint_fingerprint
    );
    assert_ne!(second_set.fingerprint, first_set.fingerprint);
    assert!(!second_set.fingerprint.contains(changed));
    assert!(!second_set.fingerprint.contains("probe=1"));
    assert!(!second_route.endpoint_fingerprint.contains("probe=1"));

    let pinned = |hash: &str| super::NormalizedRoute {
        public_model: "chat-public".into(),
        upstream_model: "chat-upstream".into(),
        protocol: "chat_completions".into(),
        endpoint_id: first_route.endpoint_id.clone(),
        origin: first_route.origin.clone(),
        endpoint_fingerprint: hash.into(),
        validation_only: false,
        native_targets: Vec::new(),
    };
    let same_id_before = super::credential_route_fingerprint(
        "auth",
        "cred",
        1,
        "bind",
        "material-fingerprint",
        0,
        &[pinned(&first_route.endpoint_fingerprint)],
    );
    let same_id_after = super::credential_route_fingerprint(
        "auth",
        "cred",
        1,
        "bind",
        "material-fingerprint",
        0,
        &[pinned(&second_route.endpoint_fingerprint)],
    );
    assert_ne!(same_id_before, same_id_after);
    assert!(!same_id_before.contains(original));
    assert!(!same_id_after.contains(changed));
    assert!(!same_id_after.contains("probe=1"));
}

fn owned_native_connection() -> ocg_domain::connection::ConnectionId {
    ocg_domain::connection::connection_id_for_legacy(
        ocg_domain::connection::LegacyConnectionKind::BuiltinProvider,
        "cpa-owned-native",
    )
}

fn codex_facts() -> super::NativeAuthorityFacts {
    super::normalize_native_label("codex")
}

#[test]
fn canonical_native_url_rejects_userinfo_and_fragment_and_omits_the_default_port() {
    let canonical = "https://chatgpt.com/backend-api/codex/responses";
    assert_eq!(
        super::canonical_native_url(canonical).as_deref(),
        Some(canonical)
    );
    assert_eq!(
        super::canonical_native_url("https://chatgpt.com:443/backend-api/codex/responses")
            .as_deref(),
        Some(canonical)
    );
    assert_eq!(
        super::endpoint_fingerprint(
            &super::canonical_native_url("https://chatgpt.com:443/backend-api/codex/responses")
                .unwrap()
        ),
        super::endpoint_fingerprint(canonical)
    );
    let origin = ocg_domain::credential::normalize_origin(canonical).unwrap();
    assert_eq!(origin, "https://chatgpt.com");
    assert!(!origin.contains(":443"));
    assert!(
        super::canonical_native_url("https://user:pass@chatgpt.com/backend-api/codex/responses")
            .is_none()
    );
    assert!(
        super::canonical_native_url("https://chatgpt.com/backend-api/codex/responses#frag")
            .is_none()
    );
    assert!(super::canonical_native_url("mailto:codex@chatgpt.com").is_none());
    assert_eq!(super::GEMINI_CALLABLE_PROTOCOL, "chat_completions");
    assert!(!super::endpoint_pin_capability_listed(&[]));
    assert!(super::endpoint_pin_capability_listed(&[
        super::NATIVE_ENDPOINT_PIN_CAPABILITY.to_string()
    ]));
}

#[test]
fn stored_native_targets_reject_duplicate_ids_and_fingerprints_independently() {
    let connection = owned_native_connection();
    let (_, targets) =
        super::native_route_targets(&codex_facts(), "gpt-5", "chat_completions", &connection)
            .expect("two distinct codex pins");
    assert_eq!(targets.len(), 2);
    assert_ne!(targets[0].pin.endpoint_id, targets[1].pin.endpoint_id);
    assert_ne!(
        targets[0].pin.endpoint_fingerprint,
        targets[1].pin.endpoint_fingerprint
    );
    assert!(super::accept_stored_native_targets(&targets));
    let route = super::NormalizedRoute {
        public_model: "gpt-5".into(),
        upstream_model: "gpt-5".into(),
        protocol: "chat_completions".into(),
        endpoint_id: targets[0].pin.endpoint_id.clone(),
        origin: targets[0].pin.origin.clone(),
        endpoint_fingerprint: targets[0].pin.endpoint_fingerprint.clone(),
        validation_only: false,
        native_targets: targets.clone(),
    };
    assert!(super::targets_for_applied_route(&route, "chat_completions", "execute").is_some());

    let mut exact = targets.clone();
    exact.push(targets[0].clone());
    assert!(!super::accept_stored_native_targets(&exact));
    let mut exact_route = route.clone();
    exact_route.native_targets = exact;
    assert!(
        super::targets_for_applied_route(&exact_route, "chat_completions", "execute").is_none()
    );

    let mut same_id = targets.clone();
    same_id[1].pin.endpoint_id = same_id[0].pin.endpoint_id.clone();
    assert_ne!(
        same_id[0].pin.endpoint_fingerprint,
        same_id[1].pin.endpoint_fingerprint
    );
    assert!(!super::accept_stored_native_targets(&same_id));
    let mut same_id_route = route.clone();
    same_id_route.native_targets = same_id;
    assert!(
        super::targets_for_applied_route(&same_id_route, "chat_completions", "execute").is_none()
    );

    let mut same_fingerprint = targets.clone();
    same_fingerprint[1].pin.endpoint_fingerprint =
        same_fingerprint[0].pin.endpoint_fingerprint.clone();
    assert_ne!(
        same_fingerprint[0].pin.endpoint_id,
        same_fingerprint[1].pin.endpoint_id
    );
    assert!(!super::accept_stored_native_targets(&same_fingerprint));
    let mut same_fingerprint_route = route;
    same_fingerprint_route.native_targets = same_fingerprint;
    assert!(
        super::targets_for_applied_route(&same_fingerprint_route, "chat_completions", "execute")
            .is_none()
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn xai_cli_facts() -> super::NativeAuthorityFacts {
    super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: Some("cli"),
        effective_generation_base: Some(""),
        effective_auth_kind: Some("oauth"),
    })
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn fixture_origin_change_changes_native_pins_not_captured_grants() {
    let connection = owned_native_connection();
    let facts = codex_facts();
    let models = vec!["gpt-5".to_string()];
    assert_eq!(facts.provider, "codex");
    assert!(facts.mode.is_empty());
    assert!(facts.reported_base.is_empty());
    let official = super::native_route_targets(&facts, "gpt-5", "chat_completions", &connection)
        .expect("official codex pins");
    let captured = super::default_grant_ids(&facts, &models, &connection).expect("official grants");
    assert_eq!(
        captured.0,
        vec![
            official.0.endpoint_id.clone(),
            official.1[1].pin.endpoint_id.clone()
        ]
    );
    assert_eq!(captured.1, vec!["https://chatgpt.com".to_string()]);

    let pins_a = {
        let _guard =
            super::native_targets::install_fixture_mapping(r#"{"codex":"http://127.0.0.1:18080"}"#);
        assert!(matches!(
            super::native_targets_for(
                &facts,
                "gpt-5",
                "chat_completions",
                super::NativeSourceOperation::CountTokens,
                &connection
            ),
            super::NativeTargetOutcome::LocalOnly
        ));
        let pins = super::native_route_targets(&facts, "gpt-5", "chat_completions", &connection)
            .expect("origin a");
        let fresh = super::default_grant_ids(&facts, &models, &connection).expect("fresh grants");
        assert_eq!(
            fresh.0,
            vec![
                pins.0.endpoint_id.clone(),
                pins.1[1].pin.endpoint_id.clone()
            ]
        );
        assert_eq!(fresh.1, vec![pins.0.origin.clone()]);
        assert_ne!(fresh, captured);
        pins
    };
    let pins_b = {
        let _guard =
            super::native_targets::install_fixture_mapping(r#"{"codex":"http://[::1]:18081"}"#);
        super::native_route_targets(&facts, "gpt-5", "chat_completions", &connection)
            .expect("origin b")
    };
    assert_eq!(facts.provider, "codex");
    assert!(facts.mode.is_empty());
    assert!(facts.reported_base.is_empty());
    assert_eq!(pins_a.0.origin, "http://127.0.0.1:18080");
    assert_eq!(pins_b.0.origin, "http://[::1]:18081");
    assert_eq!(pins_a.1[1].pin.origin, pins_a.0.origin);
    assert_ne!(pins_a.0.endpoint_id, pins_b.0.endpoint_id);
    assert_ne!(pins_a.0.endpoint_fingerprint, pins_b.0.endpoint_fingerprint);
    assert_ne!(pins_a.0.endpoint_id, official.0.endpoint_id);
    assert_eq!(
        pins_a.0.endpoint_fingerprint,
        super::endpoint_fingerprint("http://127.0.0.1:18080/backend-api/codex/responses")
    );
    assert_eq!(
        pins_a.1[1].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("http://127.0.0.1:18080/backend-api/codex/responses/compact")
    );
    assert_eq!(
        pins_b.0.endpoint_fingerprint,
        super::endpoint_fingerprint("http://[::1]:18081/backend-api/codex/responses")
    );
    assert_eq!(
        pins_a.0.endpoint_id,
        ocg_domain::connection::endpoint_id_for_route(
            &connection,
            ocg_domain::connection::EndpointOperation::ResponseCreate,
            "http://127.0.0.1:18080/backend-api/codex/responses",
        )
        .as_str()
    );
    assert!(!captured.0.contains(&pins_a.0.endpoint_id));
    assert!(!captured.0.contains(&pins_b.0.endpoint_id));
    assert!(!captured.0.contains(&pins_a.1[1].pin.endpoint_id));
    assert!(
        !captured
            .1
            .iter()
            .any(|origin| origin.contains("127.0.0.1") || origin.contains("::1"))
    );
    let still_official =
        super::default_grant_ids(&facts, &models, &connection).expect("grants restored");
    assert_eq!(still_official, captured);
    let mut loopback_base = facts.clone();
    loopback_base.reported_base = "http://127.0.0.1:18080".into();
    assert!(super::default_grant_ids(&loopback_base, &models, &connection).is_none());
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn xai_cli_fixture_rewrite_keeps_official_facts_and_captured_grants() {
    let connection = owned_native_connection();
    let facts = xai_cli_facts();
    assert_eq!(facts.mode, "cli");
    assert_eq!(facts.reported_base, "https://cli-chat-proxy.grok.com/v1");
    let models = vec!["grok-3".to_string()];
    let captured = super::default_grant_ids(&facts, &models, &connection).expect("cli grants");
    assert!(
        captured
            .1
            .iter()
            .any(|origin| origin == "https://cli-chat-proxy.grok.com")
    );
    assert!(captured.1.iter().any(|origin| origin == "https://api.x.ai"));
    {
        let _guard =
            super::native_targets::install_fixture_mapping(r#"{"xai.api":"http://127.0.0.1:9"}"#);
        let execute = super::native_targets_for(
            &facts,
            "grok-3",
            "responses",
            super::NativeSourceOperation::Execute,
            &connection,
        );
        let super::NativeTargetOutcome::Network(execute_targets) = execute else {
            panic!("api map does not rewrite cli execute");
        };
        assert_eq!(
            execute_targets[0].pin.endpoint_fingerprint,
            super::endpoint_fingerprint("https://cli-chat-proxy.grok.com/v1/responses")
        );
        let compact = super::native_targets_for(
            &facts,
            "grok-3",
            "responses",
            super::NativeSourceOperation::SourceCompact,
            &connection,
        );
        let super::NativeTargetOutcome::Network(compact_targets) = compact else {
            panic!("api map does not rewrite cli compact");
        };
        assert_eq!(
            compact_targets[0].pin.endpoint_fingerprint,
            super::endpoint_fingerprint("https://api.x.ai/v1/responses/compact")
        );
        assert_eq!(
            super::default_grant_ids(&facts, &models, &connection).as_ref(),
            Some(&captured)
        );
    }
    {
        let _guard = super::native_targets::install_fixture_mapping(
            r#"{"xai.cli":"http://127.0.0.1:18080"}"#,
        );
        let compact = super::native_targets_for(
            &facts,
            "grok-3",
            "responses",
            super::NativeSourceOperation::SourceCompact,
            &connection,
        );
        let super::NativeTargetOutcome::Network(targets) = compact else {
            panic!("cli compact rewrites X3");
        };
        assert_eq!(targets[0].pin.origin, "http://127.0.0.1:18080");
        assert_eq!(
            targets[0].pin.endpoint_fingerprint,
            super::endpoint_fingerprint("http://127.0.0.1:18080/v1/responses/compact")
        );
        assert!(!captured.0.contains(&targets[0].pin.endpoint_id));
        assert!(
            !captured
                .1
                .iter()
                .any(|origin| origin == "http://127.0.0.1:18080")
        );
    }
    assert_eq!(facts.reported_base, "https://cli-chat-proxy.grok.com/v1");
    assert_eq!(facts.mode, "cli");
    assert_eq!(facts.provider, "xai");
    let mut loopback_base = facts.clone();
    loopback_base.reported_base = "http://127.0.0.1:18080".into();
    assert_eq!(loopback_base.provider, "xai");
    assert_eq!(loopback_base.mode, "cli");
    assert!(super::default_grant_ids(&loopback_base, &models, &connection).is_none());
    assert_eq!(
        super::default_grant_ids(&facts, &models, &connection).as_ref(),
        Some(&captured)
    );
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn bad_native_fixture_map_emits_no_target_and_no_grant() {
    let connection = owned_native_connection();
    let facts = codex_facts();
    let models = vec!["gpt-5".to_string()];
    let captured = super::default_grant_ids(&facts, &models, &connection).expect("official grants");
    for mapping in [
        r#"{"codex":"#,
        r#"{"cpa":"http://127.0.0.1:9"}"#,
        r#"{"base_url":"http://127.0.0.1:9"}"#,
        r#"{"codex":"http://127.0.0.1:9","codex":"http://127.0.0.1:10"}"#,
        r#"{"codex":"https://127.0.0.1:9"}"#,
        r#"{"codex":"http://8.8.8.8:9"}"#,
    ] {
        let _guard = super::native_targets::install_fixture_mapping(mapping);
        assert_eq!(
            super::native_targets_for(
                &facts,
                "gpt-5",
                "chat_completions",
                super::NativeSourceOperation::Execute,
                &connection
            ),
            super::NativeTargetOutcome::Unavailable,
            "{mapping}"
        );
        assert!(
            super::default_grant_ids(&facts, &models, &connection).is_none(),
            "{mapping}"
        );
    }
    assert_eq!(facts.provider, "codex");
    assert!(facts.reported_base.is_empty());
    assert_eq!(
        super::default_grant_ids(&facts, &models, &connection).as_ref(),
        Some(&captured)
    );
}

#[test]
fn native_targets_follow_the_source_matrix_and_refuse_unknown_modes() {
    let connection = owned_native_connection();
    let codex = codex_facts();
    let chat = super::native_route_targets(&codex, "gpt-5", "chat_completions", &connection)
        .expect("codex chat");
    assert_eq!(chat.0.protocol, "chat_completions");
    assert_eq!(chat.0.endpoint_id, "80bcfcea-1f42-5a5e-81f8-3abd801b2aea");
    assert_eq!(
        chat.0.endpoint_fingerprint,
        "1897faf097db8edfa5c0c6765abb12be180ed7aff633203298e6c0c28fcb16e5"
    );
    assert_eq!(chat.0.origin, "https://chatgpt.com");
    assert_eq!(chat.0.http_method, "POST");
    assert_eq!(chat.1.len(), 2);
    assert_eq!(
        chat.1[0].generation_kinds,
        vec![
            "execute".to_string(),
            "refresh-resend".to_string(),
            "stream".to_string(),
            "stream-refresh".to_string(),
            "stream-bootstrap".to_string(),
            "internal".to_string(),
        ]
    );
    assert_eq!(
        chat.1[1].pin.endpoint_id,
        "2a2c3bd0-eeb1-5db1-837c-3447bdc814e8"
    );
    assert_eq!(
        chat.1[1].pin.endpoint_fingerprint,
        "1f15e0f73830039138395623d8c53a1c186ee874f0ea9e00262d9da637f81c38"
    );
    assert_eq!(chat.1[1].generation_kinds, vec!["internal".to_string()]);
    assert!(matches!(
        super::native_targets_for(
            &codex,
            "gpt-5",
            "chat_completions",
            super::NativeSourceOperation::CountTokens,
            &connection
        ),
        super::NativeTargetOutcome::LocalOnly
    ));
    let compact = super::native_targets_for(
        &codex,
        "gpt-5",
        "responses",
        super::NativeSourceOperation::SourceCompact,
        &connection,
    );
    let super::NativeTargetOutcome::Network(compact_targets) = compact else {
        panic!("codex compact is C2");
    };
    assert_eq!(
        compact_targets[0].pin.endpoint_id,
        chat.1[1].pin.endpoint_id
    );
    let wire = serde_json::to_value(&chat.1).unwrap();
    assert_eq!(wire[0]["pin"]["endpointId"], chat.0.endpoint_id);
    assert_eq!(wire[0]["pin"]["httpMethod"], "POST");
    assert_eq!(wire[1]["generationKinds"][0], "internal");

    let route = super::NormalizedRoute {
        public_model: "gpt-5".into(),
        upstream_model: "gpt-5".into(),
        protocol: "chat_completions".into(),
        endpoint_id: chat.0.endpoint_id.clone(),
        origin: chat.0.origin.clone(),
        endpoint_fingerprint: chat.0.endpoint_fingerprint.clone(),
        validation_only: false,
        native_targets: chat.1.clone(),
    };
    let execute =
        super::targets_for_applied_route(&route, "chat_completions", "execute").expect("execute");
    assert_eq!(execute.len(), 1);
    assert_eq!(execute[0].endpoint_id, chat.0.endpoint_id);
    let internal = super::targets_for_applied_route(&route, "chat_completions", "internal")
        .expect("ordinary and compact internal pins");
    assert_eq!(internal.len(), 2);
    assert_eq!(internal[0].endpoint_id, chat.0.endpoint_id);
    assert_eq!(internal[1].endpoint_id, chat.1[1].pin.endpoint_id);
    let counted = super::targets_for_applied_route(&route, "chat_completions", "count-tokens")
        .expect("zero network pins");
    assert!(counted.is_empty());
    let other_protocol =
        super::targets_for_applied_route(&route, "responses", "execute").expect("other protocol");
    assert!(other_protocol.is_empty());
    assert!(super::targets_for_applied_route(&route, "generate_content", "execute").is_none());
    assert!(super::targets_for_applied_route(&route, "chat_completions", "compact").is_none());
    let mut reused = route.clone();
    reused.native_targets.push(reused.native_targets[0].clone());
    assert!(super::targets_for_applied_route(&reused, "chat_completions", "execute").is_none());

    let claude = super::normalize_native_label("Claude");
    assert_eq!(claude.raw_label, "Claude");
    assert_eq!(claude.provider, "anthropic");
    assert!(claude.mode.is_empty());
    let anthropic = super::native_targets_for(
        &claude,
        "claude-sonnet",
        "messages",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(anthropic_targets) = anthropic else {
        panic!("claude label uses A1");
    };
    assert_eq!(
        anthropic_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.anthropic.com/v1/messages?beta=true")
    );
    assert!(matches!(
        super::native_targets_for(
            &claude,
            "claude-sonnet",
            "messages",
            super::NativeSourceOperation::SourceCompact,
            &connection
        ),
        super::NativeTargetOutcome::Unavailable
    ));

    let bare_kimi = super::normalize_native_label("kimi");
    assert!(bare_kimi.mode.is_empty());
    assert!(matches!(
        super::native_targets_for(
            &bare_kimi,
            "kimi-for-coding",
            "chat_completions",
            super::NativeSourceOperation::Execute,
            &connection
        ),
        super::NativeTargetOutcome::Unavailable
    ));
    assert!(
        super::default_grant_ids(&bare_kimi, &["kimi-for-coding".into()], &connection).is_none()
    );
    let kimi_com = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "kimi",
        effective_subtype: Some("kimi.com"),
        effective_mode: Some("com"),
        effective_generation_base: Some("https://api.kimi.com/coding/v1"),
        effective_auth_kind: None,
    });
    assert_eq!(kimi_com.provider, "kimi");
    assert_eq!(kimi_com.mode, "com");
    assert_eq!(kimi_com.raw_label, "kimi");
    assert_eq!(kimi_com.reported_base, "https://api.kimi.com/coding/v1");
    let kimi_chat = super::native_targets_for(
        &kimi_com,
        "kimi-for-coding",
        "chat_completions",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(kimi_targets) = kimi_chat else {
        panic!("kimi.com chat");
    };
    assert_eq!(
        kimi_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.kimi.com/coding/v1/chat/completions")
    );
    let kimi_ai = super::normalize_native_label("kimi-ai");
    assert_eq!(kimi_ai.mode, "ai");
    let kimi_messages = super::native_targets_for(
        &kimi_ai,
        "kimi-for-coding",
        "messages",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(ai_targets) = kimi_messages else {
        panic!("kimi.ai messages");
    };
    assert_eq!(
        ai_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.kimi.ai/coding/v1/messages?beta=true")
    );

    let xai_cli = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: Some("cli"),
        effective_generation_base: Some("https://api.x.ai/v1"),
        effective_auth_kind: Some("oauth"),
    });
    assert_eq!(xai_cli.mode, "cli");
    assert_eq!(xai_cli.reported_base, "https://cli-chat-proxy.grok.com/v1");
    let xai_execute = super::native_targets_for(
        &xai_cli,
        "grok-3",
        "responses",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(xai_targets) = xai_execute else {
        panic!("xai cli");
    };
    assert_eq!(
        xai_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://cli-chat-proxy.grok.com/v1/responses")
    );
    let xai_compact = super::native_targets_for(
        &xai_cli,
        "grok-3",
        "responses",
        super::NativeSourceOperation::SourceCompact,
        &connection,
    );
    let super::NativeTargetOutcome::Network(xai_compact_targets) = xai_compact else {
        panic!("xai cli compact stays on the official API");
    };
    assert_eq!(
        xai_compact_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.x.ai/v1/responses/compact")
    );
    let xai_api = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: Some("api"),
        effective_generation_base: Some("https://api.x.ai/v1"),
        effective_auth_kind: Some("oauth"),
    });
    assert_eq!(xai_api.mode, "api");
    assert_eq!(xai_api.reported_base, "https://api.x.ai/v1");
    let xai_official = super::native_targets_for(
        &xai_api,
        "grok-3",
        "responses",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(official) = xai_official else {
        panic!("xai api");
    };
    assert_eq!(
        official[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.x.ai/v1/responses")
    );
    let xai_missing = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: None,
        effective_generation_base: Some(""),
        effective_auth_kind: Some("oauth"),
    });
    assert!(xai_missing.provider.is_empty());
    assert!(xai_missing.mode.is_empty());
    assert!(matches!(
        super::native_targets_for(
            &xai_missing,
            "grok-3",
            "responses",
            super::NativeSourceOperation::Execute,
            &connection
        ),
        super::NativeTargetOutcome::Unavailable
    ));
    assert_eq!(super::normalize_native_label("grok").provider, "");

    let antigravity = super::normalize_native_label("antigravity");
    let ordinary =
        super::native_route_targets(&antigravity, "model-a", "chat_completions", &connection)
            .expect("daily");
    assert_eq!(
        ordinary.0.endpoint_fingerprint,
        super::endpoint_fingerprint(
            "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent"
        )
    );
    assert_eq!(ordinary.1.len(), 3);
    let streamed = super::native_route_targets(
        &antigravity,
        "gemini-3-pro-preview",
        "chat_completions",
        &connection,
    )
    .expect("streamed branch");
    assert_eq!(
        streamed.0.endpoint_fingerprint,
        super::endpoint_fingerprint(
            "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        )
    );
    let mut production = antigravity.clone();
    production.reported_base =
        "https://cloudcode-pa.googleapis.com/v1internal:generateContent".into();
    assert!(
        super::native_route_targets(&production, "model-a", "chat_completions", &connection)
            .is_none()
    );
    assert!(
        super::native_targets_for(
            &codex,
            "gpt-5",
            "generate_content",
            super::NativeSourceOperation::Execute,
            &connection
        ) == super::NativeTargetOutcome::Unavailable
    );

    let (ids, origins) =
        super::default_grant_ids(&codex, &["gpt-5".into()], &connection).expect("grants");
    assert_eq!(
        ids,
        vec![
            chat.0.endpoint_id.clone(),
            chat.1[1].pin.endpoint_id.clone()
        ]
    );
    assert_eq!(origins, vec!["https://chatgpt.com".to_string()]);

    let mut empty = route.clone();
    empty.native_targets.clear();
    let bare_fingerprint =
        super::credential_route_fingerprint("auth", "cred", 1, "bind", "material", 0, &[empty]);
    let full_fingerprint =
        super::credential_route_fingerprint("auth", "cred", 1, "bind", "material", 0, &[route]);
    assert_ne!(bare_fingerprint, full_fingerprint);
    assert_eq!(bare_fingerprint.len(), 64);
}

#[test]
fn effective_native_wire_separates_product_identity_and_resolved_bases() {
    let connection = owned_native_connection();
    let models = ["gpt-5".to_string()];
    let product_label = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "cpa",
        effective_subtype: Some("codex"),
        effective_mode: Some(""),
        effective_generation_base: Some(""),
        effective_auth_kind: None,
    });
    assert_eq!(product_label.provider, "codex");
    assert_eq!(product_label.raw_label, "cpa");
    assert!(product_label.mode.is_empty());
    assert_eq!(
        product_label.reported_base,
        "https://chatgpt.com/backend-api/codex"
    );
    let sealed = super::normalize_native_label("codex");
    let sealed_grants =
        super::default_grant_ids(&sealed, &models, &connection).expect("sealed codex grants");
    let prefixed = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "codex",
        effective_subtype: Some("Codex"),
        effective_mode: None,
        effective_generation_base: Some("https://chatgpt.com/backend-api/codex"),
        effective_auth_kind: None,
    });
    assert_eq!(
        super::default_grant_ids(&prefixed, &models, &connection).as_ref(),
        Some(&sealed_grants)
    );
    let exact_dictionary = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "codex",
        effective_subtype: Some("codex"),
        effective_mode: Some(""),
        effective_generation_base: Some("https://chatgpt.com/backend-api/codex/responses"),
        effective_auth_kind: None,
    });
    assert_eq!(
        super::default_grant_ids(&exact_dictionary, &models, &connection).as_ref(),
        Some(&sealed_grants)
    );
    let origin = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "codex",
        effective_subtype: Some("codex"),
        effective_mode: Some(""),
        effective_generation_base: Some("https://chatgpt.com"),
        effective_auth_kind: None,
    });
    assert!(origin.provider.is_empty());
    assert_eq!(
        origin.reported_base.trim_end_matches('/'),
        "https://chatgpt.com"
    );
    assert!(super::default_grant_ids(&origin, &models, &connection).is_none());
    let extra_path = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "codex",
        effective_subtype: Some("codex"),
        effective_mode: Some(""),
        effective_generation_base: Some("https://chatgpt.com/backend-api/codex/extra"),
        effective_auth_kind: None,
    });
    assert!(extra_path.provider.is_empty());
    assert_eq!(
        extra_path.reported_base,
        "https://chatgpt.com/backend-api/codex/extra"
    );
    let userinfo = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "codex",
        effective_subtype: Some("codex"),
        effective_mode: Some(""),
        effective_generation_base: Some("https://user:pass@chatgpt.com/backend-api/codex"),
        effective_auth_kind: None,
    });
    assert!(userinfo.provider.is_empty());
    assert!(userinfo.reported_base.contains("user:pass@"));
    assert!(super::default_grant_ids(&userinfo, &models, &connection).is_none());
    let fragment = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "codex",
        effective_subtype: Some("codex"),
        effective_mode: Some(""),
        effective_generation_base: Some("https://chatgpt.com/backend-api/codex#frag"),
        effective_auth_kind: None,
    });
    assert!(fragment.provider.is_empty());
    assert!(fragment.reported_base.contains("#frag"));
    let filename = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "claude.json",
        effective_subtype: None,
        effective_mode: None,
        effective_generation_base: Some(""),
        effective_auth_kind: None,
    });
    assert!(filename.provider.is_empty());
    assert_eq!(filename.raw_label, "claude.json");
    let omitted_base = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "claude",
        effective_subtype: Some("anthropic"),
        effective_mode: Some("claude"),
        effective_generation_base: None,
        effective_auth_kind: None,
    });
    assert!(omitted_base.provider.is_empty());
    assert!(omitted_base.mode.is_empty());
    assert!(omitted_base.reported_base.is_empty());

    let claude = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "claude",
        effective_subtype: Some("anthropic"),
        effective_mode: Some("claude"),
        effective_generation_base: Some(""),
        effective_auth_kind: None,
    });
    assert_eq!(claude.provider, "anthropic");
    assert_eq!(claude.raw_label, "claude");
    assert!(claude.mode.is_empty());
    assert_eq!(claude.reported_base, "https://api.anthropic.com");
    let claude_send = super::native_targets_for(
        &claude,
        "claude-sonnet",
        "messages",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(claude_targets) = claude_send else {
        panic!("claude empty base is A1");
    };
    assert_eq!(
        claude_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.anthropic.com/v1/messages?beta=true")
    );
    let claude_v1 = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "claude",
        effective_subtype: Some("anthropic"),
        effective_mode: Some("claude"),
        effective_generation_base: Some("https://api.anthropic.com/v1"),
        effective_auth_kind: None,
    });
    assert!(claude_v1.provider.is_empty());
    assert_eq!(claude_v1.reported_base, "https://api.anthropic.com/v1");
    let custom = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "claude",
        effective_subtype: Some("anthropic"),
        effective_mode: Some("claude"),
        effective_generation_base: Some("https://example.com/anthropic"),
        effective_auth_kind: None,
    });
    assert!(custom.provider.is_empty());
    assert!(custom.mode.is_empty());
    assert_eq!(custom.reported_base, "https://example.com/anthropic");

    let daily = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "antigravity",
        effective_subtype: Some("antigravity"),
        effective_mode: Some("DAILY"),
        effective_generation_base: Some(""),
        effective_auth_kind: None,
    });
    assert_eq!(daily.provider, "antigravity");
    assert!(daily.mode.is_empty());
    assert_eq!(
        daily.reported_base,
        "https://daily-cloudcode-pa.googleapis.com"
    );
    let daily_send = super::native_targets_for(
        &daily,
        "model-a",
        "chat_completions",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(daily_targets) = daily_send else {
        panic!("daily empty base is G1");
    };
    assert_eq!(
        daily_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint(
            "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent"
        )
    );
    let custom_mode = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "antigravity",
        effective_subtype: Some("antigravity"),
        effective_mode: Some("custom"),
        effective_generation_base: Some("https://daily-cloudcode-pa.googleapis.com"),
        effective_auth_kind: None,
    });
    assert!(custom_mode.provider.is_empty());
    assert!(custom_mode.mode.is_empty());
    assert_eq!(
        custom_mode.reported_base.trim_end_matches('/'),
        "https://daily-cloudcode-pa.googleapis.com"
    );
    let production = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "antigravity",
        effective_subtype: Some("antigravity"),
        effective_mode: Some("daily"),
        effective_generation_base: Some("https://cloudcode-pa.googleapis.com"),
        effective_auth_kind: None,
    });
    assert!(production.provider.is_empty());
    assert_eq!(
        production.reported_base.trim_end_matches('/'),
        "https://cloudcode-pa.googleapis.com"
    );
    assert!(
        super::native_route_targets(&production, "model-a", "chat_completions", &connection)
            .is_none()
    );

    let kimi_prefix = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "kimi.com",
        effective_subtype: Some("kimi.com"),
        effective_mode: None,
        effective_generation_base: Some("https://api.kimi.com/coding"),
        effective_auth_kind: None,
    });
    assert_eq!(kimi_prefix.provider, "kimi");
    assert_eq!(kimi_prefix.mode, "com");
    assert_eq!(kimi_prefix.reported_base, "https://api.kimi.com/coding");
    let kimi_chat = super::native_targets_for(
        &kimi_prefix,
        "kimi-for-coding",
        "chat_completions",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(kimi_targets) = kimi_chat else {
        panic!("kimi coding prefix");
    };
    assert_eq!(
        kimi_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.kimi.com/coding/v1/chat/completions")
    );
    let kimi_v1 = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "kimi.com",
        effective_subtype: Some("kimi.com"),
        effective_mode: Some("com"),
        effective_generation_base: Some("https://api.kimi.com/coding/v1/"),
        effective_auth_kind: None,
    });
    assert_eq!(kimi_v1.reported_base, "https://api.kimi.com/coding/v1");
    assert!(super::default_grant_ids(&kimi_v1, &["kimi-for-coding".into()], &connection).is_some());
    let kimi_ai = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "kimi.ai",
        effective_subtype: Some("kimi.ai"),
        effective_mode: Some("ai"),
        effective_generation_base: Some(""),
        effective_auth_kind: None,
    });
    assert_eq!(kimi_ai.mode, "ai");
    assert_eq!(kimi_ai.reported_base, "https://api.kimi.ai/coding");
    let kimi_origin = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "kimi.com",
        effective_subtype: Some("kimi.com"),
        effective_mode: Some("com"),
        effective_generation_base: Some("https://api.kimi.com"),
        effective_auth_kind: None,
    });
    assert!(kimi_origin.provider.is_empty());
    assert_eq!(
        kimi_origin.reported_base.trim_end_matches('/'),
        "https://api.kimi.com"
    );
    let mode_fight = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "kimi",
        effective_subtype: Some("kimi.com"),
        effective_mode: Some("ai"),
        effective_generation_base: Some("https://api.kimi.com/coding"),
        effective_auth_kind: None,
    });
    assert!(mode_fight.provider.is_empty());
    assert!(mode_fight.mode.is_empty());
    let raw_fight = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "kimi.com",
        effective_subtype: Some("kimi.ai"),
        effective_mode: Some("ai"),
        effective_generation_base: Some("https://api.kimi.ai/coding"),
        effective_auth_kind: None,
    });
    assert!(raw_fight.provider.is_empty());
    assert_eq!(raw_fight.raw_label, "kimi.com");
    assert!(
        super::default_grant_ids(&raw_fight, &["kimi-for-coding".into()], &connection).is_none()
    );

    let cli_default = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: Some("cli"),
        effective_generation_base: Some(""),
        effective_auth_kind: Some("OAuth"),
    });
    assert_eq!(cli_default.mode, "cli");
    assert_eq!(
        cli_default.reported_base,
        "https://cli-chat-proxy.grok.com/v1"
    );
    let cli_rewrite = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: Some("cli"),
        effective_generation_base: Some("https://api.x.ai/v1"),
        effective_auth_kind: Some("oauth"),
    });
    assert_eq!(
        cli_rewrite.reported_base,
        "https://cli-chat-proxy.grok.com/v1"
    );
    let cli_send = super::native_targets_for(
        &cli_default,
        "grok-3",
        "responses",
        super::NativeSourceOperation::Execute,
        &connection,
    );
    let super::NativeTargetOutcome::Network(cli_targets) = cli_send else {
        panic!("missing using_api resolved as cli is X1");
    };
    assert_eq!(
        cli_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://cli-chat-proxy.grok.com/v1/responses")
    );
    let cli_compact = super::native_targets_for(
        &cli_rewrite,
        "grok-3",
        "responses",
        super::NativeSourceOperation::SourceCompact,
        &connection,
    );
    let super::NativeTargetOutcome::Network(cli_compact_targets) = cli_compact else {
        panic!("cli compact stays official");
    };
    assert_eq!(
        cli_compact_targets[0].pin.endpoint_fingerprint,
        super::endpoint_fingerprint("https://api.x.ai/v1/responses/compact")
    );
    let missing_auth = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: Some("cli"),
        effective_generation_base: Some(""),
        effective_auth_kind: None,
    });
    assert!(missing_auth.provider.is_empty());
    assert!(missing_auth.mode.is_empty());
    let missing_mode = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: None,
        effective_generation_base: Some("https://api.x.ai/v1"),
        effective_auth_kind: Some("oauth"),
    });
    assert!(missing_mode.provider.is_empty());
    assert!(missing_mode.mode.is_empty());
    let custom_xai = super::facts_from_effective(&super::EffectiveNativeWire {
        raw_provider_label: "xai",
        effective_subtype: Some("xai"),
        effective_mode: Some("cli"),
        effective_generation_base: Some("https://example.com/v1"),
        effective_auth_kind: Some("oauth"),
    });
    assert!(custom_xai.provider.is_empty());
    assert!(custom_xai.mode.is_empty());
    assert_eq!(custom_xai.reported_base, "https://example.com/v1");
}
