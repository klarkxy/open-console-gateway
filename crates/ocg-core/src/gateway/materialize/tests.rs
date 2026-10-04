use super::*;
use crate::alias::{self, ResolvedModel, RuntimeCatalogs};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::gateway::protocol::{
    ApiFormat, is_known_model, materialize_parsed_request, parse_client_request,
};
use crate::models::{Account, AccountSetupStep, AccountType, AppConfig, UpstreamChannel};
use crate::provider::{
    CUSTOM_PROVIDER_ID, CredentialKind, KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, QuotaScope, UpstreamProtocolKind,
};
use crate::routing_snapshot::{ExecutionCredential, RoutingSnapshot};
use bytes::Bytes;
use chrono::Utc;
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::ModelScope;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, LegacyDestinationRef, ModelResolution,
    destination_id_for_builtin, destination_id_for_custom_account,
    destination_id_for_platform_account, sealed_capabilities,
};
use serde_json::json;
use std::sync::Arc;

fn chat_body(model: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap(),
    )
}

fn account(
    id: &str,
    provider_id: &str,

    credential_kind: CredentialKind,
    quota_scope: QuotaScope,
) -> Account {
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    Account {
        id: id.into(),
        provider_id: provider_id.into(),

        credential_kind,
        quota_scope,
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt("key").unwrap(),
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
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn r01_ambiguous_raw_model_id_fails_closed_without_outbound() {
    let error = protocol_error_from_resolve(crate::alias::ResolveError::Ambiguous {
        requested: "shared-raw".into(),
        mappings: vec![
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),

                upstream_model: "shared-raw".into(),
                routeable: true,
            },
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID.to_string(),

                upstream_model: "shared-raw".into(),
                routeable: true,
            },
        ],
    });
    assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(error.code, Some(crate::alias::AMBIGUOUS_MODEL_ID));
    assert!(error.message.contains("shared-raw"), "{}", error.message);
}

#[test]
fn parse_helpers_are_reexported_for_adapters() {
    let parsed = parse_client(ApiFormat::ChatCompletions, chat_body("glm-5.2")).unwrap();
    assert_eq!(parsed.requested_model, "glm-5.2");
    let gemini = parse_gemini(
        "glm-5.2".into(),
        false,
        Bytes::from(
            serde_json::to_vec(&json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]}))
                .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(gemini.client, ApiFormat::Gemini);
}

#[test]
fn materialize_keeps_client_name_and_mapped_upstream_alias() {
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "client-opus",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::Messages, body).unwrap();
    let plan = materialize_parsed_request(
        &parsed,
        &MaterializeSpec {
            client_model: parsed.requested_model.clone(),
            upstream_model: "glm-5.2".into(),
            resolved_alias: Some("glm-5.2".into()),
            channel: UpstreamChannel::Go,
            upstream_base_override: None,
            original_model: None,
            forced_upstream: None,
            custom_route: None,
            effort_aliases: &[],
        },
    )
    .unwrap();
    let identity = native_log_identity(&plan);
    assert_eq!(identity.requested_model, "client-opus");
    assert_eq!(identity.resolved_alias.as_deref(), Some("glm-5.2"));
    assert_eq!(identity.upstream_model, "glm-5.2");
}

fn mapping(provider_id: &str, model: &str) -> crate::alias::ProviderMapping {
    crate::alias::ProviderMapping {
        provider_id: provider_id.to_string(),
        upstream_model: model.into(),
        routeable: true,
    }
}

fn test_destination(adapter: AdapterKind, legacy: LegacyDestinationRef) -> Destination {
    let id = match &legacy {
        LegacyDestinationRef::Builtin(id) | LegacyDestinationRef::Dynamic(id) => {
            destination_id_for_builtin(id)
        }
        LegacyDestinationRef::CustomAccount(id) => destination_id_for_custom_account(id),
        LegacyDestinationRef::PlatformParent(id) => destination_id_for_platform_account(id),
    };
    let model_resolution = match &legacy {
        LegacyDestinationRef::Builtin(_) => ModelResolution::AdapterDefined,
        LegacyDestinationRef::Dynamic(_) => ModelResolution::PublicAndUpstream,
        LegacyDestinationRef::CustomAccount(_) | LegacyDestinationRef::PlatformParent(_) => {
            ModelResolution::PublicOnly
        }
    };
    Destination {
        id,
        legacy,
        adapter,
        name: "dest".into(),
        brand_family: None,
        base_url: None,
        protocols: Vec::new(),
        protocol_routes: Vec::new(),
        auth_scheme: AuthScheme::Bearer,
        model_resolution,
        catalog: Vec::new(),
        capabilities: sealed_capabilities(adapter),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
    }
}

#[test]
fn http_targets_keep_public_separators_and_exact_upstream_identity() {
    use ocg_domain::destination::CatalogModel;
    let mut destination = test_destination(
        AdapterKind::Http,
        LegacyDestinationRef::Dynamic("lab".into()),
    );
    destination.catalog = [
        ("lab_model", "vendor/lab_model"),
        ("lab model", "vendor/lab model"),
        ("lab/model", "vendor/lab/model"),
        ("lab-model", "vendor/lab-model"),
    ]
    .into_iter()
    .map(|(public, upstream)| CatalogModel {
        public_model: public.into(),
        upstream_model: upstream.into(),
        protocols: vec![UpstreamProtocolKind::ChatCompletions],
        preferred: Some(UpstreamProtocolKind::ChatCompletions),
        enabled: true,
        upstream_override: None,
    })
    .collect();
    for resolution in [
        ModelResolution::PublicOnly,
        ModelResolution::PublicAndUpstream,
    ] {
        destination.model_resolution = resolution;
        for (selected_index, selected) in destination.catalog.iter().enumerate() {
            let provider = if resolution == ModelResolution::PublicOnly {
                CUSTOM_PROVIDER_ID
            } else {
                &destination.id
            };
            let resolved = ResolvedModel::PinnedRaw {
                requested: selected.public_model.clone(),
                mapping: mapping(provider, &selected.upstream_model),
            };
            for requested in [
                selected.public_model.clone(),
                selected.public_model.to_uppercase(),
            ] {
                for (index, candidate) in destination.catalog.iter().enumerate() {
                    assert_eq!(
                        resolved_contains_model(&resolved, &destination, candidate, &requested),
                        index == selected_index,
                        "{resolution:?}: {requested} matched {}",
                        candidate.public_model
                    );
                }
            }
            if resolution == ModelResolution::PublicAndUpstream {
                for (index, candidate) in destination.catalog.iter().enumerate() {
                    assert_eq!(
                        resolved_contains_model(
                            &resolved,
                            &destination,
                            candidate,
                            &selected.upstream_model
                        ),
                        index == selected_index,
                        "canonical upstream {} matched {}",
                        selected.upstream_model,
                        candidate.upstream_model
                    );
                }
            }
        }
    }
}

#[test]
fn custom_catalog_mapping_is_not_a_dynamic_provider_id() {
    let custom_mapping = mapping(CUSTOM_PROVIDER_ID, "local-custom");
    let go_mapping = mapping(crate::provider::OPENCODE_PROVIDER_ID, "glm-5.2");
    let dynamic_mapping = mapping("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "dyn-model");
    assert!(mapping_is_custom_http_catalog(&custom_mapping));
    assert!(!mapping_is_custom_http_catalog(&dynamic_mapping));
    assert!(!mapping_is_custom_http_catalog(&go_mapping));
}

#[test]
fn live_planner_rejects_disabled_credential_and_binding() {
    let (mut snapshot, resolved) = cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "minimax-off",
        "MiniMax-New",
        &[UpstreamProtocolKind::ChatCompletions],
        UpstreamProtocolKind::ChatCompletions,
    );
    snapshot.credentials[0].enabled = false;
    let parsed =
        parse_client_request(ApiFormat::ChatCompletions, chat_body("MiniMax-New")).unwrap();
    let config = AppConfig::default();
    let disabled = materialize_execution_routes(
        &snapshot,
        &config,
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert!(disabled.routes.is_empty());
    assert_eq!(
        disabled.rejections[0].code,
        RouteRejectionCode::CredentialDisabled
    );

    snapshot.credentials[0].enabled = true;
    snapshot.credentials[0].binding_enabled = false;
    let unbound = materialize_execution_routes(
        &snapshot,
        &config,
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert!(unbound.routes.is_empty());
    assert_eq!(
        unbound.rejections[0].code,
        RouteRejectionCode::BindingDisabled
    );
}

fn cn_catalog_snapshot(
    adapter: AdapterKind,
    provider_id: &str,
    account_id: &str,
    model: &str,
    protocols: &[UpstreamProtocolKind],
    preferred: UpstreamProtocolKind,
) -> (RoutingSnapshot, ResolvedModel) {
    let mut destination =
        test_destination(adapter, LegacyDestinationRef::Builtin(provider_id.into()));
    destination.catalog = vec![CatalogModel {
        public_model: model.into(),
        upstream_model: model.into(),
        protocols: protocols.to_vec(),
        preferred: Some(preferred),
        enabled: true,
        upstream_override: None,
    }];
    let account = account(
        account_id,
        provider_id,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let mut credential = ExecutionCredential::from(&account);
    credential.destination_id = destination.id.clone();
    let connection = connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, provider_id);
    credential.authorization_connection_id = connection.to_string();
    credential.binding_id = format!("binding-{account_id}");
    credential.grants.allowed_endpoint_ids = protocols
        .iter()
        .copied()
        .map(|protocol| {
            ocg_domain::connection::endpoint_id_for(
                &connection,
                ocg_domain::connection::EndpointOperation::from(protocol),
            )
            .to_string()
        })
        .collect();
    let model_id = destination.catalog[0].public_model.clone();
    let resolved = match adapter {
        AdapterKind::Minimax => alias::resolve_with_runtime_catalogs(
            model,
            RuntimeCatalogs {
                minimax: std::slice::from_ref(&model_id),
                ..RuntimeCatalogs::default()
            },
        ),
        AdapterKind::Kimi => alias::resolve_with_runtime_catalogs(
            model,
            RuntimeCatalogs {
                kimi: std::slice::from_ref(&model_id),
                ..RuntimeCatalogs::default()
            },
        ),
        _ => panic!("cn catalog fixture is MiniMax/Kimi only"),
    }
    .unwrap();
    (
        RoutingSnapshot {
            projection: crate::destination_projection::DestinationProjection {
                destinations: vec![destination],
                credentials: Vec::new(),
            },
            credentials: vec![credential],
            ollama_pinned: Vec::new(),
        },
        resolved,
    )
}

fn materialize_cn_catalog(
    snapshot: &RoutingSnapshot,
    resolved: &ResolvedModel,
    client: ApiFormat,
    model: &str,
    body: Bytes,
) -> ExecutionRouteSet {
    let parsed = parse_client_request(client, body).unwrap();
    materialize_execution_routes(
        snapshot,
        &AppConfig::default(),
        &parsed,
        resolved,
        model,
        model,
        None,
    )
    .unwrap()
}

#[test]
fn catalog_cn_preferred_protocol_precedes_same_client() {
    assert!(!is_known_model("MiniMax-New"));
    assert!(!is_known_model("kimi-for-coding"));

    let (minimax, resolved) = cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "minimax-fresh",
        "MiniMax-New",
        &[
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Messages,
            UpstreamProtocolKind::Responses,
        ],
        UpstreamProtocolKind::Messages,
    );
    let chat = materialize_cn_catalog(
        &minimax,
        &resolved,
        ApiFormat::ChatCompletions,
        "MiniMax-New",
        chat_body("MiniMax-New"),
    );
    assert_eq!(chat.routes.len(), 1, "{:?}", chat.rejections);
    assert!(
        chat.rejections.is_empty(),
        "a successful preferred plan records no alternate attempt: {:?}",
        chat.rejections
    );
    assert_eq!(chat.routes[0].plan.client, ApiFormat::ChatCompletions);
    assert_eq!(
        chat.routes[0].plan.upstream,
        ApiFormat::Messages,
        "enabled preferred Messages wins over the Chat client"
    );
    let converted: serde_json::Value = serde_json::from_slice(&chat.routes[0].plan.body).unwrap();
    assert_eq!(converted["messages"][0]["role"], "user");

    let responses_body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "MiniMax-New",
            "input": "hi",
            "store": false,
            "vendor_extension": {"trace_id": "trace_responses"},
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": {"type": "object"}
                }
            }
        }))
        .unwrap(),
    );
    let responses = materialize_cn_catalog(
        &minimax,
        &resolved,
        ApiFormat::Responses,
        "MiniMax-New",
        responses_body,
    );
    assert_eq!(responses.routes.len(), 1, "{:?}", responses.rejections);
    assert!(
        responses.rejections.is_empty(),
        "feature fallback stays local: {:?}",
        responses.rejections
    );
    assert_eq!(
        responses.routes[0].plan.upstream,
        ApiFormat::Responses,
        "preferred Messages cannot keep Responses json_schema, so the client protocol is used"
    );
    let upstream: serde_json::Value =
        serde_json::from_slice(&responses.routes[0].plan.body).unwrap();
    assert_eq!(upstream["text"]["format"]["type"], "json_schema");
    assert_eq!(
        upstream["vendor_extension"]["trace_id"], "trace_responses",
        "the same-protocol fallback keeps unknown native fields"
    );

    let (kimi, kimi_resolved) = cn_catalog_snapshot(
        AdapterKind::Kimi,
        KIMI_PROVIDER_ID,
        "kimi-fresh",
        "kimi-for-coding",
        &[
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Messages,
        ],
        UpstreamProtocolKind::ChatCompletions,
    );
    let kimi_chat = materialize_cn_catalog(
        &kimi,
        &kimi_resolved,
        ApiFormat::ChatCompletions,
        "kimi-for-coding",
        chat_body("kimi-for-coding"),
    );
    assert_eq!(kimi_chat.routes.len(), 1, "{:?}", kimi_chat.rejections);
    assert_eq!(
        kimi_chat.routes[0].plan.upstream,
        ApiFormat::ChatCompletions
    );
}

#[test]
fn only_actual_candidate_conversion_failures_are_client_errors() {
    let (mut snapshot, resolved) = cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "minimax-json",
        "MiniMax-New",
        &[UpstreamProtocolKind::Messages],
        UpstreamProtocolKind::Messages,
    );
    let body = Bytes::from(serde_json::to_vec(&json!({
        "model": "MiniMax-New", "messages": [{"role":"user", "content":"hi"}],
        "response_format": {"type":"json_schema", "json_schema":{"name":"answer", "schema":{"type":"object"}}}
    })).unwrap());
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body).unwrap();
    let config = AppConfig::default();
    let result = materialize_execution_routes(
        &snapshot,
        &config,
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    );
    let error = result
        .err()
        .expect("unsupported conversion must remain a client error");
    assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
    snapshot.credentials[0].key_cipher.clear();
    let unavailable = materialize_execution_routes(
        &snapshot,
        &config,
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert!(unavailable.routes.is_empty());
    assert_eq!(
        unavailable.rejections[0].code,
        RouteRejectionCode::CandidateMaterializationFailed
    );
}

fn http_separator_snapshot(scope: ModelScope) -> (RoutingSnapshot, ResolvedModel) {
    let account = account(
        "http-separators",
        CUSTOM_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let mut destination = test_destination(
        AdapterKind::Http,
        LegacyDestinationRef::CustomAccount(account.id.clone()),
    );
    destination.base_url = Some("https://lab.example/v1/chat/completions".into());
    destination.protocols = vec![UpstreamProtocolKind::ChatCompletions];
    destination.catalog = ["vendor/model", "vendor-model", "vendor_model"]
        .into_iter()
        .enumerate()
        .map(|(index, public)| CatalogModel {
            public_model: public.into(),
            upstream_model: format!("upstream-{index}"),
            protocols: vec![UpstreamProtocolKind::ChatCompletions],
            preferred: Some(UpstreamProtocolKind::ChatCompletions),
            enabled: true,
            upstream_override: None,
        })
        .collect();
    let connection = connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account.id);
    let mut credential = ExecutionCredential::from(&account);
    credential.destination_id = destination.id.clone();
    credential.authorization_connection_id = connection.to_string();
    credential.binding_id = "binding-http-separators".into();
    credential.scope = scope;
    credential.grants.allowed_endpoint_ids = ocg_domain::credential::assigned_endpoints_for_routes(
        &connection,
        &ocg_domain::destination::http_configured_routes(&destination),
    )
    .into_iter()
    .map(|endpoint| endpoint.id)
    .collect();
    credential.grants.allowed_origins = vec!["https://lab.example:443".into()];
    let resolved = ResolvedModel::PinnedRaw {
        requested: "vendor/model".into(),
        mapping: mapping(CUSTOM_PROVIDER_ID, "upstream-0"),
    };
    (
        RoutingSnapshot {
            projection: crate::destination_projection::DestinationProjection {
                destinations: vec![destination],
                credentials: Vec::new(),
            },
            credentials: vec![credential],
            ollama_pinned: Vec::new(),
        },
        resolved,
    )
}

#[test]
fn model_scope_keeps_separator_identity_through_live_authorization() {
    use crate::gateway::forwarder::{LiveSendSelection, verify_execution_authorization};
    let (snapshot, _) = http_separator_snapshot(ModelScope::Only {
        models: vec!["vendor/model".into()],
    });
    let config = AppConfig::default();
    let wall = Utc::now();
    for requested in ["vendor/model", "Vendor/Model"] {
        let resolved = ResolvedModel::PinnedRaw {
            requested: requested.into(),
            mapping: mapping(CUSTOM_PROVIDER_ID, "upstream-0"),
        };
        let parsed =
            parse_client_request(ApiFormat::ChatCompletions, chat_body(requested)).unwrap();
        let set = materialize_execution_routes(
            &snapshot, &config, &parsed, &resolved, requested, requested, None,
        )
        .unwrap();
        assert_eq!(set.routes.len(), 1, "{requested}: {:?}", set.rejections);
        assert_eq!(set.routes[0].target.model.public_model, "vendor/model");
        let selection = LiveSendSelection::from_execution(&set.routes[0], requested, requested);
        verify_execution_authorization(&snapshot, &selection, &set.routes[0].spec, wall, true)
            .unwrap_or_else(|error| panic!("{requested} live auth: {error:?}"));
    }
    for requested in ["vendor-model", "vendor_model"] {
        let resolved = ResolvedModel::PinnedRaw {
            requested: requested.into(),
            mapping: mapping(CUSTOM_PROVIDER_ID, "upstream-1"),
        };
        let parsed =
            parse_client_request(ApiFormat::ChatCompletions, chat_body(requested)).unwrap();
        let set = materialize_execution_routes(
            &snapshot, &config, &parsed, &resolved, requested, requested, None,
        )
        .unwrap();
        assert!(
            set.routes.is_empty(),
            "{requested} must stay outside the allow-list"
        );
        assert!(
            set.rejections
                .iter()
                .any(|rejection| rejection.code == RouteRejectionCode::ModelScopeDenied),
            "{requested}: {:?}",
            set.rejections
        );
    }

    let (mut open, _) = http_separator_snapshot(ModelScope::All);
    let requested = "vendor-model";
    let resolved = ResolvedModel::PinnedRaw {
        requested: requested.into(),
        mapping: mapping(CUSTOM_PROVIDER_ID, "upstream-1"),
    };
    let parsed = parse_client_request(ApiFormat::ChatCompletions, chat_body(requested)).unwrap();
    let set = materialize_execution_routes(
        &open, &config, &parsed, &resolved, requested, requested, None,
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejections);
    assert_eq!(set.routes[0].target.model.public_model, "vendor-model");
    open.credentials[0].scope = ModelScope::Only {
        models: vec!["vendor/model".into()],
    };
    let selection = LiveSendSelection::from_execution(&set.routes[0], requested, requested);
    assert!(
        verify_execution_authorization(&open, &selection, &set.routes[0].spec, wall, true).is_err(),
        "live authorization must not widen vendor/model to vendor-model"
    );
}

#[test]
fn granted_messages_endpoint_is_selected_for_a_chat_request() {
    use crate::gateway::forwarder::{LiveSendSelection, verify_execution_authorization};
    let (mut snapshot, resolved) = cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "minimax-messages-only",
        "MiniMax-New",
        &[
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Messages,
        ],
        UpstreamProtocolKind::ChatCompletions,
    );
    let connection =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, MINIMAX_PROVIDER_ID);
    let messages = ocg_domain::connection::endpoint_id_for(
        &connection,
        ocg_domain::connection::EndpointOperation::MessageCreate,
    )
    .to_string();
    snapshot.credentials[0].grants.allowed_endpoint_ids = vec![messages.clone()];
    let parsed =
        parse_client_request(ApiFormat::ChatCompletions, chat_body("MiniMax-New")).unwrap();
    let set = materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejections);
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::Messages);
    assert_eq!(set.routes[0].target.endpoint_id, messages);
    let selection = LiveSendSelection::from_execution(&set.routes[0], "MiniMax-New", "MiniMax-New");
    verify_execution_authorization(&snapshot, &selection, &set.routes[0].spec, Utc::now(), true)
        .unwrap_or_else(|error| panic!("granted messages route must pass live auth: {error:?}"));
}

fn builtin_endpoint(provider_id: &str, protocol: UpstreamProtocolKind) -> String {
    let connection = connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, provider_id);
    ocg_domain::connection::endpoint_id_for(
        &connection,
        ocg_domain::connection::EndpointOperation::from(protocol),
    )
    .to_string()
}

#[test]
fn ungranted_preferred_protocol_is_not_executed() {
    let (mut snapshot, resolved) = cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "minimax-chat-grant",
        "MiniMax-New",
        &[
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses,
        ],
        UpstreamProtocolKind::Responses,
    );
    let chat = builtin_endpoint(MINIMAX_PROVIDER_ID, UpstreamProtocolKind::ChatCompletions);
    let responses = builtin_endpoint(MINIMAX_PROVIDER_ID, UpstreamProtocolKind::Responses);
    snapshot.credentials[0].grants.allowed_endpoint_ids = vec![chat.clone()];
    let parsed =
        parse_client_request(ApiFormat::ChatCompletions, chat_body("MiniMax-New")).unwrap();
    let set = materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejections);
    assert!(set.rejections.is_empty(), "{:?}", set.rejections);
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::ChatCompletions);
    assert_eq!(set.routes[0].target.endpoint_id, chat);
    assert_ne!(set.routes[0].target.endpoint_id, responses);
}

#[test]
fn preferred_loss_falls_back_to_same_client_and_keeps_native_fields() {
    let (snapshot, resolved) = cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "minimax-schema",
        "MiniMax-New",
        &[
            UpstreamProtocolKind::Messages,
            UpstreamProtocolKind::ChatCompletions,
        ],
        UpstreamProtocolKind::Messages,
    );
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "MiniMax-New",
            "messages": [
                {"role": "developer", "content": "dev"},
                {"role": "user", "content": "hi"}
            ],
            "vendor_extension": {"trace_id": "trace_chat"},
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "answer", "schema": {"type": "object"}}
            }
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body).unwrap();
    let set = materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejections);
    assert!(
        set.rejections.is_empty(),
        "local fallback must not record a probe or a failed candidate: {:?}",
        set.rejections
    );
    assert_eq!(set.routes[0].plan.client, ApiFormat::ChatCompletions);
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::ChatCompletions);
    assert_eq!(
        set.routes[0].target.endpoint_id,
        builtin_endpoint(MINIMAX_PROVIDER_ID, UpstreamProtocolKind::ChatCompletions)
    );
    let upstream: serde_json::Value = serde_json::from_slice(&set.routes[0].plan.body).unwrap();
    assert_eq!(upstream["response_format"]["type"], "json_schema");
    assert_eq!(upstream["vendor_extension"]["trace_id"], "trace_chat");
    assert_eq!(upstream["messages"][0]["role"], "developer");
    assert_eq!(upstream["messages"][0]["content"], "dev");
}

#[test]
fn unpreservable_protocols_exclude_one_candidate_and_keep_another() {
    let (mut snapshot, resolved) = cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "minimax-lossy",
        "MiniMax-New",
        &[
            UpstreamProtocolKind::Messages,
            UpstreamProtocolKind::Responses,
            UpstreamProtocolKind::ChatCompletions,
        ],
        UpstreamProtocolKind::Messages,
    );
    snapshot.credentials[0].grants.allowed_endpoint_ids = vec![
        builtin_endpoint(MINIMAX_PROVIDER_ID, UpstreamProtocolKind::Messages),
        builtin_endpoint(MINIMAX_PROVIDER_ID, UpstreamProtocolKind::Responses),
    ];
    let native = account(
        "minimax-native",
        MINIMAX_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let mut native_credential = ExecutionCredential::from(&native);
    native_credential.destination_id = snapshot.credentials[0].destination_id.clone();
    native_credential.authorization_connection_id =
        snapshot.credentials[0].authorization_connection_id.clone();
    native_credential.binding_id = "binding-minimax-native".into();
    native_credential.grants.allowed_endpoint_ids = vec![builtin_endpoint(
        MINIMAX_PROVIDER_ID,
        UpstreamProtocolKind::ChatCompletions,
    )];
    snapshot.credentials.push(native_credential);
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "MiniMax-New",
            "messages": [{"role": "user", "content": "hi"}],
            "vendor_extension": {"trace_id": "trace_native"},
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "answer", "schema": {"type": "object"}}
            }
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body).unwrap();
    let set = materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejections);
    assert_eq!(set.routes[0].routing.account.id, "minimax-native");
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::ChatCompletions);
    let upstream: serde_json::Value = serde_json::from_slice(&set.routes[0].plan.body).unwrap();
    assert_eq!(upstream["response_format"]["type"], "json_schema");
    assert_eq!(upstream["vendor_extension"]["trace_id"], "trace_native");
    assert_eq!(set.rejections.len(), 1, "{:?}", set.rejections);
    assert_eq!(
        set.rejections[0].account_id.as_deref(),
        Some("minimax-lossy")
    );
    assert_eq!(
        set.rejections[0].code,
        RouteRejectionCode::CandidateMaterializationFailed
    );
    assert!(
        set.rejections[0].detail.contains("cannot be preserved"),
        "{}",
        set.rejections[0].detail
    );
}

fn messages_history_body(signature: Option<&str>) -> Bytes {
    let mut messages = vec![json!({"role": "user", "content": "hi"})];
    if let Some(signature) = signature {
        messages.push(json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "t", "signature": signature},
                {"type": "text", "text": "ok"}
            ]
        }));
    }
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": "MiniMax-New",
            "max_tokens": 32,
            "messages": messages
        }))
        .unwrap(),
    )
}

fn minimax_messages_snapshot(account_id: &str) -> (RoutingSnapshot, ResolvedModel) {
    cn_catalog_snapshot(
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        account_id,
        "MiniMax-New",
        &[UpstreamProtocolKind::Messages],
        UpstreamProtocolKind::Messages,
    )
}

#[test]
fn production_route_domain_is_stable_for_the_same_observed_route() {
    let (snapshot, resolved) = minimax_messages_snapshot("minimax-domain");
    let first = materialize_cn_catalog(
        &snapshot,
        &resolved,
        ApiFormat::Messages,
        "MiniMax-New",
        messages_history_body(None),
    );
    let second = materialize_cn_catalog(
        &snapshot,
        &resolved,
        ApiFormat::Messages,
        "MiniMax-New",
        messages_history_body(None),
    );
    assert_eq!(first.routes.len(), 1, "{:?}", first.rejections);
    let domain = first.routes[0]
        .plan
        .replay_domain
        .expect("production materialize sets a route domain");
    assert_eq!(
        domain.hex(),
        second.routes[0].plan.replay_domain.unwrap().hex()
    );
    assert_eq!(domain.hex().len(), 64);
}

#[test]
fn native_history_restores_only_for_the_matching_credential() {
    let (mut snapshot, resolved) = minimax_messages_snapshot("minimax-a");
    snapshot.credentials[0].credential_id = "cred-a".into();
    snapshot.credentials[0].credential_version = 1;
    let mut other = snapshot.credentials[0].clone();
    other.id = "minimax-b".into();
    other.credential_id = "cred-b".into();
    other.credential_version = 2;
    other.binding_id = "binding-minimax-b".into();
    let mut only_b = snapshot.clone();
    only_b.credentials = vec![other.clone()];
    let learned = materialize_cn_catalog(
        &only_b,
        &resolved,
        ApiFormat::Messages,
        "MiniMax-New",
        messages_history_body(None),
    );
    let domain = learned.routes[0].plan.replay_domain.unwrap();
    let marked = ocg_gateway::protocol::bind_replay_opaque(domain, "sig-bytes").unwrap();
    snapshot.credentials.push(other);
    let parsed =
        parse_client_request(ApiFormat::Messages, messages_history_body(Some(&marked))).unwrap();
    let set = materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejections);
    assert_eq!(set.routes[0].routing.account.id, "minimax-b");
    assert_eq!(
        set.routes[0].plan.replay_domain.unwrap().hex(),
        domain.hex()
    );
    let upstream: serde_json::Value = serde_json::from_slice(&set.routes[0].plan.body).unwrap();
    assert_eq!(
        upstream["messages"][1]["content"][0]["signature"], "sig-bytes",
        "matching history is restored to the original opaque bytes"
    );
    assert!(
        !upstream.to_string().contains("ocg-replay-v1"),
        "the upstream body is not sent with the local marker"
    );
    assert_eq!(set.rejections.len(), 1, "{:?}", set.rejections);
    assert_eq!(set.rejections[0].account_id.as_deref(), Some("minimax-a"));
    assert!(
        set.rejections[0].detail.contains("does not match"),
        "{}",
        set.rejections[0].detail
    );
}

#[test]
fn unmarked_or_foreign_native_history_is_rejected_before_send() {
    let (snapshot, resolved) = minimax_messages_snapshot("minimax-unmarked");
    let unmarked =
        parse_client_request(ApiFormat::Messages, messages_history_body(Some("raw-sig"))).unwrap();
    let error = expect_pre_send_rejection(materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &unmarked,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    ));
    assert!(error.contains("no route domain"), "{error}");

    let foreign = ocg_gateway::protocol::ReplayDomain::parse(&"ab".repeat(32)).unwrap();
    let marked = ocg_gateway::protocol::bind_replay_opaque(foreign, "sig-bytes").unwrap();
    let mismatched =
        parse_client_request(ApiFormat::Messages, messages_history_body(Some(&marked))).unwrap();
    let error = expect_pre_send_rejection(materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &mismatched,
        &resolved,
        "MiniMax-New",
        "MiniMax-New",
        None,
    ));
    assert!(error.contains("does not match"), "{error}");
}

#[test]
fn exhausted_protocols_keep_transport_and_preservation_rejections() {
    let (snapshot, resolved) = cn_catalog_snapshot(
        AdapterKind::Kimi,
        KIMI_PROVIDER_ID,
        "kimi-mixed",
        "kimi-for-coding",
        &[
            UpstreamProtocolKind::Responses,
            UpstreamProtocolKind::Messages,
        ],
        UpstreamProtocolKind::Responses,
    );
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "kimi-for-coding",
            "max_tokens": 32,
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "t", "signature": "raw-sig"},
                    {"type": "text", "text": "ok"}
                ]}
            ]
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::Messages, body).unwrap();
    let set = materialize_execution_routes(
        &snapshot,
        &AppConfig::default(),
        &parsed,
        &resolved,
        "kimi-for-coding",
        "kimi-for-coding",
        None,
    )
    .expect("a mixed exhaustion remains a route set with no send");
    assert!(set.routes.is_empty(), "{:?}", set.rejections);
    assert_eq!(set.rejections.len(), 2, "{:?}", set.rejections);
    let transport = set
        .rejections
        .iter()
        .find(|rejection| rejection.code == RouteRejectionCode::ProductionRouteUnsupported)
        .expect("unresolvable transport");
    let preserved = set
        .rejections
        .iter()
        .find(|rejection| rejection.code == RouteRejectionCode::CandidateMaterializationFailed)
        .expect("native history preservation");
    assert_eq!(transport.account_id.as_deref(), Some("kimi-mixed"));
    assert_eq!(preserved.account_id.as_deref(), Some("kimi-mixed"));
    assert!(
        transport.detail.contains("no official upstream path"),
        "{}",
        transport.detail
    );
    assert!(
        preserved.detail.contains("no route domain"),
        "{}",
        preserved.detail
    );
    let detail = set
        .incompatibility
        .expect("mixed exhaustion publishes an aggregate detail");
    assert!(detail.contains(transport.detail.as_str()), "{detail}");
    assert!(detail.contains(preserved.detail.as_str()), "{detail}");
}

fn expect_pre_send_rejection(result: Result<ExecutionRouteSet, super::ProtocolError>) -> String {
    match result {
        Err(error) => error.message,
        Ok(set) => panic!(
            "expected a pre-send rejection, got {} routes and {} rejections",
            set.routes.len(),
            set.rejections.len()
        ),
    }
}
