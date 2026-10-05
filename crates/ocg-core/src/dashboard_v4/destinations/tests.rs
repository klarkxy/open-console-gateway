use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::provider::ProviderOrigin;
use crate::state::CoreStateInner;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;
use ocg_domain::destination::{ModelResolution, destination_id_for_dynamic};
use std::sync::Arc;

struct StateDir {
    state: Option<CoreState>,
    dir: Option<std::path::PathBuf>,
}

impl std::ops::Deref for StateDir {
    type Target = CoreState;

    fn deref(&self) -> &Self::Target {
        self.state.as_ref().unwrap()
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        self.state.take();
        if let Some(dir) = self.dir.take() {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

fn dynamic_state() -> StateDir {
    let dir = std::env::temp_dir().join(format!(
        "ocg-v4-destination-mutation-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    db.create_dynamic_provider_definition(&DynamicProviderRuntime {
        preset_id: None,
        id: "destination-edit-provider".into(),
        name: "Before".into(),
        endpoint_url: "https://before.example/v1".into(),
        upstream_protocol: ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: vec![DynamicModelMapping {
            public_model: "public-before".into(),
            upstream_model: "upstream-before".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ProviderOrigin::Custom,
        offering: "api".into(),
    })
    .unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("destination-mutation"));
    StateDir {
        state: Some(Arc::new(
            CoreStateInner::new(db, dir.clone(), cipher).unwrap(),
        )),
        dir: Some(dir),
    }
}

fn expectation(state: &CoreState) -> MutationExpectation {
    MutationExpectation {
        expected_revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn expect_ok<T>(result: Result<T, DestinationsError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("destination mutation unexpectedly failed: {error:?}"),
    }
}

#[test]
fn configurable_destination_patch_round_trips_override_and_delete_requires_no_keys() {
    let state = dynamic_state();
    let destination_id = destination_id_for_dynamic("destination-edit-provider");
    let result = expect_ok(patch_destination_locked(
        &state,
        &destination_id,
        DestinationPatchRequest {
            enabled: None,
            expectation: expectation(&state),
            name: "After".into(),
            endpoint_url: "https://after.example/v1".into(),
            upstream_protocol: ProtocolDto::Responses,
            protocol_routes: None,
            auth_scheme: AuthSchemeDto::XApiKey,
            models: vec![DestinationModelPatch {
                enabled: None,
                public_model: "public-after".into(),
                upstream_model: "upstream-after".into(),
                protocols: None,
                preferred: None,
                upstream_override: Some(super::super::types::DestinationUpstreamOverridePatch {
                    protocol: ProtocolDto::Messages,
                    endpoint_url: "https://alternate.example/messages".into(),
                }),
            }],
            authorize_credential_ids: Vec::new(),
        },
    ));
    assert_eq!(result.destination.name, "After");
    assert_eq!(
        result.destination.model_resolution,
        ModelResolutionDto::PublicAndUpstream
    );
    assert_eq!(
        result.destination.catalog[0]
            .upstream_override
            .as_ref()
            .unwrap()
            .endpoint_url,
        "https://alternate.example/messages"
    );
    let stored = expect_ok(load_destination(&state, &destination_id));
    assert_eq!(stored.model_resolution, ModelResolution::PublicAndUpstream);
    assert_eq!(
        stored.catalog[0]
            .upstream_override
            .as_ref()
            .unwrap()
            .protocol,
        ocg_domain::catalog::UpstreamProtocolKind::Messages
    );

    let deleted = expect_ok(delete_destination_locked(
        &state,
        &destination_id,
        expectation(&state),
    ));
    assert_eq!(deleted.revision.revision, state.settings_revision());
    assert!(load_destination(&state, &destination_id).is_err());
}

#[test]
fn metadata_edit_keeps_disabled_default_protocol_and_explicit_routes() {
    use super::super::types::HttpProtocolRouteDto;
    let state = dynamic_state();
    let id = destination_id_for_dynamic("destination-edit-provider");
    let mut request = DestinationPatchRequest {
        expectation: expectation(&state),
        enabled: None,
        name: "Multi".into(),
        endpoint_url: "https://before.example/v1".into(),
        upstream_protocol: ProtocolDto::ChatCompletions,
        auth_scheme: AuthSchemeDto::Bearer,
        protocol_routes: Some(vec![
            HttpProtocolRouteDto {
                protocol: ProtocolDto::ChatCompletions,
                endpoint_url: "https://before.example/v1".into(),
                auth_scheme: AuthSchemeDto::Bearer,
            },
            HttpProtocolRouteDto {
                protocol: ProtocolDto::Messages,
                endpoint_url: "https://before.example/anthropic/v1/messages".into(),
                auth_scheme: AuthSchemeDto::XApiKey,
            },
        ]),
        models: vec![DestinationModelPatch {
            public_model: "public-before".into(),
            upstream_model: "upstream-before".into(),
            upstream_override: None,
            enabled: Some(true),
            protocols: Some(vec![ProtocolDto::Messages]),
            preferred: Some(ProtocolDto::Messages),
        }],
        authorize_credential_ids: Vec::new(),
    };
    expect_ok(patch_destination_locked(&state, &id, request.clone()));
    request.expectation = expectation(&state);
    request.name = "Renamed".into();
    request.protocol_routes = None;
    request.models[0].protocols = None;
    request.models[0].preferred = None;
    let result = expect_ok(patch_destination_locked(&state, &id, request.clone()));
    assert_eq!(result.destination.protocol_routes.len(), 2);
    assert_eq!(
        result.destination.catalog[0].protocols,
        [ProtocolDto::Messages]
    );
    assert_eq!(
        result.destination.catalog[0].preferred,
        Some(ProtocolDto::Messages)
    );
    request.expectation = expectation(&state);
    request.endpoint_url = "https://changed.example/v1".into();
    assert!(patch_destination_locked(&state, &id, request.clone()).is_err());
    request.expectation = expectation(&state);
    request.endpoint_url = "https://before.example/v1".into();
    request.protocol_routes = Some(vec![
        HttpProtocolRouteDto {
            protocol: ProtocolDto::ChatCompletions,
            endpoint_url: "https://before.example/v1".into(),
            auth_scheme: AuthSchemeDto::Bearer,
        },
        HttpProtocolRouteDto {
            protocol: ProtocolDto::Messages,
            endpoint_url: "https://before.example/anthropic/v1/messages".into(),
            auth_scheme: AuthSchemeDto::XApiKey,
        },
        HttpProtocolRouteDto {
            protocol: ProtocolDto::Responses,
            endpoint_url: "https://before.example/v1/responses".into(),
            auth_scheme: AuthSchemeDto::Bearer,
        },
    ]);
    request.models[0].public_model = " public-before ".into();
    request.models[0].protocols = Some(vec![ProtocolDto::Messages, ProtocolDto::Responses]);
    let expanded = expect_ok(patch_destination_locked(&state, &id, request));
    assert_eq!(expanded.destination.protocol_routes.len(), 3);
    assert_eq!(
        expanded.destination.catalog[0].protocols,
        [ProtocolDto::Messages, ProtocolDto::Responses]
    );
    let stored = expect_ok(load_destination(&state, &id));
    assert_eq!(
        stored.base_url.as_deref(),
        Some("https://before.example/v1")
    );
    assert_eq!(stored.catalog[0].public_model, "public-before");
}

#[test]
fn account_controls_follow_resource_owner_not_credential_capacity() {
    use ocg_domain::destination::{LegacyDestinationFacts, Protocol, destination_from_legacy};
    let custom = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: "custom".into(),
        name: "Custom".into(),
        endpoint_url: "https://example.com/v1".into(),
        protocol: Protocol::ChatCompletions,
        model_capabilities: vec![("public".into(), "upstream".into())],
    })
    .unwrap();
    assert_eq!(custom.max_credentials, None);
    assert_eq!(
        DestinationDto::from(&custom)
            .account_controls
            .configuration_owner,
        AccountConfigurationOwnerDto::Account
    );
    let mut generic = custom.clone();
    generic.legacy = LegacyDestinationRef::Dynamic("provider".into());
    generic.max_credentials = Some(1);
    generic.auth_scheme = AuthScheme::None;
    let controls = DestinationDto::from(&generic).account_controls;
    assert_eq!(
        controls.configuration_owner,
        AccountConfigurationOwnerDto::Destination
    );
    assert_eq!(controls.toggle_write, AccountToggleWriteDto::Account);
}

#[test]
fn account_controls_keep_profile_and_commercial_facts_independent() {
    use ocg_domain::destination::{LegacyDestinationFacts, destination_from_legacy};
    let builtin = |id: &str| {
        destination_from_legacy(&LegacyDestinationFacts::Builtin {
            provider_id: id.into(),
        })
        .unwrap()
    };
    let mut go = builtin("opencode");
    go.capabilities.managed_signup = false;
    let dto = DestinationDto::from(&go);
    assert!(dto.account_controls.browser_profile);
    assert_eq!(
        dto.account_controls.console_link,
        Some(AccountConsoleLinkDto::Opencode)
    );
    assert!(dto.plan.unwrap().expiry_cadence.is_some());
    let zen = DestinationDto::from(&builtin("opencode-zen-free"));
    assert_eq!(
        zen.account_controls.toggle_write,
        AccountToggleWriteDto::ProviderSettings
    );
    assert!(zen.plan.unwrap().expiry_cadence.is_none());
    let cpa = DestinationDto::from(&builtin("cpa"));
    assert!(cpa.plan.is_none());
    assert_eq!(cpa.account_controls.console_link, None);
    assert_eq!(
        DestinationDto::from(&builtin("ollama"))
            .account_controls
            .console_link,
        Some(AccountConsoleLinkDto::Ollama)
    );
}

/// Skip-spawn owned plane. The empty task fills `GatewayHandle`; it is not a child process.
struct OwnedApplyGuard {
    _shutdown_rx: tokio::sync::oneshot::Receiver<()>,
}

impl OwnedApplyGuard {
    fn arm(state: &CoreState) -> Self {
        crate::cpa_execution::set_skip_spawn(true);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(Some("synthetic-management".to_string()));
        crate::cpa_execution::set_before_apply_commit(None);
        crate::cpa_execution::set_artifact_dir(
            state,
            crate::cpa_execution::documented_runtime_dir(),
        );
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async {});
        *state.gateway.lock() = Some(crate::gateway_runtime::GatewayHandle {
            port: 9,
            listen_addr: "127.0.0.1:9".parse().unwrap(),
            dashboard_is_local: true,
            shutdown,
            task,
        });
        Self {
            _shutdown_rx: shutdown_rx,
        }
    }
}

impl Drop for OwnedApplyGuard {
    fn drop(&mut self) {
        crate::cpa_execution::set_skip_spawn(false);
        crate::cpa_execution::set_fail_ready(false);
        crate::cpa_execution::set_test_password(None);
        crate::cpa_execution::set_before_apply_commit(None);
        clear_mutation_receipt_failure();
    }
}

fn plant_malformed_quota_policy(state: &CoreState) {
    state
        .db
        .lock()
        .conn
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![crate::cpa_policy::SETTINGS_KEY, "{"],
        )
        .unwrap();
}

fn rename_request(state: &CoreState, name: &str) -> DestinationPatchRequest {
    DestinationPatchRequest {
        enabled: None,
        expectation: expectation(state),
        name: name.into(),
        endpoint_url: "https://before.example/v1".into(),
        upstream_protocol: ProtocolDto::ChatCompletions,
        protocol_routes: None,
        auth_scheme: AuthSchemeDto::Bearer,
        models: vec![DestinationModelPatch {
            enabled: None,
            public_model: "public-before".into(),
            upstream_model: "upstream-before".into(),
            protocols: None,
            preferred: None,
            upstream_override: None,
        }],
        authorize_credential_ids: Vec::new(),
    }
}

fn patch_bytes(request: &DestinationPatchRequest) -> Bytes {
    Bytes::from(serde_json::to_vec(request).unwrap())
}

async fn started_plane(state: &CoreState) -> crate::cpa_execution::ExecutionReport {
    let started =
        crate::cpa_execution::start(state, state.settings_revision(), state.process_generation())
            .await
            .expect("owned plane start");
    assert_eq!(started.apply_status, "applied");
    assert!(started.desired_running);
    started
}

#[tokio::test(flavor = "current_thread")]
async fn patch_malformed_quota_policy_refuses_before_commit() {
    let state = dynamic_state();
    let core = state.state.clone().unwrap();
    let id = destination_id_for_dynamic("destination-edit-provider");
    let _plane = OwnedApplyGuard::arm(&core);
    let started = started_plane(&core).await;
    let revision = core.settings_revision();
    plant_malformed_quota_policy(&core);
    let error = patch_destination(
        State(core.clone()),
        Path(id.clone()),
        patch_bytes(&rename_request(&core, "After")),
    )
    .await
    .expect_err("malformed quota policy");
    assert!(format!("{error:?}").contains("official quota policy could not be read"));
    assert_eq!(expect_ok(load_destination(&core, &id)).name, "Before");
    assert_eq!(core.settings_revision(), revision);
    let after = crate::cpa_execution::execution_report(&core);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(core.settings_update.try_lock().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn patch_stale_cas_does_not_apply() {
    let state = dynamic_state();
    let core = state.state.clone().unwrap();
    let id = destination_id_for_dynamic("destination-edit-provider");
    let _plane = OwnedApplyGuard::arm(&core);
    let started = started_plane(&core).await;
    let revision = core.settings_revision();
    let mut request = rename_request(&core, "After");
    request.expectation.expected_revision = revision + 1;
    let error = patch_destination(State(core.clone()), Path(id.clone()), patch_bytes(&request))
        .await
        .expect_err("stale destination patch");
    assert!(format!("{error:?}").contains("revisionConflict"));
    assert_eq!(expect_ok(load_destination(&core, &id)).name, "Before");
    assert_eq!(core.settings_revision(), revision);
    let after = crate::cpa_execution::execution_report(&core);
    assert_eq!(after.desired_revision, started.desired_revision);
    assert_eq!(after.applied_revision, started.applied_revision);
    assert_eq!(after.apply_status, started.apply_status);
    assert!(core.settings_update.try_lock().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn patch_receipt_failure_after_commit_still_applies() {
    let state = dynamic_state();
    let core = state.state.clone().unwrap();
    let id = destination_id_for_dynamic("destination-edit-provider");
    let _plane = OwnedApplyGuard::arm(&core);
    let started = started_plane(&core).await;
    let revision = core.settings_revision();
    fail_next_mutation_receipt();
    let error = patch_destination(
        State(core.clone()),
        Path(id.clone()),
        patch_bytes(&rename_request(&core, "After")),
    )
    .await
    .expect_err("receipt read");
    assert!(format!("{error:?}").contains("official quota policy could not be read"));
    assert_eq!(expect_ok(load_destination(&core, &id)).name, "After");
    assert!(core.settings_revision() > revision);
    let applied = crate::cpa_execution::execution_report(&core);
    assert_eq!(applied.desired_revision, started.desired_revision + 1);
    assert_eq!(applied.applied_revision, applied.desired_revision);
    assert_eq!(applied.apply_status, "applied");
    assert!(core.settings_update.try_lock().is_some());
}
