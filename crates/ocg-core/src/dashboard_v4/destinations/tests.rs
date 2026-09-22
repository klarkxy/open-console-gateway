use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::dashboard_v3::MutationExpectation;
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::provider::ProviderOrigin;
use crate::state::CoreStateInner;
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
        Err(_) => panic!("destination mutation unexpectedly failed"),
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
    assert!(patch_destination_locked(&state, &id, request).is_err());
    assert_eq!(
        expect_ok(load_destination(&state, &id)).base_url.as_deref(),
        Some("https://before.example/v1")
    );
}
