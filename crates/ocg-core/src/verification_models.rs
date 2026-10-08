//! Single-request verification selection from the effective saved catalog.

use crate::provider::UpstreamProtocolKind;
use crate::provider_contracts::EffectiveScopeContract;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationModel {
    pub model: String,
    pub protocol: UpstreamProtocolKind,
}

/// An explicit model is never replaced by a different candidate. Automatic
/// selection sorts exact catalog IDs so directory response order is irrelevant.
pub fn select_verification_model(
    scope: &EffectiveScopeContract,
    explicit: Option<&str>,
) -> Result<VerificationModel, String> {
    if !scope.catalog_routable || !scope.production_inference {
        return Err("OpenCode Go has no enabled routable catalog; refresh its model directory and enable a supported model before verifying a Key".into());
    }
    let mut ids: Vec<&str> = match explicit {
        Some(id) => vec![id],
        None => scope.catalog.models.iter().map(String::as_str).collect(),
    };
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        if !scope
            .catalog
            .models
            .iter()
            .any(|saved| saved.eq_ignore_ascii_case(id))
        {
            continue;
        }
        let Some(model) = scope.model(id).filter(|model| model.routable) else {
            continue;
        };
        let supported = |protocol: UpstreamProtocolKind| {
            model
                .protocols
                .get(protocol.as_str())
                .is_some_and(|row| row.available && row.enabled && row.source.confers_support())
        };
        let protocol = supported(model.preferred_protocol)
            .then_some(model.preferred_protocol)
            .or_else(|| {
                UpstreamProtocolKind::ALL
                    .into_iter()
                    .find(|protocol| supported(*protocol))
            });
        if let Some(protocol) = protocol {
            return Ok(VerificationModel {
                model: model.model_id.clone(),
                protocol,
            });
        }
    }
    Err(match explicit {
        Some(id) => format!("model `{id}` has no enabled supported protocol in the saved OpenCode Go catalog; refresh the directory or choose an enabled model"),
        None => "OpenCode Go has no enabled model with supported protocol evidence; refresh its model directory and enable a supported model before verifying a Key".into(),
    })
}

impl VerificationModel {
    /// OpenCode's protocol-derived authentication uses the same isolated
    /// Bearer/XApiKey and JSON/version header primitives as production sends.
    pub fn go_headers(&self, key: &str) -> Result<reqwest::header::HeaderMap, String> {
        let mut headers = crate::custom_http::isolated_inference_headers(
            crate::custom_http::custom_auth_scheme(self.protocol),
            key,
        )
        .map_err(|error| error.to_string())?;
        let extra = crate::custom_http::json_content_headers(
            self.protocol == UpstreamProtocolKind::Messages,
        )
        .map_err(|error| error.to_string())?;
        headers.extend(extra);
        Ok(headers)
    }

    pub fn path(&self) -> &'static str {
        crate::provider_contracts::protocol_to_api(self.protocol)
            .upstream_path()
            .expect("upstream verification protocols have paths")
    }

    pub fn body(&self, message: &str, max_tokens: u32) -> Result<Vec<u8>, String> {
        let encoded = crate::custom::minimal_verification_body(self.protocol, &self.model)
            .map_err(|error| error.message)?;
        let mut body: serde_json::Value =
            serde_json::from_slice(&encoded).map_err(|error| error.to_string())?;
        match self.protocol {
            UpstreamProtocolKind::Responses => {
                body["input"] = message.into();
                body["max_output_tokens"] = max_tokens.max(16).into();
            }
            UpstreamProtocolKind::ChatCompletions | UpstreamProtocolKind::Messages => {
                body["messages"][0]["content"] = message.into();
                body["max_tokens"] = max_tokens.into();
            }
        }
        serde_json::to_vec(&body).map_err(|error| error.to_string())
    }
}

/// Merge keyless discovery with saved controls without writing either snapshot.
pub(crate) fn discovered_go_contract(
    models: Vec<String>,
    baseline: &crate::goat::OfficialProtocolBaseline,
    saved_scope: &EffectiveScopeContract,
    persisted: &crate::provider_contracts::PersistedContracts,
    projection: &crate::destination_projection::DestinationProjection,
    base_url: &str,
) -> Result<EffectiveScopeContract, String> {
    use crate::provider_contracts::ContractScope;
    use crate::provider_contracts::{
        ContractEvidenceSource, PersistedModelProtocol, PersistedScopeRow,
    };
    let scope = ContractScope::provider(crate::provider::OPENCODE_PROVIDER_ID);
    let models: Vec<_> = models
        .into_iter()
        .filter(|id| !crate::gateway::free_models::is_free_model(id))
        .collect();
    if models.is_empty() {
        return Err("OpenCode Go directory returned no paid models; refresh its model directory and retry Key verification".into());
    }
    let now = chrono::Utc::now();
    let mut persisted = persisted.clone();
    persisted.scopes.insert(
        scope.clone(),
        PersistedScopeRow {
            scope: scope.clone(),
            catalog_models: models.clone(),
            catalog_refreshed_at: Some(now),
            catalog_source: crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS.into(),
            catalog_source_url: crate::goat::opencode_go_models_url_for_base(base_url),
            revision: saved_scope.revision,
            updated_at: now,
        },
    );
    let evidence = persisted.evidence.entry(scope.clone()).or_default();
    for id in &models {
        for protocol in baseline
            .protocols_for(crate::provider::OPENCODE_PROVIDER_ID, id)
            .unwrap_or_default()
        {
            evidence
                .retain(|row| !(row.model_id.eq_ignore_ascii_case(id) && row.protocol == protocol));
            evidence.push(PersistedModelProtocol {
                scope: scope.clone(),
                model_id: id.clone(),
                protocol,
                source: ContractEvidenceSource::Static,
                verified_at: Some(now),
                observed_at: None,
                last_probe_result: None,
                last_probe_at: None,
                last_probe_error: None,
            });
        }
    }
    let evidence = evidence.clone();
    let mut contracts =
        crate::provider_contracts::build_effective_contracts(&Default::default(), &[], persisted);
    let go = contracts
        .providers
        .get_mut(crate::provider::OPENCODE_PROVIDER_ID)
        .ok_or_else(|| "OpenCode Go contract is unavailable".to_string())?;
    // Static offline defaults cannot substitute for discovery protocol facts.
    for model in go.models.values_mut() {
        for row in model.protocols.values_mut() {
            let documented = evidence.iter().any(|saved| {
                saved.model_id.eq_ignore_ascii_case(&model.model_id)
                    && saved.protocol == row.protocol
                    && saved.source.confers_support()
            });
            row.available &= documented;
            row.enabled &= documented;
        }
        model.routable &= model.has_enabled_protocol();
    }
    let mut projection = projection.clone();
    for destination in &mut projection.destinations {
        if destination.id
            == ocg_domain::destination::destination_id_for_builtin(
                crate::provider::OPENCODE_PROVIDER_ID,
            )
        {
            destination.catalog = go
                .models
                .values()
                .map(|model| ocg_domain::destination::CatalogModel {
                    public_model: model.model_id.clone(),
                    upstream_model: model.model_id.clone(),
                    protocols: model.enabled_protocols(),
                    preferred: Some(model.preferred_protocol),
                    enabled: model.routable,
                    upstream_override: None,
                })
                .collect();
        }
    }
    contracts.apply_destination_configuration(&projection);
    contracts
        .providers
        .remove(crate::provider::OPENCODE_PROVIDER_ID)
        .ok_or_else(|| "OpenCode Go contract is unavailable".to_string())
}

#[cfg(test)]
mod tests;
