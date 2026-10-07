//! Compact management reads. Summaries deliberately do not masquerade as full inventories.
use crate::billing_types::BillingStatus;
use crate::dashboard_v3::*;
use crate::dashboard_v4::types::*;
use crate::model_metadata::ModelMetadata;
use crate::platform::{PlatformAccount, PlatformGroup, PlatformKind, PlatformLink, PlatformQuota};
use ocg_domain::connection::{AuthorizationState, ConnectionOrigin};
use ocg_domain::credential::AuthState;
use ocg_domain::provider::ProviderOrigin;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! dto {
    ($(pub struct $name:ident { $($body:tt)* })*) => {$(
        #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "camelCase")]
        #[schemars(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $($body)* }
    )*};
}
dto! {
pub struct PageReadIssue { pub resource: String, pub id: Option<String>, pub code: String }
pub struct DestinationSummary {
    pub account_controls: AccountControlsDto, pub id: String, pub legacy: LegacyDestinationRefDto,
    pub adapter: AdapterKindDto, pub name: String, pub brand_family: Option<String>, pub base_url: Option<String>,
    pub protocols: Vec<ProtocolDto>, pub protocol_routes: Vec<HttpProtocolRouteDto>, pub auth_scheme: AuthSchemeDto,
    pub model_resolution: ModelResolutionDto, pub capabilities: CapabilitiesDto, pub plan: Option<PlanDto>,
    pub max_credentials: Option<u32>, pub observer_credential_id: Option<String>, pub enabled: bool,
    pub catalog_count: u32, pub enabled_catalog_count: u32
}
pub struct AccountSummary {
    pub id: String, pub provider_id: String, pub credential_kind: AccountCredentialKind, pub quota_scope: AccountQuotaScope,
    pub name: String, pub username: Option<String>, pub enabled: bool, pub account_type: AccountType, pub setup_step: AccountSetupStep,
    pub purchase_date: String, pub expires_on: String, pub cooldown_until: Option<String>, pub cooldown_generic_until: Option<String>,
    pub cooldown_5h_until: Option<String>, pub cooldown_week_until: Option<String>, pub cooldown_month_until: Option<String>,
    pub cooldown_free_until: Option<String>, pub last_error: Option<String>, pub auth_error: Option<String>, pub notes: Option<String>,
    pub usage_sync_last_success_at: Option<String>, pub usage_sync_next_allowed_at: Option<String>, pub created_at: String,
    pub updated_at: String, pub revision: u64, pub process_generation: u64, pub verification_status: AccountVerificationStatus,
    pub connection_verified_at: Option<String>, pub verification_error: Option<String>, pub plan_routable: bool,
    pub ollama_billing_tier: Option<OllamaBillingTier>, pub model_capability_count: u32
}
pub struct CredentialScopeSummary { pub kind: String, pub model_count: u32, pub single_model: Option<String> }
pub struct AccountCredentialSummary {
    pub id: String, pub legacy_account_id: String, pub destination_id: String, pub name: String, pub notes: Option<String>,
    pub has_secret: bool, pub key_preview: Option<String>, pub enabled: bool, pub routing_rank: u32,
    pub scope: CredentialScopeSummary, pub grants: CredentialGrantsDto, pub auth_state: AuthState,
    pub last_error: Option<String>, pub cooldowns: CredentialCooldownsDto, pub quota_pool_id: Option<String>,
    pub onboarding_task: Option<DestinationOnboardingTaskDto>, pub purchase_date: Option<String>, pub quota_recovery: Option<QuotaRecoveryDto>
}
pub struct PlatformSnapshotSummary {
    pub observed_at: i64, pub stale: bool, pub errors: Vec<String>, pub quotas: Vec<PlatformQuota>,
    pub billing_preference: Option<String>, pub wallet_overflow: Option<bool>, pub model_count: u32, pub price_count: u32, pub group_count: u32,
    pub wallet: Option<PlatformWalletSummary>, pub key_remaining: Option<PlatformAmountSummary>, pub key_name: Option<String>
}
pub struct PlatformAmountSummary { pub amount: f64, pub unit: String }
pub struct PlatformWalletSummary {
    pub unit: String, pub remaining: Option<f64>, pub remaining_unlimited: bool,
    pub history_used: Option<f64>, pub month_used: Option<f64>, pub observed_at: Option<i64>
}
pub struct PlatformSummary {
    pub id: String, pub kind: PlatformKind, pub name: String, pub base_url: String, pub has_user_credential: bool,
    pub version: u64, pub snapshot: Option<PlatformSnapshotSummary>
}
pub struct PlatformLinkSummary {
    pub account_id: String, pub platform_account_id: String, pub group: PlatformGroup, pub snapshot: Option<PlatformSnapshotSummary>
}
pub struct PageAction { pub key: String, pub allowed: bool, pub reason: Option<String> }
pub struct AccountRefreshFact { pub supported: bool, pub observed_at: Option<String>, pub fresh_until: Option<String>, pub next_allowed_at: Option<String> }
pub struct AccountPageTags { pub credential_count: u32, pub binding_disabled: bool, pub quota_share_name: Option<String>, pub quota_share_count: u32, pub duplicate_name: bool }
pub struct AccountPageRow {
    pub credential: AccountCredentialSummary, pub account: Option<AccountSummary>, pub platform_link: Option<PlatformLinkSummary>,
    pub status: String, pub route_available: bool, pub model_count: u32, pub inference_endpoint_url: Option<String>,
    pub actions: Vec<PageAction>, pub billing: Option<BillingStatus>, pub refresh: AccountRefreshFact, pub tags: AccountPageTags
}
pub struct AccountCardPageItem {
    pub card_id: String, pub position: u32, pub destination: DestinationSummary, pub platform: Option<PlatformSummary>,
    pub total_credentials: u32, pub matched_credentials: u32, pub rows: Vec<AccountPageRow>, pub credential_create: Option<CredentialCreateCapabilityDto>,
    pub actions: Vec<PageAction>, pub availability: String, pub rows_offset: u32, pub rows_has_more: bool, pub cpa_status: Option<String>
}
pub struct AccountPlanFilter { pub value: String, pub label: String, pub card_count: u32, pub credential_count: u32 }
pub struct AccountsPage {
    pub revision: ControlRevision, pub read_version: String, pub as_of: String, pub valid_until: Option<String>,
    pub total_cards: u32, pub total_credentials: u32, pub matched_cards: u32, pub matched_credentials: u32,
    pub cards: Vec<AccountCardPageItem>, pub offset: u32, pub limit: u32, pub has_more: bool, pub errors: Vec<PageReadIssue>,
    pub plan_options: Vec<AccountPlanFilter>, pub routing_mode: RoutingMode, pub conversation_sticky: bool
}
pub struct AccountCardCredentialsPage {
    pub revision: ControlRevision, pub read_version: String, pub as_of: String, pub valid_until: Option<String>, pub card_id: String, pub total: u32, pub filtered_total: u32,
    pub rows: Vec<AccountPageRow>, pub offset: u32, pub limit: u32, pub has_more: bool, pub errors: Vec<PageReadIssue>
}
pub struct AccountPageDetail {
    pub revision: ControlRevision, pub account: Account, pub destination: Option<DestinationDto>, pub credential: Option<DestinationCredentialDto>,
    pub identity: Option<IdentitySummary>, pub connection: Option<ConnectionSummary>, pub platform: Option<PlatformAccount>, pub platform_link: Option<PlatformLink>, pub operations: AccountOperationDetail
}
pub struct AccountOperationDetail {
    pub rotate: bool, pub binding: bool, pub create: bool, pub unsupported_reason: Option<String>,
    pub credential_id: Option<String>, pub binding_id: Option<String>, pub identity_id: Option<String>,
    pub allowed_connections: Vec<ConnectionSummary>, pub share_targets: Vec<AccountCredentialShareTarget>,
    pub test_models: Vec<AccountTestModelChoice>, pub granted_endpoint_ids: Vec<String>, pub stale_endpoint_ids: Vec<String>, pub stale_origins: Vec<String>
}
pub struct AccountCredentialShareTarget { pub id: String, pub label: String }
pub struct AccountTestModelChoice { pub model_id: String, pub alias: String, pub protocol: AccountUpstreamProtocol }
pub struct AccountPageLayout { pub revision: ControlRevision, pub cards: Vec<RoutingCard> }
pub struct ProviderPageItem {
    pub rail_key: String, pub destination_id: Option<String>, pub connection_id: Option<String>, pub provider_id: Option<String>,
    pub legacy: LegacyIdentity, pub name: String, pub brand_family: Option<String>, pub preset_id: Option<String>, pub origin: ConnectionOrigin,
    pub lifecycle: ConnectionLifecycle, pub authorization: AuthorizationState, pub eligibility: Eligibility,
    pub credential_create: CredentialCreateCapabilityDto, pub credential_count: u32, pub enabled_credential_count: u32, pub catalog_count: u32
}
pub struct ProvidersPage {
    pub revision: ControlRevision, pub read_version: String, pub as_of: String, pub valid_until: Option<String>,
    pub total: u32, pub filtered_total: u32, pub items: Vec<ProviderPageItem>, pub offset: u32, pub limit: u32, pub has_more: bool, pub errors: Vec<PageReadIssue>
}
pub struct ProviderCatalogEntrySummary {
    pub provider_id: String,

    /// Row provenance in the unified `providers` table. Wire values:
    /// `builtin` (sealed adapter), `preset` (preset-derived dynamic row),
    /// `custom` (manual dynamic row).
    pub origin: ProviderOrigin,
    /// Whether the dashboard may PATCH this entry. Always `false` for
    /// `builtin` rows; `true` for `preset`/`custom`.
    pub editable: bool,
    /// Whether the dashboard may DELETE this entry. Same rules as `editable`.
    pub deletable: bool,
    /// Plan/api offering label carried by the catalog row. Builtin rows use
    /// the sealed builtin map; dynamic rows mirror the persisted `offering`.
    pub offering: String,

    pub display_name: String,
    pub display_family: String,
    pub credential_kind: AccountCredentialKind,
    pub quota_scope: AccountQuotaScope,
    pub singleton: bool,
    pub creation_availability: String,
    pub creation_unavailable_reason: Option<String>,
    pub verification_policy: String,
    pub verification_runtime_availability: String,
    pub routable: bool,
    pub managed_registration: bool,
    pub pricing_availability: String,
    pub usage_availability: String,
    pub manual_usage_calibration: bool,
    pub quota_unit: String,
    pub model_source: String,
    pub key_prefix: Option<String>,
    pub auth_schemes: Vec<AccountAuthScheme>,
    pub upstream_protocols: Vec<AccountUpstreamProtocol>,
    pub form_fields: Vec<ProviderCatalogFormField>,
    pub model_alias_count: u32,
}
pub struct ProviderCatalogSummary { pub source: String, pub source_url: String, pub refreshed_at: Option<String>, pub model_count: u32, pub refresh_supported: bool }
pub struct ProviderScopeSummary {
    pub key: String, pub scope_kind: ContractScopeKind, pub scope_id: String, pub provider_id: String, pub label: String,
    pub static_protocol_snapshot_date: Option<String>, pub account_count: u32, pub catalog: ProviderCatalogSummary,
    pub usage: CapabilitySummary, pub card: CardCapabilitySummary, pub catalog_routable: bool, pub production_inference: bool,
    pub disabled_reasons: Vec<String>, pub revision: u64, pub all_disabled: bool
}
pub struct ProviderModelWriteTarget { pub kind: String, pub id: String }
pub struct ProviderPageDetail {
    pub revision: ControlRevision, pub read_version: String, pub item: ProviderPageItem, pub destination: Option<DestinationSummary>,
    pub endpoints: Vec<ConnectionEndpoint>, pub catalog_entry: Option<ProviderCatalogEntrySummary>, pub scope: Option<ProviderScopeSummary>,
    pub model_write_target: Option<ProviderModelWriteTarget>, pub actions: Vec<PageAction>
}
pub struct ProviderModelPageRow {
    pub public_model: String, pub upstream_model: String, pub contract: EffectiveModelContract,
    pub upstream_override: Option<ProviderModelUpstreamOverride>, pub metadata: Option<ModelMetadata>, pub metadata_source: Option<String>,
    pub target_protocol: Option<AccountUpstreamProtocol>, pub test_protocol: Option<AccountUpstreamProtocol>,
    pub writable_protocols: Vec<AccountUpstreamProtocol>, pub effective_on: bool, pub actions: Vec<PageAction>
}
pub struct ProviderModelsPage {
    pub revision: ControlRevision, pub read_version: String, pub total: u32, pub filtered_total: u32, pub all_disabled: bool,
    pub models: Vec<ProviderModelPageRow>, pub offset: u32, pub limit: u32, pub has_more: bool
}
pub struct ProviderEditDetail {
    pub revision: ControlRevision, pub read_version: String, pub item: ProviderPageItem,
    pub destination: Option<DestinationDto>, pub definition: Option<ProviderDefinition>, pub related_contracts: ProviderContracts,
    pub catalog_entry: Option<ProviderCatalogEntry>, pub credentials: Vec<DestinationCredentialDto>, pub accounts: Vec<Account>, pub identities: Vec<IdentitySummary>, pub connection: Option<ConnectionSummary>, pub scope: Option<ProviderEditScope>
}
pub struct ProviderEditScope { pub summary: ProviderScopeSummary, pub models: Vec<ProviderModelPageRow>, pub accounts: Vec<ProviderAccountChoice> }
pub struct AliasPageTarget { pub account_id: Option<String>, pub provider_id: Option<String>, pub destination_id: Option<String>, pub model: String, pub capabilities: bool }
pub struct AliasCapabilitySummary { pub state: String, pub destination_id: Option<String>, pub source: Option<String>, pub input_modalities: Vec<String>, pub output_modalities: Vec<String> }
pub struct AliasPageRow {
    pub key: String, pub provider_id: String, pub destination_id: Option<String>, pub public_model: String, pub upstream_model: String,
    pub provider_plan: String, pub custom_account_id: Option<String>, pub custom_account: Option<String>, pub routable: bool,
    pub routing_ranks: Vec<u32>, pub platform_label: Option<String>, pub capability: AliasCapabilitySummary,
    pub target: Option<AliasPageTarget>, pub capability_target: Option<AliasPageTarget>
}
pub struct AliasPageGroup {
    pub public_model: String, pub publication_key: String, pub published: bool, pub total_rows: u32, pub matching_rows: u32,
    pub has_overlap: bool, pub continued: bool, pub rows: Vec<AliasPageRow>
}
pub struct AliasesPage {
    pub revision: ControlRevision, pub read_version: String, pub as_of: String, pub valid_until: Option<String>,
    pub total_groups: u32, pub total_rows: u32, pub filtered_groups: u32, pub filtered_rows: u32, pub groups: Vec<AliasPageGroup>,
    pub offset: u32, pub limit: u32, pub has_more: bool, pub errors: Vec<PageReadIssue>
}
pub struct AccountPageRefresh { pub revision: ControlRevision, pub outcome: String, pub billing: Option<BillingStatus>, pub account: Option<Account>, pub refresh: AccountRefreshFact, pub errors: Vec<PageReadIssue> }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountPageRefreshMode {
    Automatic,
    Manual,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountPageRefreshRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub mode: AccountPageRefreshMode,
}
