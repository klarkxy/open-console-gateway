#![allow(dead_code)]

use chrono::Utc;
use ocg_core::dashboard_v3::OfficialProtocolBaseline;
use ocg_core::kernel::ids::is_free_model;
use ocg_core::kernel::protocol::{ApiFormat, supported_model_protocol_profiles};
use ocg_core::provider::{OPENCODE_GO_BASE_URL, OPENCODE_PROVIDER_ID, UpstreamProtocolKind};
use ocg_core::provider_contracts::{CATALOG_SOURCE_OPENCODE_MODELS, ContractScope};
use ocg_core::state::CoreStateInner;

/// Persist a refreshed OpenCode Go `/models` snapshot plus official-docs
/// protocols. Tests use this instead of leftover checked-in catalog seeds.
pub(crate) fn persist_refreshed_go_catalog(state: &CoreStateInner) {
    let now = Utc::now();
    let profiles: Vec<_> = supported_model_protocol_profiles()
        .filter(|(id, _, supported)| {
            *id != "big-pickle" && !is_free_model(id) && !supported.is_empty()
        })
        .collect();
    let models: Vec<String> = profiles
        .iter()
        .map(|(id, _, _)| (*id).to_string())
        .collect();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &models,
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            OPENCODE_GO_BASE_URL,
            now,
        )
        .unwrap();
    // A deterministic mock docs snapshot for protocol-conversion tests;
    // this is not a claim about the current upstream documentation.
    let baseline = OfficialProtocolBaseline::mapped(profiles.iter().map(|(id, preferred, _)| {
        let protocol = match preferred {
            ApiFormat::Messages => UpstreamProtocolKind::Messages,
            ApiFormat::Responses => UpstreamProtocolKind::Responses,
            ApiFormat::ChatCompletions => UpstreamProtocolKind::ChatCompletions,
            ApiFormat::Gemini => unreachable!("Gemini is client-only"),
        };
        (*id, protocol)
    }));
    state
        .db
        .lock()
        .apply_official_protocol_baseline(&scope, &models, &baseline, now)
        .unwrap();
    // These scenarios exercise operator-enabled passthrough as well as the
    // documented conversion default. Keep that setup explicit after refresh.
    let enabled = profiles
        .iter()
        .flat_map(|(id, _, supported)| {
            supported.iter().map(move |format| {
                let protocol = match format {
                    ApiFormat::ChatCompletions => UpstreamProtocolKind::ChatCompletions,
                    ApiFormat::Responses => UpstreamProtocolKind::Responses,
                    ApiFormat::Messages => UpstreamProtocolKind::Messages,
                    ApiFormat::Gemini => unreachable!("Gemini is client-only"),
                };
                (
                    (*id).to_string(),
                    protocol,
                    ocg_core::provider_contracts::ProtocolOverrideState::ForceOn,
                )
            })
        })
        .collect::<Vec<_>>();
    state
        .db
        .lock()
        .set_model_protocol_overrides(&scope, &enabled, now)
        .unwrap();
    state.reload_provider_contracts().unwrap();
}

/// Explicit ready Zen routes for suites concerned with fallback and snapshots.
pub(crate) fn persist_enabled_zen_catalog(state: &CoreStateInner) {
    use ocg_core::provider::OPENCODE_ZEN_FREE_PROVIDER_ID;
    use ocg_core::provider_contracts::ProtocolOverrideState;
    let now = Utc::now();
    let models: Vec<String> = [
        "deepseek-v4-flash-free",
        "mimo-v2.5-free",
        "nemotron-3-ultra-free",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    state
        .activate_zen_free_model_catalog(ocg_core::kernel::zen::ZenFreeModelCatalog {
            models: models.clone(),
            refreshed_at: Some(now),
            ..Default::default()
        })
        .unwrap();
    state
        .db
        .lock()
        .set_model_protocol_overrides(
            &ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
            &models
                .into_iter()
                .map(|id| {
                    (
                        id,
                        UpstreamProtocolKind::ChatCompletions,
                        ProtocolOverrideState::ForceOn,
                    )
                })
                .collect::<Vec<_>>(),
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();
}

pub(crate) fn persist_provider_catalog(state: &CoreStateInner, provider_id: &str, models: &[&str]) {
    let now = Utc::now();
    state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(provider_id),
            &models
                .iter()
                .map(|id| (*id).to_string())
                .collect::<Vec<_>>(),
            Some(now),
            "test_refreshed_catalog",
            "https://example.test/models",
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();
}
