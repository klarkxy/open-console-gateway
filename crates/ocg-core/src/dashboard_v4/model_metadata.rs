//! Explicit per-route model declarations for directories that only return IDs.
use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::model_metadata::{self, ModelMetadata};
use crate::state::CoreState;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationModelMetadataEntry {
    pub public_model: String,
    pub upstream_model: String,
    pub metadata: ModelMetadata,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationModelMetadata {
    pub revision: ControlRevision,
    pub destination_id: String,
    pub models: Vec<DestinationModelMetadataEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationModelMetadataUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub public_model: String,
    /// null removes the operator declaration and reveals discovered facts.
    pub metadata: Option<ModelMetadata>,
}

pub(super) async fn get(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<DestinationModelMetadata>, V3ApiError> {
    let _settings = state.settings_update.lock();
    payload(&state, &id).map(Json)
}

pub(super) async fn put(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationModelMetadata>, V3ApiError> {
    let input = parse_mutation_json::<DestinationModelMetadataUpdate>(&body)?;
    // A missing member is not an implicit delete. Reset requires explicit null.
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(V3ApiError::internal)?;
    if json.get("metadata").is_none() {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "metadata is required (use null to reset)",
        ));
    }
    if let Some(metadata) = &input.metadata {
        metadata
            .validate()
            .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    }
    let _settings = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&state.db.lock())
        .map_err(V3ApiError::internal)?;
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| V3ApiError::not_found_at(&state, "destination not found"))?;
    let model = destination
        .catalog
        .iter()
        .find(|m| m.public_model == input.public_model)
        .ok_or_else(|| V3ApiError::not_found_at(&state, "exact public model not found"))?;
    state
        .commit_configuration_update(|db| {
            model_metadata::declare(db, destination, model, input.metadata)
        })
        .map_err(V3ApiError::internal)?;
    payload(&state, &id).map(Json)
}

fn payload(state: &CoreState, id: &str) -> Result<DestinationModelMetadata, V3ApiError> {
    let db = state.db.lock();
    let snapshot =
        crate::routing_snapshot::RoutingSnapshot::load(&db).map_err(V3ApiError::internal)?;
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "destination not found"))?;
    let records = model_metadata::load(&db).map_err(V3ApiError::internal)?;
    let models = destination
        .catalog
        .iter()
        .map(|model| {
            let (metadata, source) = model_metadata::effective(&records, destination, model);
            DestinationModelMetadataEntry {
                public_model: model.public_model.clone(),
                upstream_model: model.upstream_model.clone(),
                metadata,
                source: source.to_string(),
            }
        })
        .collect();
    Ok(DestinationModelMetadata {
        revision: ControlRevision::from_state(state),
        destination_id: id.to_string(),
        models,
    })
}
