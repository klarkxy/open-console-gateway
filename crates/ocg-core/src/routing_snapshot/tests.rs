use super::*;
use crate::gateway::forwarder::{LiveSendSelection, verify_execution_authorization};
use crate::gateway::handler::RuntimeCatalogSnapshot;
use crate::gateway::materialize::{ExecutionRoute, materialize_execution_routes};
use crate::gateway::protocol::parse_client_request;
use crate::kernel::protocol::ApiFormat;
use crate::models::{Account, AccountCustomConfigInput, AccountModelCapabilityInput, AppConfig};
use bytes::Bytes;
use ocg_domain::credential::{ModelScope, assigned_endpoints_for_routes};
use ocg_domain::destination::{
    AuthScheme, CatalogModel, HttpProtocolRoute, LegacyDestinationRef, ModelResolution, Protocol,
    http_configured_routes,
};
use ocg_domain::dynamic::{
    DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride, DynamicProviderDefinition,
};

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> (Scratch, crate::db::Database, RoutingSnapshot) {
    let directory = Scratch(
        std::env::temp_dir().join(format!("ocg-runtime-snapshot-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    let db = crate::db::Database::open(directory.0.clone()).unwrap();
    let now = Utc::now();
    let account: Account = serde_json::from_value(serde_json::json!({
        "id": "snapshot-http-key", "name": "HTTP Key", "provider_id": crate::provider::CUSTOM_PROVIDER_ID,
        "key_cipher": "encrypted-test-material", "enabled": true, "purchase_date": "",
        "created_at": now, "updated_at": now
    })).unwrap();
    db.create_account_with_contract(
        &account,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1".into(),
            upstream_protocol: Protocol::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "public-model".into(),
            upstream_model: "upstream-exact".into(),
            protocol: Protocol::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let snapshot = RoutingSnapshot::load(&db).unwrap();
    (directory, db, snapshot)
}

fn route(snapshot: &RoutingSnapshot, name: &str) -> ExecutionRoute {
    route_for_protocol(snapshot, name, ApiFormat::ChatCompletions)
}

fn route_for_protocol(snapshot: &RoutingSnapshot, name: &str, client: ApiFormat) -> ExecutionRoute {
    let catalog = RuntimeCatalogSnapshot::from_routing(snapshot.clone(), Utc::now());
    let resolved = catalog.resolve(name).unwrap();
    let body = match client {
        ApiFormat::ChatCompletions => serde_json::json!({
            "model": name, "messages": [{"role":"user", "content":"hello"}]
        }),
        ApiFormat::Responses => {
            serde_json::json!({"model": name, "input": "hello", "store": false})
        }
        ApiFormat::Messages => serde_json::json!({
            "model": name, "max_tokens": 64,
            "messages": [{"role":"user", "content":"hello"}]
        }),
        ApiFormat::Gemini => panic!("unsupported test client"),
    };
    let parsed =
        parse_client_request(client, Bytes::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let materialized = materialize_execution_routes(
        snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        name,
        name,
        None,
    )
    .unwrap();
    assert!(
        !materialized.routes.is_empty(),
        "{client:?} route for {name}: {:?}; resolved: {resolved:?}",
        materialized.rejections
    );
    materialized.routes.into_iter().next().unwrap()
}

#[test]
fn platform_native_operations_keep_account_authority_and_recheck_persisted_revocation() {
    use crate::platform::{PlatformGroup, PlatformKind};
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
    };
    let (_directory, db, standalone) = fixture();
    let original = standalone
        .credentials
        .iter()
        .find(|c| c.id == "snapshot-http-key")
        .unwrap();
    let connection = connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &original.id);
    let original_chat = endpoint_id_for(
        &connection,
        EndpointOperation::from(Protocol::ChatCompletions),
    )
    .to_string();
    assert!(
        original
            .grants
            .allowed_endpoint_ids
            .contains(&original_chat)
    );
    db.create_platform_account(
        "snapshot-platform",
        PlatformKind::NewApi,
        "Platform",
        "https://api.example.com",
        None,
    )
    .unwrap();
    db.link_platform_account(&original.id, "snapshot-platform", &PlatformGroup::default())
        .unwrap();
    let linked = RoutingSnapshot::load(&db).unwrap();
    let credential = linked
        .credentials
        .iter()
        .find(|c| c.id == original.id)
        .unwrap();
    assert_eq!(
        credential.authorization_connection_id,
        original.authorization_connection_id
    );
    assert_eq!(credential.grants, original.grants);
    let cases = [
        (
            ApiFormat::ChatCompletions,
            Protocol::ChatCompletions,
            "chat/completions",
        ),
        (ApiFormat::Responses, Protocol::Responses, "responses"),
        (ApiFormat::Messages, Protocol::Messages, "messages"),
    ];
    let endpoint_ids: Vec<String> = cases
        .iter()
        .map(|(_, protocol, _)| {
            endpoint_id_for(&connection, EndpointOperation::from(*protocol)).to_string()
        })
        .collect();
    // Linking an existing Key must retain its explicitly saved grants. An
    // ungranted client protocol does not hide the granted Chat route.
    for (client, _, _) in cases {
        let route = route_for_protocol(&linked, "public-model", client);
        assert_eq!(route.plan.upstream, ApiFormat::ChatCompletions);
        assert_eq!(route.target.endpoint_id, original_chat);
        let selected = LiveSendSelection::from_execution(&route, "public-model", "public-model");
        verify_execution_authorization(&linked, &selected, &route.spec, Utc::now(), true).unwrap();
    }
    db.update_credential_binding(
        &credential.binding_id,
        None,
        None,
        Some(&endpoint_ids),
        Some(&credential.grants.allowed_origins),
    )
    .unwrap();
    let authorized = RoutingSnapshot::load(&db).unwrap();
    for (client, protocol, path) in cases {
        let route = route_for_protocol(&authorized, "public-model", client);
        assert_eq!(route.plan.upstream, client);
        assert_eq!(route.plan.model, "upstream-exact");
        assert_eq!(
            route.spec.request_url().unwrap(),
            format!("https://api.example.com/v1/{path}")
        );
        assert_eq!(
            route.target.endpoint_id,
            endpoint_id_for(&connection, EndpointOperation::from(protocol)).to_string()
        );
        let selected = LiveSendSelection::from_execution(&route, "public-model", "public-model");
        verify_execution_authorization(&authorized, &selected, &route.spec, Utc::now(), true)
            .unwrap();
        let remaining: Vec<String> = endpoint_ids
            .iter()
            .filter(|id| **id != route.target.endpoint_id)
            .cloned()
            .collect();
        db.update_credential_binding(
            &credential.binding_id,
            None,
            None,
            Some(&remaining),
            Some(&credential.grants.allowed_origins),
        )
        .unwrap();
        let revoked = RoutingSnapshot::load(&db).unwrap();
        assert!(
            verify_execution_authorization(&revoked, &selected, &route.spec, Utc::now(), true)
                .is_err()
        );
        db.update_credential_binding(
            &credential.binding_id,
            None,
            None,
            Some(&endpoint_ids),
            Some(&credential.grants.allowed_origins),
        )
        .unwrap();
    }
}

#[test]
fn persisted_http_identity_is_independent_of_legacy_kind_and_account_provider_label() {
    let (_directory, _db, mut snapshot) = fixture();
    let before = route(&snapshot, "public-model");
    let destination_id = before.target.destination.id.clone();
    snapshot
        .projection
        .destinations
        .iter_mut()
        .find(|d| d.id == destination_id)
        .unwrap()
        .legacy = LegacyDestinationRef::Dynamic("unrelated-migration-label".into());
    snapshot
        .credentials
        .iter_mut()
        .find(|c| c.id == before.routing.account.id)
        .unwrap()
        .provider_id = "metadata-only".into();
    let after = route(&snapshot, "public-model");
    assert_eq!(before.plan.model, "upstream-exact");
    assert_eq!(before.plan.body, after.plan.body);
    assert_eq!(
        before.spec.request_url().unwrap(),
        after.spec.request_url().unwrap()
    );
    let selected = LiveSendSelection::from_execution(&before, "public-model", "public-model");
    verify_execution_authorization(&snapshot, &selected, &before.spec, Utc::now(), true).unwrap();
}

#[test]
fn route_grants_keep_an_override_authorized_when_a_same_origin_default_is_added() {
    use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};

    let (_directory, db, initial) = fixture();
    let destination_id = initial
        .credentials
        .iter()
        .find(|credential| credential.id == "snapshot-http-key")
        .unwrap()
        .destination_id
        .clone();
    let override_a = "https://api.example.com/anthropic-a/v1/messages";
    let old_catalog = vec![CatalogModel {
        public_model: "public-model".into(),
        upstream_model: "upstream-exact".into(),
        protocols: vec![Protocol::Messages],
        preferred: Some(Protocol::Messages),
        enabled: true,
        upstream_override: Some(DynamicModelUpstreamOverride {
            protocol: Protocol::Messages,
            endpoint_url: override_a.into(),
        }),
    }];
    crate::db::destination_store::replace_destination_catalog(
        &db.conn,
        &destination_id,
        &old_catalog,
    )
    .unwrap();
    let before = RoutingSnapshot::load(&db).unwrap();
    let credential = before
        .credentials
        .iter()
        .find(|credential| credential.id == "snapshot-http-key")
        .unwrap();
    let destination = before
        .projection
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap();
    let connection =
        connection_id_for_legacy(LegacyConnectionKind::CustomAccount, "snapshot-http-key");
    let old_routes = http_configured_routes(destination);
    let old_override_id = assigned_endpoints_for_routes(&connection, &old_routes)
        .into_iter()
        .zip(old_routes.iter())
        .find(|(_, route)| route.url.as_deref() == Some(override_a))
        .unwrap()
        .0
        .id;
    db.update_credential_binding(
        &credential.binding_id,
        Some(&ModelScope::All),
        None,
        Some(&[old_override_id, "unknown-stale-endpoint".into()]),
        Some(&credential.grants.allowed_origins),
    )
    .unwrap();

    let default_messages_b = "https://api.example.com/anthropic/v1/messages";
    let routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: "https://api.example.com/v1".into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: default_messages_b.into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    crate::db::destination_commands::replace_http_destination_with_routes_on(
        &db,
        &destination_id,
        &DynamicProviderDefinition {
            preset_id: None,
            id: "unused".into(),
            name: "HTTP Key".into(),
            endpoint_url: "https://api.example.com/v1".into(),
            upstream_protocol: Protocol::ChatCompletions,
            auth_kind: DynamicAuthKind::Bearer,
            mappings: vec![
                DynamicModelMapping {
                    public_model: "public-model".into(),
                    upstream_model: "upstream-exact".into(),
                    upstream_override: Some(DynamicModelUpstreamOverride {
                        protocol: Protocol::Messages,
                        endpoint_url: override_a.into(),
                    }),
                },
                DynamicModelMapping {
                    public_model: "default-message".into(),
                    upstream_model: "default-message-upstream".into(),
                    upstream_override: None,
                },
            ],
        },
        &[],
        Some(&routes),
    )
    .unwrap();

    let snapshot = RoutingSnapshot::load(&db).unwrap();
    let authorized = route_for_protocol(&snapshot, "public-model", ApiFormat::Messages);
    assert_eq!(authorized.spec.request_url().unwrap(), override_a);
    let selected = LiveSendSelection::from_execution(&authorized, "public-model", "public-model");
    verify_execution_authorization(&snapshot, &selected, &authorized.spec, Utc::now(), true)
        .unwrap();
    let live_credential = snapshot
        .credentials
        .iter()
        .find(|credential| credential.id == "snapshot-http-key")
        .unwrap();
    let current_destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap();
    let expected_override_id =
        assigned_endpoints_for_routes(&connection, &http_configured_routes(current_destination))
            .into_iter()
            .zip(http_configured_routes(current_destination))
            .find(|(_, route)| route.url.as_deref() == Some(override_a))
            .unwrap()
            .0
            .id;
    assert_eq!(
        live_credential.grants.allowed_endpoint_ids,
        vec![expected_override_id]
    );

    let catalog = RuntimeCatalogSnapshot::from_routing(snapshot.clone(), Utc::now());
    let resolved = catalog.resolve("default-message").unwrap();
    let parsed = parse_client_request(
        ApiFormat::Messages,
        Bytes::from_static(
            br#"{"model":"default-message","max_tokens":64,"messages":[{"role":"user","content":"hello"}]}"#,
        ),
    )
    .unwrap();
    let materialized_b = materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        "default-message",
        "default-message",
        None,
    )
    .unwrap();
    assert!(
        materialized_b
            .routes
            .iter()
            .all(|route| { route.spec.request_url().ok().as_deref() != Some(default_messages_b) }),
        "ungranted Messages B must not be selected: {:?}",
        materialized_b.rejections
    );
    assert!(
        materialized_b.rejections.iter().any(|rejection| {
            rejection.code
                == crate::gateway::materialize::RouteRejectionCode::ProductionRouteUnsupported
        }),
        "missing grant is decided before send: {:?}",
        materialized_b.rejections
    );
}

#[test]
fn exact_target_and_credential_changes_reject_the_frozen_attempt() {
    let (_directory, _db, snapshot) = fixture();
    let route = route(&snapshot, "public-model");
    let selected = LiveSendSelection::from_execution(&route, "public-model", "public-model");
    verify_execution_authorization(&snapshot, &selected, &route.spec, Utc::now(), true).unwrap();
    for mutation in 0..11 {
        let mut live = snapshot.clone();
        let c = live
            .credentials
            .iter_mut()
            .find(|c| c.id == selected.account_id)
            .unwrap();
        let destination = live
            .projection
            .destinations
            .iter_mut()
            .find(|d| d.id == c.destination_id)
            .unwrap();
        match mutation {
            0 => c.credential_version += 1,
            1 => c.key_cipher = "rotated-encrypted-material".into(),
            2 => c.enabled = false,
            3 => c.binding_enabled = false,
            4 => c.scope = ModelScope::Only { models: vec![] },
            5 => c.grants.allowed_endpoint_ids.clear(),
            6 => c.grants.allowed_origins.clear(),
            7 => destination.base_url = Some("https://api.example.com/changed/v1".into()),
            8 => destination.catalog[0].upstream_model = "changed-model".into(),
            9 => destination.catalog[0].protocols = vec![Protocol::Messages],
            10 => destination.catalog[0].enabled = false,
            _ => unreachable!(),
        }
        assert!(
            verify_execution_authorization(&live, &selected, &route.spec, Utc::now(), true)
                .is_err(),
            "mutation {mutation}"
        );
    }
    assert_eq!(route.plan.model, "upstream-exact");
    assert_eq!(
        route.spec.request_url().unwrap(),
        "https://api.example.com/v1/chat/completions"
    );
}

#[test]
fn keyless_http_still_rechecks_binding_scope_and_route() {
    let (_directory, _db, mut snapshot) = fixture();
    let id = snapshot
        .credentials
        .iter()
        .find(|c| c.id == "snapshot-http-key")
        .unwrap()
        .destination_id
        .clone();
    snapshot
        .projection
        .destinations
        .iter_mut()
        .find(|d| d.id == id)
        .unwrap()
        .auth_scheme = AuthScheme::None;
    snapshot
        .credentials
        .iter_mut()
        .find(|c| c.id == "snapshot-http-key")
        .unwrap()
        .key_cipher
        .clear();
    let route = route(&snapshot, "public-model");
    let selected = LiveSendSelection::from_execution(&route, "public-model", "public-model");
    verify_execution_authorization(&snapshot, &selected, &route.spec, Utc::now(), true).unwrap();
    snapshot
        .credentials
        .iter_mut()
        .find(|c| c.id == selected.account_id)
        .unwrap()
        .binding_enabled = false;
    assert!(
        verify_execution_authorization(&snapshot, &selected, &route.spec, Utc::now(), true)
            .is_err()
    );
}

#[test]
fn model_resolution_policy_controls_raw_names_without_changing_http_transport() {
    let (_directory, _db, mut snapshot) = fixture();
    let public = route(&snapshot, "public-model");
    let catalog = RuntimeCatalogSnapshot::from_routing(snapshot.clone(), Utc::now());
    assert!(
        crate::alias::resolve_with_runtime_catalogs("upstream-exact", catalog.catalogs()).is_err()
    );
    snapshot
        .projection
        .destinations
        .iter_mut()
        .find(|d| d.id == public.target.destination.id)
        .unwrap()
        .model_resolution = ModelResolution::PublicAndUpstream;
    let raw = route(&snapshot, "upstream-exact");
    assert_eq!(
        raw.spec.request_url().unwrap(),
        public.spec.request_url().unwrap()
    );
    assert_eq!(raw.plan.model, public.plan.model);
}

#[test]
fn malformed_persisted_scope_fails_the_snapshot_instead_of_reconstructing_accounts() {
    let (_directory, db, _) = fixture();
    db.conn.execute("UPDATE credentials SET scope_json = 'invalid-json' WHERE legacy_account_id = 'snapshot-http-key'", []).unwrap();
    assert!(RoutingSnapshot::load(&db).is_err());
}

#[test]
fn disabled_http_rows_keep_raw_identity_ambiguous() {
    let (_directory, _db, mut snapshot) = fixture();
    let index = snapshot
        .projection
        .destinations
        .iter()
        .position(|d| d.adapter == ocg_domain::destination::AdapterKind::Http)
        .unwrap();
    snapshot.projection.destinations[index].model_resolution = ModelResolution::PublicAndUpstream;
    let mut second = snapshot.projection.destinations[index].clone();
    second.id = uuid::Uuid::new_v4().to_string();
    second.catalog[0].public_model = "other-public".into();
    second.catalog[0].enabled = false;
    second.enabled = false;
    snapshot.projection.destinations.push(second);
    let catalog = RuntimeCatalogSnapshot::from_routing(snapshot, Utc::now());
    assert!(matches!(
        crate::alias::resolve_with_runtime_catalogs("upstream-exact", catalog.catalogs()),
        Err(crate::alias::ResolveError::Ambiguous { .. })
    ));
    assert!(catalog.model_has_enabled_protocol("public-model"));
    assert!(!catalog.model_has_enabled_protocol("other-public"));
}

#[test]
fn ollama_coexisting_tags_use_only_explicit_pins() {
    let (_directory, _db, mut snapshot) = fixture();
    let mut destination = ocg_domain::destination::destination_from_legacy(
        &ocg_domain::destination::LegacyDestinationFacts::Builtin {
            provider_id: crate::provider::OLLAMA_PROVIDER_ID.into(),
        },
    )
    .unwrap();
    destination.catalog = ["deepseek-v4-flash:old", "deepseek-v4-flash:new"]
        .into_iter()
        .map(|id| ocg_domain::destination::CatalogModel {
            public_model: id.into(),
            upstream_model: id.into(),
            protocols: vec![Protocol::ChatCompletions],
            preferred: Some(Protocol::ChatCompletions),
            enabled: true,
            upstream_override: None,
        })
        .collect();
    snapshot.projection.destinations.push(destination);
    snapshot.ollama_pinned = vec!["deepseek-v4-flash:new".into()];
    let catalog = RuntimeCatalogSnapshot::from_routing(snapshot, Utc::now());
    let crate::alias::ResolvedModel::Alias { mappings, .. } =
        crate::alias::resolve_with_runtime_catalogs("deepseek-v4-flash", catalog.catalogs())
            .unwrap()
    else {
        panic!("expected shared alias")
    };
    let matches = mappings
        .iter()
        .filter(|m| m.provider_id == crate::provider::OLLAMA_PROVIDER_ID)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].upstream_model, "deepseek-v4-flash:new");
}

#[test]
fn shared_upstream_sibling_cannot_admit_a_disabled_or_out_of_scope_public_model() {
    let (_directory, _db, mut snapshot) = fixture();
    let destination = snapshot
        .projection
        .destinations
        .iter_mut()
        .find(|d| d.adapter == ocg_domain::destination::AdapterKind::Http)
        .unwrap();
    destination.model_resolution = ModelResolution::PublicAndUpstream;
    let mut sibling = destination.catalog[0].clone();
    sibling.public_model = "disabled-public".into();
    sibling.enabled = false;
    destination.catalog.push(sibling);
    let plans = |snapshot: &RoutingSnapshot| {
        let catalog = RuntimeCatalogSnapshot::from_routing(snapshot.clone(), Utc::now());
        let resolved =
            crate::alias::resolve_with_runtime_catalogs("disabled-public", catalog.catalogs())
                .unwrap();
        let parsed = parse_client_request(
            ApiFormat::ChatCompletions,
            Bytes::from_static(
                br#"{"model":"disabled-public","messages":[{"role":"user","content":"hello"}]}"#,
            ),
        )
        .unwrap();
        materialize_execution_routes(
            snapshot,
            &AppConfig::default(),
            &parsed,
            &resolved,
            "disabled-public",
            "disabled-public",
            None,
        )
        .unwrap()
    };
    let catalog = RuntimeCatalogSnapshot::from_routing(snapshot.clone(), Utc::now());
    assert!(!catalog.model_has_enabled_protocol("disabled-public"));
    assert!(plans(&snapshot).routes.is_empty());
    snapshot
        .projection
        .destinations
        .iter_mut()
        .find(|d| d.adapter == ocg_domain::destination::AdapterKind::Http)
        .unwrap()
        .catalog[1]
        .enabled = true;
    snapshot
        .credentials
        .iter_mut()
        .find(|c| c.id == "snapshot-http-key")
        .unwrap()
        .scope = ModelScope::Only {
        models: vec!["public-model".into()],
    };
    let rejected = plans(&snapshot);
    assert!(rejected.routes.is_empty());
    assert!(
        rejected
            .rejections
            .iter()
            .any(|r| r.code == crate::gateway::materialize::RouteRejectionCode::ModelScopeDenied)
    );
    assert_eq!(
        route(&snapshot, "public-model").target.model.public_model,
        "public-model"
    );
}

#[test]
fn raw_pin_rejects_distinct_routes_inside_one_destination_but_keeps_identical_aliases() {
    let (_directory, _db, mut snapshot) = fixture();
    let destination = snapshot
        .projection
        .destinations
        .iter_mut()
        .find(|d| d.adapter == ocg_domain::destination::AdapterKind::Http)
        .unwrap();
    destination.model_resolution = ModelResolution::PublicAndUpstream;
    let mut sibling = destination.catalog[0].clone();
    sibling.public_model = "second-public".into();
    destination.catalog.push(sibling);
    let identical = RuntimeCatalogSnapshot::from_routing(snapshot.clone(), Utc::now());
    assert!(identical.resolve("upstream-exact").is_ok());
    let destination = snapshot
        .projection
        .destinations
        .iter_mut()
        .find(|d| d.adapter == ocg_domain::destination::AdapterKind::Http)
        .unwrap();
    destination.catalog[1].upstream_override =
        Some(ocg_domain::dynamic::DynamicModelUpstreamOverride {
            protocol: Protocol::ChatCompletions,
            endpoint_url: "https://api.example.com/other/v1".into(),
        });
    destination.catalog[1].enabled = false;
    let distinct = RuntimeCatalogSnapshot::from_routing(snapshot, Utc::now());
    assert!(matches!(
        distinct.resolve("upstream-exact"),
        Err(crate::alias::ResolveError::Ambiguous { .. })
    ));
    assert!(!distinct.model_has_enabled_protocol("upstream-exact"));
    assert!(distinct.resolve("public-model").is_ok());
    assert!(distinct.resolve("second-public").is_ok());
}

#[test]
fn quota_probe_overlay_matches_version_cipher_and_epoch() {
    use crate::quota_recovery::{PersistedQuotaRecovery, QuotaEpisode};
    use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};
    use std::collections::HashMap;
    let (_directory, db, mut snapshot) = fixture();
    let current = snapshot
        .credentials
        .iter()
        .find(|credential| credential.id == "snapshot-http-key")
        .unwrap()
        .clone();
    let recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        Utc::now(),
        None,
    );
    let episode = QuotaEpisode {
        credential_id: current.credential_id.clone(),
        account_id: current.id.clone(),
        credential_version: current.credential_version,
        epoch: recovery.epoch,
        key_cipher: current.key_cipher.clone(),
    };
    assert!(crate::db::quota_recovery::save_on(&db.conn, &episode, &recovery).unwrap());
    snapshot = RoutingSnapshot::load(&db).unwrap();
    let mut probes = HashMap::new();
    probes.insert(episode.credential_id.clone(), episode.clone());
    snapshot.apply_quota_probes(&probes);
    assert!(
        snapshot
            .credentials
            .iter()
            .find(|credential| credential.id == "snapshot-http-key")
            .unwrap()
            .quota_probe
    );

    let mut stale = episode.clone();
    stale.key_cipher = "replacement-cipher".into();
    probes.insert(episode.credential_id.clone(), stale);
    snapshot.apply_quota_probes(&probes);
    assert!(
        !snapshot
            .credentials
            .iter()
            .find(|credential| credential.id == "snapshot-http-key")
            .unwrap()
            .quota_probe
    );

    db.rotate_account_credential("snapshot-http-key", "rotated-cipher")
        .unwrap();
    snapshot = RoutingSnapshot::load(&db).unwrap();
    probes.insert(episode.credential_id.clone(), episode);
    snapshot.apply_quota_probes(&probes);
    let rotated = snapshot
        .credentials
        .iter()
        .find(|credential| credential.id == "snapshot-http-key")
        .unwrap();
    assert!(rotated.quota_recovery.is_none());
    assert!(!rotated.quota_probe);
}
