//! Provider usage capability surface.
//!
//! OpenCode Go is the only verified authoritative automatic-sync contract
//! today. Command Code GOAT publishes authoritative manual evidence, but is
//! intentionally excluded from the automatic coordinator.

use crate::provider::{ProviderRegistry, UsageContractKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageEvidence {
    Authoritative,
    Unavailable,
}

/// Stable API-facing capability description. An absent endpoint is meaningful:
/// no caller may infer or synthesize one from the provider base URL.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderUsageCapability {
    pub provider_id: &'static str,

    pub evidence: ProviderUsageEvidence,
    pub experimental: bool,
    pub endpoint: Option<&'static str>,
    pub automatic_sync: bool,
    pub authoritative_for_quota: bool,
    pub affects_inference_eligibility: bool,
}

pub fn provider_usage_capability(provider_id: &str) -> Option<ProviderUsageCapability> {
    let descriptor = ProviderRegistry::get(provider_id)?;
    if !descriptor.usage.publishes_capability {
        return None;
    }
    Some(ProviderUsageCapability {
        provider_id: descriptor.provider_id,

        evidence: match descriptor.usage.contract {
            UsageContractKind::Authoritative => ProviderUsageEvidence::Authoritative,
            UsageContractKind::LocalState
            | UsageContractKind::ExperimentalUnavailable
            | UsageContractKind::Unavailable => ProviderUsageEvidence::Unavailable,
        },
        experimental: descriptor.usage.experimental,
        endpoint: descriptor.usage.endpoint,
        automatic_sync: descriptor.usage.automatic_sync,
        authoritative_for_quota: descriptor.usage.authoritative_for_quota,
        affects_inference_eligibility: descriptor.usage.affects_inference_eligibility,
    })
}

pub fn supports_authoritative_auto_sync(provider_id: &str) -> bool {
    provider_usage_capability(provider_id)
        .is_some_and(|capability| capability.automatic_sync && capability.authoritative_for_quota)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_keep_goat_and_zen_out_of_authoritative_go_sync() {
        use crate::provider::{
            COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID,
            OPENCODE_ZEN_FREE_PROVIDER_ID, ProviderAdapterKind,
        };

        let go = provider_usage_capability(OPENCODE_PROVIDER_ID).unwrap();
        assert_eq!(go.evidence, ProviderUsageEvidence::Authoritative);
        assert!(go.automatic_sync);
        assert!(go.authoritative_for_quota);

        let goat = provider_usage_capability(COMMAND_CODE_PROVIDER_ID).unwrap();
        assert_eq!(goat.evidence, ProviderUsageEvidence::Authoritative);
        assert!(!goat.automatic_sync);
        assert!(!goat.authoritative_for_quota);

        assert!(provider_usage_capability(OPENCODE_ZEN_FREE_PROVIDER_ID).is_none());

        assert!(provider_usage_capability(CUSTOM_PROVIDER_ID).is_none());
        for descriptor in crate::provider::ProviderRegistry::iter() {
            let capability = provider_usage_capability(descriptor.provider_id);
            match descriptor.kind {
                ProviderAdapterKind::OpenCodeGo => {
                    let capability = capability.expect("Go publishes authoritative usage");
                    assert_eq!(capability.evidence, ProviderUsageEvidence::Authoritative);
                    assert!(capability.automatic_sync);
                    assert!(capability.authoritative_for_quota);
                }
                ProviderAdapterKind::CommandCodeGoat => {
                    let capability = capability.expect("GOAT publishes manual official usage");
                    assert_eq!(capability.evidence, ProviderUsageEvidence::Authoritative);
                    assert!(!capability.automatic_sync);
                    assert!(!capability.authoritative_for_quota);
                }
                ProviderAdapterKind::OllamaCloud => {
                    let capability = capability.expect("Ollama Cloud publishes local-state usage");
                    assert_eq!(capability.evidence, ProviderUsageEvidence::Unavailable);
                    assert!(!capability.automatic_sync);
                    assert!(!capability.authoritative_for_quota);
                }
                ProviderAdapterKind::MiniMaxCn | ProviderAdapterKind::KimiCn => {
                    let capability = capability.expect("sealed CN Plan publishes usage");
                    assert_eq!(capability.evidence, ProviderUsageEvidence::Authoritative);
                    assert!(!capability.automatic_sync);
                }
                ProviderAdapterKind::ZenFree => {
                    assert!(capability.is_none());
                }
                ProviderAdapterKind::Cpa => {
                    assert!(capability.is_none());
                }
                ProviderAdapterKind::ConfigurableHttp => {
                    assert!(capability.is_none());
                }
            }
        }
    }

    #[test]
    fn usage_capability_publishes_through_provider_descriptor() {
        use crate::provider::{
            COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID,
            OPENCODE_ZEN_FREE_PROVIDER_ID, ProviderRegistry,
        };

        let go_usage = ProviderRegistry::get(OPENCODE_PROVIDER_ID).unwrap().usage;
        assert!(go_usage.publishes_capability);
        assert!(provider_usage_capability(OPENCODE_PROVIDER_ID).is_some());

        let goat_usage = ProviderRegistry::get(COMMAND_CODE_PROVIDER_ID)
            .unwrap()
            .usage;
        assert!(!goat_usage.experimental);
        assert!(goat_usage.publishes_capability);
        assert!(!goat_usage.automatic_sync);
        let goat = provider_usage_capability(COMMAND_CODE_PROVIDER_ID).unwrap();
        assert_eq!(goat.evidence, ProviderUsageEvidence::Authoritative);
        assert!(!goat.automatic_sync);

        let zen_usage = ProviderRegistry::get(OPENCODE_ZEN_FREE_PROVIDER_ID)
            .unwrap()
            .usage;
        assert!(!zen_usage.publishes_capability);
        assert!(!zen_usage.authoritative_for_quota);
        assert!(provider_usage_capability(OPENCODE_ZEN_FREE_PROVIDER_ID).is_none());

        assert!(
            !ProviderRegistry::get(CUSTOM_PROVIDER_ID)
                .unwrap()
                .usage
                .publishes_capability
        );
    }
}
