//! Transfer-only destination/credential carriers.
//!
//! These types live on the encrypted node-migration envelope. They are not
//! Dashboard V4 listing DTOs and must not be added to the public schema.

use chrono::{DateTime, Utc};
use ocg_domain::credential::{AuthState, ModelScope};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Capabilities, CatalogModel, Credential, Destination,
    LegacyDestinationRef, ModelResolution, Plan, Protocol,
};
use ocg_domain::dynamic::DynamicModelUpstreamOverride;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::dashboard_v4::types::{
    AdapterKindDto, AuthSchemeDto, CapabilitiesDto, CatalogModelDto, CredentialCooldownsDto,
    CredentialGrantsDto, DestinationOnboardingTaskDto, ExpiryCadenceDto, LegacyDestinationKindDto,
    LegacyDestinationRefDto, PlanDto, PlanWindowKindDto, PricingSourceDto, ProtocolDto,
    RedirectPolicyDto, UsageSourceDto,
};
use crate::platform::PlatformGroup;

pub(super) const PURPOSE_INFERENCE: &str = "inference";
pub(super) const PURPOSE_PLATFORM_OBSERVER: &str = "platform_observer";
pub(super) const PURPOSE_CPA_OBSERVER: &str = "cpa_observer";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PortableCatalogModel {
    pub public_model: String,
    pub upstream_model: String,
    pub protocols: Vec<ProtocolDto>,
    pub preferred: Option<ProtocolDto>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_override: Option<PortableCatalogOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PortableCatalogOverride {
    pub protocol: String,
    pub endpoint_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PortableDestination {
    pub id: String,
    pub legacy: LegacyDestinationRefDto,
    pub adapter: AdapterKindDto,
    pub name: String,
    pub brand_family: Option<String>,
    pub base_url: Option<String>,
    pub protocols: Vec<ProtocolDto>,
    pub auth_scheme: AuthSchemeDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution: Option<ModelResolution>,
    pub catalog: Vec<PortableCatalogModel>,
    pub capabilities: CapabilitiesDto,
    pub plan: Option<PlanDto>,
    pub max_credentials: Option<u32>,
    pub observer_credential_id: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarding_draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offering: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PortableCredential {
    pub id: String,
    pub legacy_account_id: String,
    pub destination_id: String,
    pub name: String,
    pub notes: Option<String>,
    pub has_secret: bool,
    pub enabled: bool,
    pub routing_rank: u32,
    pub scope: ModelScope,
    pub grants: CredentialGrantsDto,
    pub auth_state: AuthState,
    pub last_error: Option<String>,
    pub cooldowns: CredentialCooldownsDto,
    pub quota_pool_id: Option<String>,
    pub onboarding_task: Option<DestinationOnboardingTaskDto>,
    pub purchase_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_state_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_verified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_billing_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_group: Option<PlatformGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_meter: Option<crate::billing_types::PortableCreditMeter>,
}

impl Zeroize for PortableDestination {
    fn zeroize(&mut self) {
        self.id.zeroize();
        self.legacy.id.zeroize();
        self.name.zeroize();
        self.brand_family.zeroize();
        self.base_url.zeroize();
        self.observer_credential_id.zeroize();
        self.platform_kind.zeroize();
        self.platform_snapshot.zeroize();
        self.preset_id.zeroize();
        self.origin.zeroize();
        self.offering.zeroize();
        for model in &mut self.catalog {
            model.public_model.zeroize();
            model.upstream_model.zeroize();
            if let Some(override_route) = model.upstream_override.as_mut() {
                override_route.protocol.zeroize();
                override_route.endpoint_url.zeroize();
            }
        }
    }
}

impl Zeroize for PortableCredential {
    fn zeroize(&mut self) {
        self.id.zeroize();
        self.legacy_account_id.zeroize();
        self.destination_id.zeroize();
        self.name.zeroize();
        self.notes.zeroize();
        self.key.zeroize();
        self.username.zeroize();
        self.password.zeroize();
        self.management_key.zeroize();
        self.identity_id.zeroize();
        self.identity_label.zeroize();
        self.identity_notes.zeroize();
        self.provider_id.zeroize();
    }
}

impl From<&CatalogModel> for PortableCatalogModel {
    fn from(value: &CatalogModel) -> Self {
        let dto = CatalogModelDto::from(value);
        Self {
            public_model: dto.public_model,
            upstream_model: dto.upstream_model,
            protocols: dto.protocols,
            preferred: dto.preferred,
            enabled: dto.enabled,
            upstream_override: value.upstream_override.as_ref().map(|route| {
                PortableCatalogOverride {
                    protocol: route.protocol.as_str().to_string(),
                    endpoint_url: route.endpoint_url.clone(),
                }
            }),
        }
    }
}

impl From<&Destination> for PortableDestination {
    fn from(destination: &Destination) -> Self {
        Self {
            id: destination.id.clone(),
            legacy: LegacyDestinationRefDto::from(&destination.legacy),
            adapter: destination.adapter.into(),
            name: destination.name.clone(),
            brand_family: destination.brand_family.clone(),
            base_url: destination.base_url.clone(),
            protocols: destination
                .protocols
                .iter()
                .copied()
                .map(ProtocolDto::from)
                .collect(),
            auth_scheme: destination.auth_scheme.into(),
            model_resolution: Some(destination.model_resolution),
            catalog: destination
                .catalog
                .iter()
                .map(PortableCatalogModel::from)
                .collect(),
            capabilities: CapabilitiesDto::from(&destination.capabilities),
            plan: destination.plan.as_ref().map(PlanDto::from),
            max_credentials: destination.max_credentials,
            observer_credential_id: destination.observer_credential_id.clone(),
            enabled: destination.enabled,
            platform_kind: None,
            platform_version: None,
            platform_snapshot: None,
            onboarding_draft: None,
            preset_id: None,
            origin: None,
            offering: None,
        }
    }
}

impl From<&Credential> for PortableCredential {
    fn from(credential: &Credential) -> Self {
        Self {
            id: credential.id.clone(),
            legacy_account_id: credential.legacy_account_id.clone(),
            destination_id: credential.destination_id.clone(),
            name: credential.name.clone(),
            notes: credential.notes.clone(),
            has_secret: credential.has_secret,
            enabled: credential.enabled,
            routing_rank: credential.routing_rank,
            scope: credential.scope.clone(),
            grants: CredentialGrantsDto::from(&credential.grants),
            auth_state: credential.auth_state,
            last_error: credential.last_error.clone(),
            cooldowns: CredentialCooldownsDto::from(&credential.cooldowns),
            quota_pool_id: credential.quota_pool_id.clone(),
            onboarding_task: credential
                .onboarding_task
                .as_ref()
                .map(DestinationOnboardingTaskDto::from),
            purchase_date: credential.purchase_date.clone(),
            identity_id: None,
            identity_label: None,
            identity_confidence: None,
            authority_site: None,
            authority_subject: None,
            identity_enabled: None,
            identity_notes: None,
            credential_version: None,
            auth_state_version: None,
            binding_id: None,
            binding_enabled: None,
            key: String::new(),
            username: None,
            password: None,
            management_key: None,
            purpose: None,
            provider_id: None,
            account_type: None,
            setup_step: None,
            verification_status: None,
            connection_verified_at: None,
            credential_kind: None,
            quota_scope: None,
            ollama_billing_tier: None,
            link_group: None,
            credit_meter: None,
        }
    }
}

pub(super) fn protocol_from_dto(value: ProtocolDto) -> Protocol {
    match value {
        ProtocolDto::ChatCompletions => Protocol::ChatCompletions,
        ProtocolDto::Responses => Protocol::Responses,
        ProtocolDto::Messages => Protocol::Messages,
    }
}

pub(super) fn adapter_from_dto(value: AdapterKindDto) -> AdapterKind {
    match value {
        AdapterKindDto::OpencodeGo => AdapterKind::OpencodeGo,
        AdapterKindDto::Zen => AdapterKind::Zen,
        AdapterKindDto::Goat => AdapterKind::Goat,
        AdapterKindDto::Minimax => AdapterKind::Minimax,
        AdapterKindDto::Kimi => AdapterKind::Kimi,
        AdapterKindDto::Ollama => AdapterKind::Ollama,
        AdapterKindDto::Cpa => AdapterKind::Cpa,
        AdapterKindDto::Http => AdapterKind::Http,
    }
}

pub(super) fn auth_scheme_from_dto(value: AuthSchemeDto) -> AuthScheme {
    match value {
        AuthSchemeDto::None => AuthScheme::None,
        AuthSchemeDto::Bearer => AuthScheme::Bearer,
        AuthSchemeDto::XApiKey => AuthScheme::XApiKey,
    }
}

pub(super) fn legacy_ref_from_dto(value: &LegacyDestinationRefDto) -> LegacyDestinationRef {
    match value.kind {
        LegacyDestinationKindDto::Builtin => LegacyDestinationRef::Builtin(value.id.clone()),
        LegacyDestinationKindDto::Dynamic => LegacyDestinationRef::Dynamic(value.id.clone()),
        LegacyDestinationKindDto::CustomAccount => {
            LegacyDestinationRef::CustomAccount(value.id.clone())
        }
        LegacyDestinationKindDto::PlatformParent => {
            LegacyDestinationRef::PlatformParent(value.id.clone())
        }
    }
}

fn capabilities_from_dto(value: &CapabilitiesDto) -> Capabilities {
    Capabilities {
        testable: value.testable,
        discoverable_models: value.discoverable_models,
        official_balance_probe: value.official_balance_probe.clone(),
        observer: value.observer,
        managed_signup: value.managed_signup,
        external_integration: value.external_integration,
        billing_tier_required: value.billing_tier_required,
        redirect_policy: match value.redirect_policy {
            RedirectPolicyDto::NoFollow => ocg_domain::destination::RedirectPolicy::NoFollow,
            RedirectPolicyDto::FollowKeyless => {
                ocg_domain::destination::RedirectPolicy::FollowKeyless
            }
        },
        identity_headers: value.identity_headers,
    }
}

fn plan_from_dto(value: &PlanDto) -> Plan {
    Plan {
        usage_source: match value.usage_source {
            UsageSourceDto::OfficialApi => ocg_domain::destination::UsageSource::OfficialApi,
            UsageSourceDto::LocalProjection => {
                ocg_domain::destination::UsageSource::LocalProjection
            }
            UsageSourceDto::None => ocg_domain::destination::UsageSource::None,
        },
        windows: value
            .windows
            .iter()
            .map(|window| ocg_domain::destination::PlanWindow {
                kind: match window.kind {
                    PlanWindowKindDto::FiveHours => {
                        ocg_domain::destination::PlanWindowKind::FiveHours
                    }
                    PlanWindowKindDto::Week => ocg_domain::destination::PlanWindowKind::Week,
                    PlanWindowKindDto::Month => ocg_domain::destination::PlanWindowKind::Month,
                    PlanWindowKindDto::Free => ocg_domain::destination::PlanWindowKind::Free,
                },
            })
            .collect(),
        expiry_cadence: value.expiry_cadence.map(|cadence| match cadence {
            ExpiryCadenceDto::Monthly => ocg_domain::destination::ExpiryCadence::Monthly,
        }),
        pricing_source: match value.pricing_source {
            PricingSourceDto::Official => ocg_domain::destination::PricingSource::Official,
            PricingSourceDto::VerifiedSnapshot => {
                ocg_domain::destination::PricingSource::VerifiedSnapshot
            }
            PricingSourceDto::Unpriced => ocg_domain::destination::PricingSource::Unpriced,
        },
        manual_calibration: value.manual_calibration,
    }
}

pub(super) fn catalog_from_portable(models: &[PortableCatalogModel]) -> Vec<CatalogModel> {
    models
        .iter()
        .map(|model| CatalogModel {
            public_model: model.public_model.clone(),
            upstream_model: model.upstream_model.clone(),
            protocols: model
                .protocols
                .iter()
                .copied()
                .map(protocol_from_dto)
                .collect(),
            preferred: model.preferred.map(protocol_from_dto),
            enabled: model.enabled,
            upstream_override: model.upstream_override.as_ref().map(|route| {
                DynamicModelUpstreamOverride {
                    protocol: ocg_domain::catalog::UpstreamProtocolKind::try_from(
                        route.protocol.as_str(),
                    )
                    .expect("portable catalog override protocol was validated"),
                    endpoint_url: route.endpoint_url.clone(),
                }
            }),
        })
        .collect()
}

pub(super) fn destination_from_portable(destination: &PortableDestination) -> Destination {
    let legacy = legacy_ref_from_dto(&destination.legacy);
    let model_resolution = destination.model_resolution.unwrap_or(match &legacy {
        LegacyDestinationRef::Dynamic(_) => ModelResolution::PublicAndUpstream,
        LegacyDestinationRef::CustomAccount(_) | LegacyDestinationRef::PlatformParent(_) => {
            ModelResolution::PublicOnly
        }
        LegacyDestinationRef::Builtin(_) => ModelResolution::AdapterDefined,
    });
    Destination {
        id: destination.id.clone(),
        legacy,
        adapter: adapter_from_dto(destination.adapter),
        name: destination.name.clone(),
        brand_family: destination.brand_family.clone(),
        base_url: destination.base_url.clone(),
        protocols: destination
            .protocols
            .iter()
            .copied()
            .map(protocol_from_dto)
            .collect(),
        auth_scheme: auth_scheme_from_dto(destination.auth_scheme),
        model_resolution,
        catalog: catalog_from_portable(&destination.catalog),
        capabilities: capabilities_from_dto(&destination.capabilities),
        plan: destination.plan.as_ref().map(plan_from_dto),
        max_credentials: destination.max_credentials,
        observer_credential_id: destination.observer_credential_id.clone(),
        enabled: destination.enabled,
    }
}

pub(super) fn parse_cooldown_field(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

pub(super) fn catalog_override_from_json(value: Option<&str>) -> Option<PortableCatalogOverride> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed: DynamicModelUpstreamOverride = serde_json::from_str(raw).ok()?;
    Some(PortableCatalogOverride {
        protocol: parsed.protocol.as_str().to_string(),
        endpoint_url: parsed.endpoint_url,
    })
}

pub(super) fn credential_purpose(credential: &PortableCredential) -> &str {
    credential
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(PURPOSE_INFERENCE)
}

pub(super) fn is_observer_purpose(purpose: &str) -> bool {
    purpose == PURPOSE_PLATFORM_OBSERVER || purpose == PURPOSE_CPA_OBSERVER
}
