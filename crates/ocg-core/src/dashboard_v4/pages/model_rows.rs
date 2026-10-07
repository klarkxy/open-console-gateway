//! Pure catalog presentation shared by page reads and acknowledged mutations.
use crate::dashboard_v3;
use crate::dashboard_v3::{
    AccountUpstreamProtocol as Protocol, ContractEvidenceSource, ContractScopeKind,
    EffectiveModelContract, EffectiveModelProtocols, EffectiveProtocolEvidence,
    ProtocolOverrideState, ProviderCatalogPresentation, ProviderModelAction,
    ProviderModelPresentation,
};
use crate::dashboard_v4::types::*;

fn evidence(model: &EffectiveModelContract, p: Protocol) -> Option<&EffectiveProtocolEvidence> {
    match p {
        Protocol::ChatCompletions => model.protocols.chat_completions.as_ref(),
        Protocol::Responses => model.protocols.responses.as_ref(),
        Protocol::Messages => model.protocols.messages.as_ref(),
    }
}
const PROTOCOLS: [Protocol; 3] = [
    Protocol::ChatCompletions,
    Protocol::Responses,
    Protocol::Messages,
];
pub(crate) fn enabled_protocol(model: &EffectiveModelContract) -> Option<Protocol> {
    std::iter::once(model.preferred_protocol)
        .chain(PROTOCOLS)
        .find(|p| evidence(model, *p).is_some_and(|e| e.enabled))
}
fn protocol(p: ProtocolDto) -> Protocol {
    match p {
        ProtocolDto::ChatCompletions => Protocol::ChatCompletions,
        ProtocolDto::Responses => Protocol::Responses,
        ProtocolDto::Messages => Protocol::Messages,
    }
}
pub(crate) fn http_model(d: &DestinationDto, m: &CatalogModelDto) -> EffectiveModelContract {
    let available = if let Some(v) = &m.upstream_override {
        vec![protocol(v.protocol)]
    } else if !d.protocol_routes.is_empty() {
        d.protocol_routes
            .iter()
            .map(|r| protocol(r.protocol))
            .collect()
    } else {
        d.protocols
            .iter()
            .copied()
            .map(protocol)
            .collect::<Vec<_>>()
    };
    let e = |p: Protocol| {
        Some(EffectiveProtocolEvidence {
            protocol: p,
            available: available.contains(&p),
            enabled: m.enabled
                && available.contains(&p)
                && m.protocols.iter().copied().map(protocol).any(|v| v == p),
            source: ContractEvidenceSource::Static,
            verified_at: None,
            observed_at: None,
            last_probe_result: None,
            last_probe_at: None,
            last_probe_error: None,
            r#override: ProtocolOverrideState::Auto,
        })
    };
    let preferred = m
        .preferred
        .map(protocol)
        .filter(|p| available.contains(p))
        .or_else(|| available.first().copied())
        .or_else(|| m.preferred.map(protocol))
        .unwrap_or(Protocol::ChatCompletions);
    EffectiveModelContract {
        alias: m.public_model.clone(),
        model_id: m.public_model.clone(),
        preferred_protocol: preferred,
        protocols: EffectiveModelProtocols {
            chat_completions: e(Protocol::ChatCompletions),
            responses: e(Protocol::Responses),
            messages: e(Protocol::Messages),
        },
        routable: m.enabled
            && m.protocols
                .iter()
                .copied()
                .map(protocol)
                .any(|p| available.contains(&p)),
        disabled_reasons: if m.enabled {
            vec![]
        } else {
            vec!["model_disabled".into()]
        },
    }
}
fn presentation_action(key: &str, allowed: bool) -> ProviderModelAction {
    ProviderModelAction {
        key: key.into(),
        allowed,
        reason: (!allowed).then(|| "unavailable".into()),
    }
}
pub(crate) fn presentation(
    d: &DestinationDto,
    scope_kind: ContractScopeKind,
    models: Vec<EffectiveModelContract>,
) -> ProviderCatalogPresentation {
    let models: Vec<ProviderModelPresentation> = models
        .into_iter()
        .map(|m| {
            let saved = d.catalog.iter().find(|c| {
                if d.legacy.kind == LegacyDestinationKindDto::Builtin {
                    c.upstream_model.eq_ignore_ascii_case(&m.model_id)
                } else {
                    c.public_model == m.model_id
                }
            });
            let upstream = saved
                .map(|c| c.upstream_model.clone())
                .unwrap_or_else(|| m.model_id.clone());
            let public = if d.legacy.kind == LegacyDestinationKindDto::Builtin {
                m.alias.clone()
            } else {
                m.model_id.clone()
            };
            let test = enabled_protocol(&m);
            let target = test.or_else(|| {
                std::iter::once(m.preferred_protocol)
                    .chain(PROTOCOLS)
                    .find(|p| evidence(&m, *p).is_some_and(|e| e.available))
            });
            let writable = PROTOCOLS
                .into_iter()
                .filter(|p| {
                    evidence(&m, *p)
                        .is_some_and(|e| scope_kind == ContractScopeKind::Provider || e.available)
                })
                .collect::<Vec<_>>();
            let editable = d.adapter == AdapterKindDto::Http
                || d.legacy.kind == LegacyDestinationKindDto::Builtin;
            ProviderModelPresentation {
                public_model: public,
                upstream_model: upstream,
                contract: m,
                upstream_override: saved.and_then(|c| c.upstream_override.as_ref()).map(|v| {
                    dashboard_v3::ProviderModelUpstreamOverride {
                        protocol: protocol(v.protocol),
                        endpoint_url: v.endpoint_url.clone(),
                    }
                }),
                target_protocol: target,
                test_protocol: test,
                writable_protocols: writable.clone(),
                effective_on: test.is_some(),
                actions: vec![
                    presentation_action("toggle", editable && !writable.is_empty()),
                    presentation_action("test", d.capabilities.testable && test.is_some()),
                    presentation_action("modelEditable", editable),
                    presentation_action("metadataEditable", !d.capabilities.external_integration),
                ],
            }
        })
        .collect();
    ProviderCatalogPresentation {
        total: u32::try_from(models.len()).unwrap_or(u32::MAX),
        all_disabled: models.iter().all(|m| !m.effective_on),
        models,
    }
}
pub(crate) fn http_presentation(d: &DestinationDto) -> Option<ProviderCatalogPresentation> {
    (d.adapter == AdapterKindDto::Http).then(|| {
        presentation(
            d,
            ContractScopeKind::CustomEndpoint,
            d.catalog.iter().map(|m| http_model(d, m)).collect(),
        )
    })
}

#[cfg(test)]
mod tests;
