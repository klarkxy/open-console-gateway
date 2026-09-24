use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use ocg_domain::credential::{AuthState, ModelScope};
use ocg_domain::destination::{
    AdapterKind, LegacyCredentialFacts, LegacyDestinationFacts, LegacyIdentityFacts,
    LegacyPlatformLink, ModelResolution, credential_from_legacy, destination_from_legacy,
    destination_id_for_dynamic, destination_id_for_platform_account,
};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride};
use zeroize::Zeroizing;

use super::portable::{
    PURPOSE_CPA_OBSERVER, PURPOSE_INFERENCE, PURPOSE_PLATFORM_OBSERVER, PortableCatalogOverride,
    PortableCredential, PortableDestination, catalog_override_from_json, credential_purpose,
    is_observer_purpose,
};
use super::{
    MAX_ACCOUNTS, MAX_CAPABILITIES, MAX_ENDPOINT_CHARS, MAX_KEY_CHARS, MAX_NAME_CHARS,
    MAX_NOTES_CHARS, MAX_ROUTING_CARDS, MAX_USERNAME_CHARS, PAYLOAD_VERSION, PortableCooldowns,
    PortableNodeState, PortablePayload, PortableProviderDefinition, TransferError,
    V5_PAYLOAD_VERSION, V7_PAYLOAD_VERSION, V8_PAYLOAD_VERSION, V9_PAYLOAD_VERSION,
    ValidatedAccount, validate_node_state, validate_portable_dynamic_providers,
};
use crate::dashboard_v4::types::{
    AdapterKindDto, AuthSchemeDto, CredentialCooldownsDto, CredentialGrantsDto,
    LegacyDestinationKindDto, RoutingCard,
};
use crate::db::ImportedCustomDestination;
use crate::db::identity::{
    IdentityImportSnapshot, ImportedAccountIdentity, ImportedIdentity, ImportedQuotaPool,
};
use crate::destination_projection;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{
    AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep as ModelSetupStep,
    AccountType as ModelAccountType, normalize_account_notes, normalize_purchase_date,
};
use crate::platform::{PlatformKind, PortablePlatformAccount, PortablePlatformLink};
use crate::provider::{
    ConnectionVerificationStatus, CreationAvailability, CredentialKind, QuotaScope,
    UpstreamProtocolKind, builtin_provider, provider_allows_enablement,
};
use crate::state::CoreState;

#[derive(Debug)]
pub(super) struct UnifiedNewModelImport {
    pub destination_controls: Vec<ocg_domain::destination::Destination>,
    pub destinations: Vec<PortableDestination>,
    pub credentials: Vec<PortableCredential>,
    pub routing_cards: Option<Vec<RoutingCard>>,
    pub identity_snapshot: Option<IdentityImportSnapshot>,
    pub dynamic_providers: Vec<DynamicProviderRuntime>,
    pub custom_destinations: Vec<ImportedCustomDestination>,
    pub custom_credential_destinations: HashMap<String, String>,
    pub draft_provider_ids: HashSet<String>,
    pub platform_accounts: Vec<PortablePlatformAccount>,
    pub platform_links: Vec<PortablePlatformLink>,
    pub platform_catalogs: HashMap<String, Vec<ocg_domain::destination::CatalogModel>>,
    pub platform_links_authoritative: bool,
    pub platform_snapshots: HashMap<String, String>,
    pub platform_versions: HashMap<String, i64>,
    pub cpa_base_url: Option<String>,
    pub cpa_management_key: Option<String>,
}

#[derive(Debug)]
pub(super) struct ValidatedMigration {
    pub exported_at: String,
    pub accounts: Vec<ValidatedAccount>,
    pub node: Zeroizing<PortableNodeState>,
    pub unified: UnifiedNewModelImport,
    /// V4/V5 packages may still carry the old exclusive-radio `force_off`
    /// siblings. Later packages keep an explicit close.
    pub legacy_exclusive_radio_repair: bool,
}

pub(super) fn export_new_model(
    state: &CoreState,
) -> Result<(Vec<PortableDestination>, Vec<PortableCredential>, u64), TransferError> {
    let db = state.db.lock();
    let stored =
        destination_projection::load_persisted(&db).map_err(|_| TransferError::Internal)?;
    let dest_platform = crate::db::platform::snapshot_destination_platform_extras(&db.conn)
        .map_err(|_| TransferError::Internal)?;
    let dest_dynamic = crate::db::dynamic_store::snapshot_destination_extras(&db.conn)
        .map_err(|_| TransferError::Internal)?;
    let dest_cpa = crate::db::cpa::snapshot_destination_extras(&db.conn)
        .map_err(|_| TransferError::Internal)?;
    let model_overrides = crate::db::dynamic_store::snapshot_model_overrides(&db.conn)
        .map_err(|_| TransferError::Internal)?;
    let observers = crate::db::platform::snapshot_observer_credentials(&db.conn)
        .map_err(|_| TransferError::Internal)?;
    let extras = crate::db::account_store::snapshot_credential_extras(&db.conn)
        .map_err(|_| TransferError::Internal)?;
    let identity_model = db
        .list_identity_model()
        .map_err(|_| TransferError::Internal)?;
    let ollama_tiers = stored
        .credentials
        .iter()
        .filter_map(|credential| {
            let extras = extras.get(&credential.legacy_account_id)?;
            if extras.provider_id.as_deref() != Some(crate::provider::OLLAMA_PROVIDER_ID) {
                return None;
            }
            db.ollama_cloud_billing_tier(&credential.legacy_account_id)
                .ok()
                .flatten()
                .map(|tier| {
                    (
                        credential.legacy_account_id.clone(),
                        tier.as_str().to_string(),
                    )
                })
        })
        .collect::<HashMap<_, _>>();
    drop(db);

    let platform_by_id: HashMap<_, _> = dest_platform
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    let dynamic_by_id: HashMap<_, _> = dest_dynamic
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    let cpa_by_id: HashMap<_, _> = dest_cpa
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    let mut overrides_by_dest: HashMap<String, HashMap<String, Option<String>>> = HashMap::new();
    for row in model_overrides {
        overrides_by_dest
            .entry(row.destination_id)
            .or_default()
            .insert(row.public_model_key, row.upstream_override);
    }
    let observers_by_id: HashMap<_, _> = observers
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    let identity_by_account: HashMap<_, _> = identity_model
        .accounts
        .iter()
        .map(|row| (row.account.id.as_str(), row))
        .collect();

    let mut destinations = Vec::new();
    let mut keep_dest_ids = HashSet::new();
    for destination in &stored.destinations {
        if destination.adapter == AdapterKind::Zen {
            continue;
        }
        if destination.adapter == AdapterKind::Cpa {
            let extra_url = cpa_by_id
                .get(&destination.id)
                .and_then(|extra| extra.base_url.clone());
            let configured_url = destination
                .base_url
                .as_deref()
                .or(extra_url.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some();
            let configured_observer = observers_by_id
                .values()
                .any(|row| row.destination_id == destination.id && row.has_secret);
            if !configured_url && !configured_observer {
                continue;
            }
        }
        let mut portable = PortableDestination::from(destination);
        if let Some(overrides) = overrides_by_dest.get(&destination.id) {
            for model in &mut portable.catalog {
                if let Some(raw) = overrides.get(&model.public_model.to_ascii_lowercase()) {
                    model.upstream_override = catalog_override_from_json(raw.as_deref());
                }
            }
        }
        if let Some(extra) = platform_by_id.get(&destination.id) {
            portable.platform_kind = extra.platform_kind.clone();
            portable.platform_version = extra.platform_version;
            portable.platform_snapshot = extra.platform_snapshot.clone();
        }
        if let Some(extra) = dynamic_by_id.get(&destination.id) {
            portable.onboarding_draft = Some(extra.onboarding_draft != 0);
            portable.preset_id = extra.preset_id.clone();
            portable.origin = extra.origin.clone();
            portable.offering = extra.offering.clone();
        }
        if let Some(extra) = cpa_by_id.get(&destination.id)
            && portable.base_url.is_none()
        {
            portable.base_url = extra.base_url.clone();
        }
        keep_dest_ids.insert(portable.id.clone());
        destinations.push(portable);
    }

    let mut credentials = Vec::new();
    let mut skipped = 0_u64;
    for credential in &stored.credentials {
        if !keep_dest_ids.contains(&credential.destination_id) {
            continue;
        }
        let extra = extras.get(&credential.legacy_account_id);
        let observer = observers_by_id.get(&credential.id);
        let purpose = observer
            .map(|row| row.purpose.as_str())
            .unwrap_or(PURPOSE_INFERENCE);
        if credential.legacy_account_id == crate::provider::CPA_ACCOUNT_ID
            && purpose == PURPOSE_INFERENCE
        {
            continue;
        }
        let account_type = extra
            .and_then(|row| row.account_type.as_deref())
            .and_then(|value| ModelAccountType::try_from(value).ok());
        let setup_step = extra
            .and_then(|row| row.setup_step.as_deref())
            .and_then(|value| ModelSetupStep::try_from(value).ok());
        if purpose == PURPOSE_INFERENCE
            && account_type == Some(ModelAccountType::Managed)
            && setup_step.is_some_and(|step| step != ModelSetupStep::Ready)
        {
            skipped += 1;
            continue;
        }
        let mut portable = PortableCredential::from(credential);
        portable.purpose = Some(purpose.to_string());
        if let Some(extra) = extra {
            portable.username = extra.username.clone();
            portable.provider_id = extra.provider_id.clone();
            portable.account_type = extra.account_type.clone();
            portable.setup_step = extra.setup_step.clone();
            portable.verification_status = extra.verification_status.clone();
            portable.connection_verified_at = extra.connection_verified_at.clone();
            portable.credential_kind = extra.credential_kind.clone();
            portable.quota_scope = extra.quota_scope.clone();
            portable.identity_id = extra.identity_id.clone();
            portable.identity_label = extra.identity_label.clone();
            portable.identity_confidence = extra.identity_confidence.clone();
            portable.authority_site = extra.authority_site.clone();
            portable.authority_subject = extra.authority_subject.clone();
            portable.identity_enabled = extra.identity_enabled.map(|value| value != 0);
            portable.identity_notes = extra.identity_notes.clone();
            portable.credential_version = extra.credential_version.map(|value| value as u64);
            portable.auth_state_version = extra.auth_state_version.map(|value| value as u64);
            portable.binding_id = extra.binding_id.clone();
            portable.binding_enabled = extra.binding_enabled.map(|value| value != 0);
            if let Some(group_json) = extra.group_json.as_deref()
                && let Ok(group) = serde_json::from_str(group_json)
            {
                portable.link_group = Some(group);
            }
            let key_cipher = extra.key_cipher.as_str();
            if !key_cipher.is_empty() {
                let plaintext = state
                    .decrypt_key(key_cipher)
                    .map_err(|_| TransferError::Internal)?;
                if is_observer_purpose(purpose) {
                    portable.management_key = Some(plaintext);
                } else {
                    portable.key = plaintext;
                }
            }
            if let Some(password_cipher) = extra.password_cipher.as_deref()
                && !password_cipher.is_empty()
            {
                portable.password = Some(
                    state
                        .decrypt_key(password_cipher)
                        .map_err(|_| TransferError::Internal)?,
                );
            }
        }
        if let Some(identity) = identity_by_account.get(credential.legacy_account_id.as_str()) {
            if portable.identity_id.is_none() {
                portable.identity_id = Some(identity.identity_id.clone());
            }
            if portable.identity_label.is_none() {
                portable.identity_label = Some(identity.account.name.clone());
            }
            if portable.credential_version.is_none() {
                portable.credential_version = Some(identity.credential_version);
            }
            if portable.auth_state_version.is_none() {
                portable.auth_state_version = Some(identity.auth_state_version);
            }
            if portable.binding_id.is_none() {
                portable.binding_id = Some(identity.binding_id.clone());
            }
            if portable.binding_enabled.is_none() {
                portable.binding_enabled = Some(identity.binding_enabled);
            }
            if portable.provider_id.is_none() {
                portable.provider_id = Some(identity.account.provider_id.clone());
            }
        }
        if let Some(tier) = ollama_tiers.get(&credential.legacy_account_id) {
            portable.ollama_billing_tier = Some(tier.clone());
        }
        portable.credit_meter = crate::db::billing::export_on(
            &state.db.lock().conn,
            &credential.legacy_account_id,
            Utc::now(),
        )
        .map_err(|_| TransferError::Internal)?;
        credentials.push(portable);
    }
    let mut exported_ids: HashSet<String> = credentials
        .iter()
        .map(|credential| credential.id.clone())
        .collect();
    for observer in observers_by_id.values() {
        if !keep_dest_ids.contains(&observer.destination_id)
            || !is_observer_purpose(&observer.purpose)
            || !exported_ids.insert(observer.id.clone())
        {
            continue;
        }
        let management_key = if observer.key_cipher.is_empty() {
            None
        } else {
            Some(
                state
                    .decrypt_key(&observer.key_cipher)
                    .map_err(|_| TransferError::Internal)?,
            )
        };
        credentials.push(portable_observer_credential(observer, management_key));
    }
    Ok((destinations, credentials, skipped))
}

fn validate_routing_cards(
    payload: &PortablePayload,
    destination_ids: &HashSet<String>,
) -> Result<Option<Vec<RoutingCard>>, TransferError> {
    match payload.routing_cards.as_ref() {
        None if payload.version >= V9_PAYLOAD_VERSION => Err(TransferError::Invalid(
            "this V9 backup is missing routingCards".to_string(),
        )),
        None => Ok(None),
        Some(_) if payload.version < V9_PAYLOAD_VERSION => Err(TransferError::Invalid(
            "this backup carries routing card semantics that cannot be imported as a V4/V5/V6/V7/V8 package"
                .to_string(),
        )),
        Some(cards) => {
            if cards.len() > MAX_ROUTING_CARDS {
                return Err(TransferError::InvalidBundle);
            }
            let inference: HashMap<_, _> = payload
                .credentials
                .iter()
                .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
                .map(|credential| (credential.id.as_str(), credential))
                .collect();
            let mut card_ids = HashSet::new();
            let mut seen_credentials = HashSet::new();
            for (index, card) in cards.iter().enumerate() {
                let prefix = || format!("routing card {}", index + 1);
                if card.id.trim().is_empty()
                    || card.id.len() > 128
                    || !card_ids.insert(card.id.as_str())
                {
                    return Err(TransferError::Invalid(format!(
                        "{} has an invalid or duplicate id",
                        prefix()
                    )));
                }
                if card.destination_id.is_empty()
                    || !destination_ids.contains(&card.destination_id)
                {
                    return Err(TransferError::Invalid(format!(
                        "{} names an unknown destination",
                        prefix()
                    )));
                }
                for credential_id in &card.credential_ids {
                    let Some(credential) = inference.get(credential_id.as_str()) else {
                        return Err(TransferError::Invalid(format!(
                            "{} names an unknown credential",
                            prefix()
                        )));
                    };
                    if credential.destination_id != card.destination_id {
                        return Err(TransferError::Invalid(format!(
                            "{} credential does not belong to its destination",
                            prefix()
                        )));
                    }
                    if !seen_credentials.insert(credential_id.as_str()) {
                        return Err(TransferError::Invalid(
                            "a credential appears in more than one routing card".to_string(),
                        ));
                    }
                }
            }
            if seen_credentials.len() != inference.len() {
                return Err(TransferError::Invalid(
                    "routingCards must cover every inference credential exactly once".to_string(),
                ));
            }
            let flattened: Vec<&str> = cards
                .iter()
                .flat_map(|card| card.credential_ids.iter().map(String::as_str))
                .collect();
            let mut ranked: Vec<&PortableCredential> = payload
                .credentials
                .iter()
                .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
                .collect();
            ranked.sort_by_key(|credential| credential.routing_rank);
            let ranked_ids: Vec<&str> = ranked
                .iter()
                .map(|credential| credential.id.as_str())
                .collect();
            if flattened != ranked_ids {
                return Err(TransferError::Invalid(
                    "routingCards order contradicts credential routing_rank".to_string(),
                ));
            }
            if let Some(node) = payload.node.as_ref() {
                let card_accounts: Vec<&str> = flattened.iter()
                    .map(|id| inference[id].legacy_account_id.as_str())
                    .collect();
                let account_ids: HashSet<&str> = card_accounts.iter().copied().collect();
                let node_accounts: Vec<&str> = node.account_order.iter()
                    .map(String::as_str)
                    .filter(|id| account_ids.contains(id))
                    .collect();
                if card_accounts != node_accounts {
                    return Err(TransferError::Invalid(
                        "routingCards order contradicts node account_order".to_string(),
                    ));
                }
            }
            Ok(Some(cards.clone()))
        }
    }
}

pub(super) fn validate_new_model_payload(
    payload: &mut PortablePayload,
) -> Result<(Vec<ValidatedAccount>, UnifiedNewModelImport), TransferError> {
    if payload.node.is_none() {
        return Err(TransferError::InvalidBundle);
    }
    if payload.destinations.is_empty()
        && payload.credentials.is_empty()
        && !payload.accounts.is_empty()
    {
        return Err(TransferError::Invalid(
            "this V7+ backup is missing its destination/credential snapshot".to_string(),
        ));
    }
    if payload.credentials.len() > MAX_ACCOUNTS {
        return Err(TransferError::InvalidBundle);
    }
    compare_legacy_fields_to_new_model(payload)?;
    for destination in &mut payload.destinations {
        if destination.model_resolution.is_none() {
            if payload.version >= V8_PAYLOAD_VERSION {
                return Err(TransferError::Invalid(format!(
                    "destination `{}` is missing modelResolution",
                    destination.id
                )));
            }
            destination.model_resolution = Some(match destination.legacy.kind {
                LegacyDestinationKindDto::Dynamic => ModelResolution::PublicAndUpstream,
                LegacyDestinationKindDto::CustomAccount
                | LegacyDestinationKindDto::PlatformParent => ModelResolution::PublicOnly,
                LegacyDestinationKindDto::Builtin => ModelResolution::AdapterDefined,
            });
        }
        let expected_resolution = match destination.legacy.kind {
            LegacyDestinationKindDto::Dynamic => ModelResolution::PublicAndUpstream,
            LegacyDestinationKindDto::CustomAccount | LegacyDestinationKindDto::PlatformParent => {
                ModelResolution::PublicOnly
            }
            LegacyDestinationKindDto::Builtin => ModelResolution::AdapterDefined,
        };
        if destination.model_resolution != Some(expected_resolution) {
            return Err(TransferError::Invalid(format!(
                "destination `{}` has an incompatible modelResolution",
                destination.id
            )));
        }
        if destination.legacy.kind == LegacyDestinationKindDto::CustomAccount {
            destination.max_credentials = None;
        }
    }
    let mut destination_ids = HashSet::new();
    for destination in &payload.destinations {
        if destination.id.trim().is_empty() || !destination_ids.insert(destination.id.clone()) {
            return Err(TransferError::Invalid(
                "destination snapshot has a missing or duplicate destination id".to_string(),
            ));
        }
        if destination.legacy.id.trim().is_empty() {
            return Err(TransferError::Invalid(format!(
                "destination `{}` is missing its legacy id",
                destination.id
            )));
        }
        match destination.legacy.kind {
            LegacyDestinationKindDto::PlatformParent => {
                if destination
                    .platform_kind
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(TransferError::Invalid(format!(
                        "destination `{}` is missing its platform kind",
                        destination.id
                    )));
                }
            }
            LegacyDestinationKindDto::Dynamic => {
                if destination.onboarding_draft.is_none()
                    || destination
                        .origin
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or("")
                        .is_empty()
                    || destination
                        .offering
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or("")
                        .is_empty()
                {
                    return Err(TransferError::Invalid(format!(
                        "destination `{}` is missing dynamic draft/origin/offering",
                        destination.id
                    )));
                }
            }
            LegacyDestinationKindDto::CustomAccount => {
                if destination
                    .base_url
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(TransferError::Invalid(format!(
                        "destination `{}` is missing its Custom Endpoint",
                        destination.id
                    )));
                }
            }
            LegacyDestinationKindDto::Builtin if destination.adapter == AdapterKindDto::Cpa => {
                if destination
                    .base_url
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(TransferError::Invalid(format!(
                        "destination `{}` is missing its CPA base url",
                        destination.id
                    )));
                }
            }
            LegacyDestinationKindDto::Builtin => {}
        }
    }
    let mut credential_ids = HashSet::new();
    let mut account_ids = HashSet::new();
    for (index, credential) in payload.credentials.iter().enumerate() {
        let prefix = || format!("credential {}", index + 1);
        if credential.id.trim().is_empty() || !credential_ids.insert(credential.id.clone()) {
            return Err(TransferError::Invalid(
                "destination snapshot has a missing or duplicate credential id".to_string(),
            ));
        }
        uuid::Uuid::parse_str(credential.id.trim()).map_err(|_| {
            TransferError::Invalid(format!("{} has an invalid credential id", prefix()))
        })?;
        if !destination_ids.contains(&credential.destination_id) {
            return Err(TransferError::Invalid(format!(
                "credential `{}` names an unknown destination",
                credential.id
            )));
        }
        let purpose = credential_purpose(credential);
        if let Some(meter) = credential.credit_meter.as_ref() {
            let destination = payload
                .destinations
                .iter()
                .find(|destination| destination.id == credential.destination_id)
                .ok_or(TransferError::InvalidBundle)?;
            if is_observer_purpose(purpose)
                || destination.adapter != AdapterKindDto::Http
                || !matches!(
                    destination.legacy.kind,
                    LegacyDestinationKindDto::Dynamic | LegacyDestinationKindDto::CustomAccount
                )
            {
                return Err(TransferError::Invalid(format!(
                    "{} cannot carry a personal credit meter",
                    prefix()
                )));
            }
            validate_portable_credit_meter(meter, credential, destination).map_err(|error| {
                TransferError::Invalid(format!("{} has an invalid credit meter: {error}", prefix()))
            })?;
        }
        if is_observer_purpose(purpose) {
            if credential.has_secret
                && credential
                    .management_key
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
            {
                return Err(TransferError::Invalid(format!(
                    "{} is missing its observer management secret",
                    prefix()
                )));
            }
            continue;
        }
        let account_id = credential.legacy_account_id.trim();
        if account_id.is_empty() || uuid::Uuid::parse_str(account_id).is_err() {
            return Err(TransferError::Invalid(format!(
                "{} is missing a stable account id",
                prefix()
            )));
        }
        if !account_ids.insert(account_id.to_string()) {
            return Err(TransferError::Invalid(format!(
                "{} duplicates an earlier account identity in the package",
                prefix()
            )));
        }
        validate_inference_credential(credential, index)?;
    }
    let (dynamic_providers, draft_provider_ids) =
        dynamics_from_destinations(&payload.destinations)?;
    let (custom_destinations, custom_credential_destinations) =
        customs_from_destinations(&payload.destinations, &payload.credentials)?;
    let (
        platform_accounts,
        platform_links,
        platform_catalogs,
        platform_snapshots,
        platform_versions,
    ) = platforms_from_destinations(&payload.destinations, &payload.credentials)?;
    let (cpa_base_url, cpa_management_key) =
        cpa_from_destinations(&payload.destinations, &payload.credentials);
    let identity_snapshot =
        identity_snapshot_from_credentials(&payload.credentials, &payload.quota_pools)?;
    let accounts = validated_accounts_from_credentials(
        &payload.destinations,
        &payload.credentials,
        &dynamic_providers,
    )?;
    let routing_cards = validate_routing_cards(payload, &destination_ids)?;
    Ok((
        accounts,
        UnifiedNewModelImport {
            destination_controls: payload
                .destinations
                .iter()
                .map(super::portable::destination_from_portable)
                .collect::<Result<Vec<_>, _>>()?,
            destinations: payload.destinations.clone(),
            credentials: payload.credentials.clone(),
            routing_cards,
            identity_snapshot,
            dynamic_providers,
            custom_destinations,
            custom_credential_destinations,
            draft_provider_ids,
            platform_accounts,
            platform_links,
            platform_catalogs,
            platform_links_authoritative: true,
            platform_snapshots,
            platform_versions,
            cpa_base_url,
            cpa_management_key,
        },
    ))
}

fn validate_portable_credit_meter(
    meter: &crate::billing_types::PortableCreditMeter,
    credential: &PortableCredential,
    destination: &PortableDestination,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        meter.created_at <= meter.exported_at,
        "creation is after export"
    );
    anyhow::ensure!(
        meter
            .last_calibration_at
            .is_none_or(|at| at <= meter.exported_at),
        "calibration is after export"
    );
    anyhow::ensure!(
        meter
            .monthly_cursor
            .is_none_or(|at| at <= meter.exported_at),
        "monthly cursor is after export"
    );
    let mut state = crate::billing::CreditMeterState::new(
        "portable-validation".into(),
        credential.id.clone(),
        destination.id.clone(),
        destination.base_url.clone().unwrap_or_default(),
        meter.configuration.clone(),
        meter.buckets.clone(),
        meter.created_at,
    )?;
    state.spent_since_calibration = meter.spent_since_calibration;
    state.overdrawn = meter.overdrawn;
    state.unpriced_requests = meter.unpriced_requests;
    state.last_calibration_at = meter.last_calibration_at;
    state.monthly_cursor = meter.monthly_cursor;
    state.validate()
}

pub(super) fn map_old_graph_to_unified(
    payload: &PortablePayload,
    accounts: &[ValidatedAccount],
    dynamic_providers: Vec<DynamicProviderRuntime>,
    draft_provider_ids: HashSet<String>,
    identity_snapshot: Option<IdentityImportSnapshot>,
) -> Result<UnifiedNewModelImport, TransferError> {
    let mut destinations = Vec::new();
    let mut seen_dest = HashSet::new();
    for runtime in &dynamic_providers {
        let facts = LegacyDestinationFacts::Dynamic {
            definition: runtime.definition(),
        };
        let mapped = destination_from_legacy(&facts).map_err(|error| {
            TransferError::Invalid(format!("dynamic destination could not be mapped: {error}"))
        })?;
        let mut portable = PortableDestination::from(&mapped);
        portable.onboarding_draft = Some(draft_provider_ids.contains(&runtime.id));
        portable.preset_id = runtime.preset_id.clone();
        portable.origin = Some(runtime.origin.as_str().to_string());
        portable.offering = Some(runtime.offering.clone());
        for (model, mapping) in portable.catalog.iter_mut().zip(&runtime.mappings) {
            if let Some(override_route) = &mapping.upstream_override {
                model.upstream_override = Some(PortableCatalogOverride {
                    protocol: override_route.protocol.as_str().to_string(),
                    endpoint_url: override_route.endpoint_url.clone(),
                });
            }
        }
        seen_dest.insert(portable.id.clone());
        destinations.push(portable);
    }
    for parent in &payload.platform_accounts {
        let facts = LegacyDestinationFacts::PlatformParent {
            id: parent.id.clone(),
            kind: match parent.kind {
                PlatformKind::NewApi => ocg_domain::destination::PlatformKind::NewApi,
                PlatformKind::Sub2api => ocg_domain::destination::PlatformKind::Sub2Api,
            },
            name: parent.name.clone(),
            base_url: parent.base_url.clone(),
            has_user_credential: false,
        };
        let mapped = destination_from_legacy(&facts).map_err(|error| {
            TransferError::Invalid(format!("platform destination could not be mapped: {error}"))
        })?;
        let mut portable = PortableDestination::from(&mapped);
        portable.platform_kind = Some(match parent.kind {
            PlatformKind::NewApi => "new_api".to_string(),
            PlatformKind::Sub2api => "sub2api".to_string(),
        });
        portable.platform_version = Some(1);
        if seen_dest.insert(portable.id.clone()) {
            destinations.push(portable);
        }
    }
    let link_by_account: HashMap<_, _> = payload
        .platform_links
        .iter()
        .map(|link| (link.account_id.clone(), link.platform_account_id.clone()))
        .collect();
    for account in accounts {
        let Some(account_id) = account.id.as_deref() else {
            continue;
        };
        if link_by_account.contains_key(account_id) {
            continue;
        }
        if account.provider_id == crate::kernel::ids::CUSTOM_PROVIDER_ID {
            let config = account.custom_config.as_ref().ok_or_else(|| {
                TransferError::Invalid("custom account is missing its Custom Endpoint".to_string())
            })?;
            let facts = LegacyDestinationFacts::CustomAccount {
                account_id: account_id.to_string(),
                name: account.name.clone(),
                endpoint_url: config.endpoint_url.clone(),
                protocol: config.upstream_protocol,
                model_capabilities: account
                    .capabilities
                    .iter()
                    .map(|capability| {
                        (
                            capability.public_model.clone(),
                            capability.upstream_model.clone(),
                        )
                    })
                    .collect(),
            };
            let mapped = destination_from_legacy(&facts).map_err(|error| {
                TransferError::Invalid(format!("custom destination could not be mapped: {error}"))
            })?;
            let portable = PortableDestination::from(&mapped);
            if seen_dest.insert(portable.id.clone()) {
                destinations.push(portable);
            }
            continue;
        }
        if builtin_provider(&account.provider_id).is_some() {
            let mapped = destination_from_legacy(&LegacyDestinationFacts::Builtin {
                provider_id: account.provider_id.clone(),
            })
            .map_err(|error| {
                TransferError::Invalid(format!("builtin destination could not be mapped: {error}"))
            })?;
            let portable = PortableDestination::from(&mapped);
            if seen_dest.insert(portable.id.clone()) {
                destinations.push(portable);
            }
        }
    }
    let domain_destinations: Vec<_> = destinations
        .iter()
        .map(super::portable::destination_from_portable)
        .collect::<Result<Vec<_>, _>>()?;
    let mut credentials = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        let Some(account_id) = account.id.as_deref() else {
            continue;
        };
        let identity = account.identity.as_ref();
        let facts = LegacyCredentialFacts {
            id: account_id.to_string(),
            provider_id: account.provider_id.clone(),
            name: account.name.clone(),
            notes: account.notes.clone(),
            has_key: !account.key.is_empty(),
            enabled: account.enabled,
            order_index: index as u32,
            setup_step: account.setup_step,
            account_type: account.account_type,
            auth_error: None,
            last_error: None,
            purchase_date: Some(account.purchase_date.clone()),
            cooldown_generic_until: account.cooldowns.generic,
            cooldown_5h_until: account.cooldowns.five_hours,
            cooldown_week_until: account.cooldowns.week,
            cooldown_month_until: account.cooldowns.month,
            cooldown_free_until: account.cooldowns.free,
            verified: account.verification_status == ConnectionVerificationStatus::Verified,
            platform_link: link_by_account
                .get(account_id)
                .map(|parent_id| LegacyPlatformLink {
                    parent_id: parent_id.clone(),
                }),
            identity: identity.map(|row| LegacyIdentityFacts {
                quota_pool_id: None,
                model_scope: row.binding_model_scope.clone(),
                allowed_endpoint_ids: row.allowed_endpoint_ids.clone(),
                allowed_origins: row.allowed_origins.clone(),
                binding_enabled: row.binding_enabled,
            }),
        };
        let mapped = credential_from_legacy(&facts, &domain_destinations).map_err(|error| {
            TransferError::Invalid(format!("credential could not be mapped: {error}"))
        })?;
        let mut portable = PortableCredential::from(&mapped);
        portable.key = account.key.to_string();
        portable.username = account.username.clone();
        portable.purpose = Some(PURPOSE_INFERENCE.to_string());
        portable.provider_id = Some(account.provider_id.clone());
        portable.account_type = Some(account.account_type.as_str().to_string());
        portable.setup_step = Some(account.setup_step.as_str().to_string());
        portable.verification_status = Some(account.verification_status.as_str().to_string());
        portable.connection_verified_at = account
            .connection_verified_at
            .map(|value| value.to_rfc3339());
        portable.credential_kind = Some(account.credential_kind.as_str().to_string());
        portable.quota_scope = Some(account.quota_scope.as_str().to_string());
        portable.ollama_billing_tier = account
            .ollama_billing_tier
            .map(|tier| tier.as_str().to_string());
        if let Some(identity) = identity {
            portable.identity_id = Some(identity.identity_id.clone());
            portable.credential_version = Some(identity.credential_version);
            portable.auth_state_version = Some(identity.auth_state_version);
            portable.binding_id = Some(identity.binding_id.clone());
            portable.binding_enabled = Some(identity.binding_enabled);
        }
        if let Some(link) = payload
            .platform_links
            .iter()
            .find(|link| link.account_id == account_id)
        {
            portable.link_group = Some(link.group.clone());
        }
        credentials.push(portable);
    }
    let (custom_destinations, custom_credential_destinations) =
        customs_from_destinations(&destinations, &credentials)?;
    Ok(UnifiedNewModelImport {
        destinations,
        credentials,
        routing_cards: None,
        identity_snapshot,
        dynamic_providers,
        custom_destinations,
        custom_credential_destinations,
        draft_provider_ids,
        platform_accounts: payload.platform_accounts.clone(),
        platform_links: payload.platform_links.clone(),
        platform_catalogs: HashMap::new(),
        destination_controls: Vec::new(),
        platform_links_authoritative: payload.version >= V5_PAYLOAD_VERSION,
        platform_snapshots: HashMap::new(),
        platform_versions: HashMap::new(),
        cpa_base_url: None,
        cpa_management_key: None,
    })
}

fn compare_legacy_fields_to_new_model(payload: &PortablePayload) -> Result<(), TransferError> {
    if payload.accounts.is_empty()
        && payload.platform_accounts.is_empty()
        && payload.platform_links.is_empty()
        && payload.dynamic_providers.is_empty()
        && payload.identities.is_empty()
    {
        return Ok(());
    }
    let creds_by_account: HashMap<_, _> = payload
        .credentials
        .iter()
        .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
        .map(|credential| (credential.legacy_account_id.as_str(), credential))
        .collect();
    let dests_by_id: HashMap<_, _> = payload
        .destinations
        .iter()
        .map(|destination| (destination.id.as_str(), destination))
        .collect();
    for account in &payload.accounts {
        let Some(account_id) = account.id.as_deref() else {
            return Err(TransferError::Invalid(
                "imported account is missing a stable id that destination/credential can compare"
                    .to_string(),
            ));
        };
        let Some(credential) = creds_by_account.get(account_id) else {
            return Err(TransferError::Invalid(format!(
                "imported account `{account_id}` is not present on destination/credential"
            )));
        };
        if account.enabled != credential.enabled {
            return Err(TransferError::Invalid(format!(
                "imported account `{account_id}` enabled conflicts with destination/credential"
            )));
        }
        if let Some(scope) = account.binding_model_scope.as_ref()
            && scope != &credential.scope
        {
            return Err(TransferError::Invalid(format!(
                "imported account `{account_id}` scope conflicts with destination/credential"
            )));
        }
        if !account.key.is_empty() && !credential.key.is_empty() && account.key != credential.key {
            return Err(TransferError::Invalid(format!(
                "imported account `{account_id}` key conflicts with destination/credential"
            )));
        }
        if let Some(identity_id) = account.identity_id.as_deref()
            && credential.identity_id.as_deref() != Some(identity_id)
        {
            return Err(TransferError::Invalid(format!(
                "imported account `{account_id}` identity conflicts with destination/credential"
            )));
        }
        if let (Some(expected), Some(actual)) = (
            account.allowed_endpoint_ids.as_ref(),
            Some(&credential.grants.allowed_endpoint_ids),
        ) && expected != actual
        {
            return Err(TransferError::Invalid(format!(
                "imported account `{account_id}` grants conflict with destination/credential"
            )));
        }
        if let Some(destination) = dests_by_id.get(credential.destination_id.as_str())
            && account.name != destination.name
            && account.name != credential.name
        {
            return Err(TransferError::Invalid(format!(
                "imported account `{account_id}` name conflicts with destination/credential"
            )));
        }
    }
    if !payload.accounts.is_empty() {
        for credential in creds_by_account.values() {
            if !payload
                .accounts
                .iter()
                .any(|account| account.id.as_deref() == Some(credential.legacy_account_id.as_str()))
            {
                return Err(TransferError::Invalid(format!(
                    "credential `{}` is not covered by the leftover account graph",
                    credential.id
                )));
            }
        }
    }
    for parent in &payload.platform_accounts {
        let dest_id = destination_id_for_platform_account(&parent.id);
        let Some(destination) = dests_by_id.get(dest_id.as_str()) else {
            return Err(TransferError::Invalid(format!(
                "imported platform `{}` is not present on destination/credential",
                parent.id
            )));
        };
        let expected_kind = match parent.kind {
            PlatformKind::NewApi => "new_api",
            PlatformKind::Sub2api => "sub2api",
        };
        if destination.platform_kind.as_deref() != Some(expected_kind)
            || destination.base_url.as_deref() != Some(parent.base_url.as_str())
        {
            return Err(TransferError::Invalid(format!(
                "imported platform `{}` conflicts with destination/credential",
                parent.id
            )));
        }
    }
    for provider in &payload.dynamic_providers {
        let dest_id = destination_id_for_dynamic(&provider.id);
        let Some(destination) = dests_by_id.get(dest_id.as_str()) else {
            return Err(TransferError::Invalid(format!(
                "imported dynamic provider `{}` is not present on destination/credential",
                provider.id
            )));
        };
        if destination.base_url.as_deref() != Some(provider.endpoint_url.as_str())
            || destination.name != provider.name
        {
            return Err(TransferError::Invalid(format!(
                "imported dynamic provider `{}` conflicts with destination/credential",
                provider.id
            )));
        }
    }
    let creds_by_identity: HashMap<_, _> = payload
        .credentials
        .iter()
        .filter_map(|credential| {
            credential
                .identity_id
                .as_deref()
                .map(|identity_id| (identity_id, credential))
        })
        .collect();
    for identity in &payload.identities {
        let Some(credential) = creds_by_identity.get(identity.id.as_str()) else {
            return Err(TransferError::Invalid(format!(
                "imported identity `{}` is not present on destination/credential",
                identity.id
            )));
        };
        if let Some(label) = credential.identity_label.as_deref()
            && label != identity.label
        {
            return Err(TransferError::Invalid(format!(
                "imported identity `{}` conflicts with destination/credential",
                identity.id
            )));
        }
        if let Some(confidence) = credential.identity_confidence.as_deref()
            && confidence != identity.identity_confidence
        {
            return Err(TransferError::Invalid(format!(
                "imported identity `{}` conflicts with destination/credential",
                identity.id
            )));
        }
        if let Some(enabled) = credential.identity_enabled
            && enabled != identity.enabled
        {
            return Err(TransferError::Invalid(format!(
                "imported identity `{}` conflicts with destination/credential",
                identity.id
            )));
        }
    }
    Ok(())
}

fn validate_inference_credential(
    credential: &PortableCredential,
    index: usize,
) -> Result<(), TransferError> {
    let prefix = || format!("credential {}", index + 1);
    if credential.name.trim().is_empty() || credential.name.chars().count() > MAX_NAME_CHARS {
        return Err(TransferError::Invalid(format!(
            "{} has an invalid name",
            prefix()
        )));
    }
    if credential.key.chars().count() > MAX_KEY_CHARS {
        return Err(TransferError::Invalid(format!(
            "{} has an account Key that is too long",
            prefix()
        )));
    }
    if credential
        .username
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_USERNAME_CHARS)
    {
        return Err(TransferError::Invalid(format!(
            "{} has a username that is too long",
            prefix()
        )));
    }
    if credential
        .notes
        .as_deref()
        .is_some_and(|value| value.chars().count() > MAX_NOTES_CHARS)
    {
        return Err(TransferError::Invalid(format!(
            "{} has notes that are too long",
            prefix()
        )));
    }
    if credential.has_secret && credential.key.trim().is_empty() {
        return Err(TransferError::Invalid(format!(
            "{} is missing its account Key",
            prefix()
        )));
    }
    let identity_id = credential
        .identity_id
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| {
            TransferError::Invalid(format!("{} is missing its identity id", prefix()))
        })?;
    let binding_id = credential
        .binding_id
        .as_deref()
        .map(str::trim)
        .ok_or_else(|| TransferError::Invalid(format!("{} is missing its binding id", prefix())))?;
    let credential_version = credential.credential_version.ok_or_else(|| {
        TransferError::Invalid(format!("{} is missing its credential version", prefix()))
    })?;
    let auth_state_version = credential.auth_state_version.ok_or_else(|| {
        TransferError::Invalid(format!(
            "{} is missing its credential auth-state version",
            prefix()
        ))
    })?;
    if uuid::Uuid::parse_str(identity_id).is_err() || uuid::Uuid::parse_str(binding_id).is_err() {
        return Err(TransferError::Invalid(format!(
            "{} has an invalid identity or binding id",
            prefix()
        )));
    }
    if credential_version == 0
        || credential_version > i64::MAX as u64
        || auth_state_version == 0
        || auth_state_version > i64::MAX as u64
    {
        return Err(TransferError::Invalid(format!(
            "{} has an out-of-range credential version",
            prefix()
        )));
    }
    if let ModelScope::Only { models } = &credential.scope
        && models.iter().any(|model| model.trim().is_empty())
    {
        return Err(TransferError::Invalid(format!(
            "{} has an invalid binding model scope",
            prefix()
        )));
    }
    let mut seen_ids = HashSet::new();
    for id in &credential.grants.allowed_endpoint_ids {
        if id.trim().is_empty()
            || uuid::Uuid::parse_str(id.trim()).is_err()
            || !seen_ids.insert(id.trim())
        {
            return Err(TransferError::Invalid(format!(
                "{} has a malformed binding endpoint grant",
                prefix()
            )));
        }
    }
    let mut seen_origins = HashSet::new();
    for origin in &credential.grants.allowed_origins {
        let Some(normalized) = ocg_domain::credential::normalize_origin(origin) else {
            return Err(TransferError::Invalid(format!(
                "{} has a malformed binding origin grant",
                prefix()
            )));
        };
        if !seen_origins.insert(normalized) {
            return Err(TransferError::Invalid(format!(
                "{} has a duplicate binding origin grant",
                prefix()
            )));
        }
    }
    Ok(())
}

fn customs_from_destinations(
    destinations: &[PortableDestination],
    credentials: &[PortableCredential],
) -> Result<(Vec<ImportedCustomDestination>, HashMap<String, String>), TransferError> {
    let custom_by_id = destinations
        .iter()
        .filter(|destination| destination.legacy.kind == LegacyDestinationKindDto::CustomAccount)
        .map(|destination| (destination.id.as_str(), destination))
        .collect::<HashMap<_, _>>();
    let mut imported = Vec::with_capacity(custom_by_id.len());
    for destination in custom_by_id.values() {
        if destination.adapter != AdapterKindDto::Http {
            return Err(TransferError::Invalid(format!(
                "Custom destination `{}` must use the HTTP adapter",
                destination.id
            )));
        }
        let expected_id =
            ocg_domain::destination::destination_id_for_custom_account(&destination.legacy.id);
        if destination.id != expected_id {
            return Err(TransferError::Invalid(format!(
                "Custom destination `{}` has an incompatible stable identity",
                destination.id
            )));
        }
        let (protocol, endpoint_url, auth_scheme) =
            super::portable::verified_default_http_route(destination)?;
        let endpoint_url =
            crate::custom::validate_custom_endpoint_url(&endpoint_url).map_err(|_| {
                TransferError::Invalid(format!(
                    "destination `{}` has an invalid Custom Endpoint",
                    destination.id
                ))
            })?;
        let models = destination
            .catalog
            .iter()
            .map(|model| {
                Ok(DynamicModelMapping {
                    public_model: model.public_model.clone(),
                    upstream_model: model.upstream_model.clone(),
                    upstream_override: model
                        .upstream_override
                        .as_ref()
                        .map(|value| {
                            Ok::<_, TransferError>(DynamicModelUpstreamOverride {
                                protocol: UpstreamProtocolKind::try_from(value.protocol.as_str())
                                    .map_err(|_| {
                                        TransferError::Invalid(format!(
                                            "Custom destination `{}` has an invalid model protocol override",
                                            destination.id
                                        ))
                                    })?,
                                endpoint_url: value.endpoint_url.clone(),
                            })
                        })
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, TransferError>>()?;
        let definition = ocg_domain::dynamic::DynamicProviderDefinition {
            preset_id: None,
            id: destination.legacy.id.clone(),
            name: destination.name.clone(),
            endpoint_url: endpoint_url.clone(),
            upstream_protocol: protocol,
            auth_kind: match auth_scheme {
                ocg_domain::destination::AuthScheme::Bearer => DynamicAuthKind::Bearer,
                ocg_domain::destination::AuthScheme::XApiKey => DynamicAuthKind::XApiKey,
                ocg_domain::destination::AuthScheme::ApiKey => DynamicAuthKind::ApiKey,
                ocg_domain::destination::AuthScheme::None => DynamicAuthKind::None,
            },
            mappings: models.clone(),
        };
        let definition = crate::dynamic::validate_definition(definition).map_err(|error| {
            TransferError::Invalid(format!(
                "Custom destination `{}` is invalid: {error}",
                destination.id
            ))
        })?;
        imported.push(ImportedCustomDestination {
            id: destination.id.clone(),
            legacy_id: destination.legacy.id.clone(),
            name: definition.name,
            endpoint_url: definition.endpoint_url,
            protocol: definition.upstream_protocol,
            auth_scheme,
            models: definition.mappings,
            enabled: destination.enabled,
        });
    }
    let mut associations = HashMap::new();
    for credential in credentials {
        if is_observer_purpose(credential_purpose(credential))
            || !custom_by_id.contains_key(credential.destination_id.as_str())
        {
            continue;
        }
        if associations
            .insert(
                credential.legacy_account_id.clone(),
                credential.destination_id.clone(),
            )
            .is_some()
        {
            return Err(TransferError::Invalid(format!(
                "credential `{}` has duplicate Custom destination ownership",
                credential.id
            )));
        }
    }
    Ok((imported, associations))
}

fn dynamics_from_destinations(
    destinations: &[PortableDestination],
) -> Result<(Vec<DynamicProviderRuntime>, HashSet<String>), TransferError> {
    let mut providers = Vec::new();
    for destination in destinations {
        if destination.legacy.kind != LegacyDestinationKindDto::Dynamic {
            continue;
        }
        let draft = destination.onboarding_draft.unwrap_or(false);
        let (protocol, endpoint_url, auth_scheme) =
            super::portable::verified_default_http_route(destination)?;
        let auth_kind = match auth_scheme {
            ocg_domain::destination::AuthScheme::None => DynamicAuthKind::None,
            ocg_domain::destination::AuthScheme::XApiKey => DynamicAuthKind::XApiKey,
            ocg_domain::destination::AuthScheme::ApiKey => DynamicAuthKind::ApiKey,
            ocg_domain::destination::AuthScheme::Bearer => DynamicAuthKind::Bearer,
        };
        let mappings = destination
            .catalog
            .iter()
            .map(|model| {
                Ok(DynamicModelMapping {
                    public_model: model.public_model.clone(),
                    upstream_model: model.upstream_model.clone(),
                    upstream_override: model
                        .upstream_override
                        .as_ref()
                        .map(|value| {
                            Ok::<_, TransferError>(DynamicModelUpstreamOverride {
                                protocol: UpstreamProtocolKind::try_from(value.protocol.as_str())
                                    .map_err(|_| {
                                        TransferError::Invalid(format!(
                                            "dynamic destination `{}` has an invalid model protocol override",
                                            destination.id
                                        ))
                                    })?,
                                endpoint_url: value.endpoint_url.clone(),
                            })
                        })
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, TransferError>>()?;
        let portable = PortableProviderDefinition {
            preset_id: destination.preset_id.clone(),
            id: destination.legacy.id.clone(),
            name: destination.name.clone(),
            endpoint_url,
            upstream_protocol: protocol.as_str().to_string(),
            auth_kind: auth_kind.as_str().to_string(),
            models: mappings
                .iter()
                .map(|mapping| super::PortableProviderDefinitionModel {
                    public_model: mapping.public_model.clone(),
                    upstream_model: mapping.upstream_model.clone(),
                    upstream_override: mapping.upstream_override.as_ref().map(|value| {
                        super::PortableProviderDefinitionModelOverride {
                            protocol: value.protocol.as_str().to_string(),
                            endpoint_url: value.endpoint_url.clone(),
                        }
                    }),
                })
                .collect(),
            onboarding_draft: Some(draft),
        };
        providers.push(portable);
    }
    validate_portable_dynamic_providers(&providers, PAYLOAD_VERSION)
}

#[allow(clippy::type_complexity)]
fn platforms_from_destinations(
    destinations: &[PortableDestination],
    credentials: &[PortableCredential],
) -> Result<
    (
        Vec<PortablePlatformAccount>,
        Vec<PortablePlatformLink>,
        HashMap<String, Vec<ocg_domain::destination::CatalogModel>>,
        HashMap<String, String>,
        HashMap<String, i64>,
    ),
    TransferError,
> {
    let mut parents = Vec::new();
    let mut catalogs = HashMap::new();
    let mut snapshots = HashMap::new();
    let mut versions = HashMap::new();
    let dest_by_id: HashMap<_, _> = destinations
        .iter()
        .map(|destination| (destination.id.as_str(), destination))
        .collect();
    for destination in destinations {
        if destination.legacy.kind != LegacyDestinationKindDto::PlatformParent {
            continue;
        }
        let kind = match destination.platform_kind.as_deref() {
            Some("new_api") => PlatformKind::NewApi,
            Some("sub2api") => PlatformKind::Sub2api,
            _ => {
                return Err(TransferError::Invalid(format!(
                    "destination `{}` has an invalid platform kind",
                    destination.id
                )));
            }
        };
        let base_url = destination
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                TransferError::Invalid(format!(
                    "destination `{}` is missing its platform base url",
                    destination.id
                ))
            })?;
        if let Some(snapshot) = destination.platform_snapshot.clone() {
            snapshots.insert(destination.id.clone(), snapshot);
        }
        if let Some(version) = destination.platform_version {
            versions.insert(destination.id.clone(), version);
        }
        parents.push(PortablePlatformAccount {
            id: destination.legacy.id.clone(),
            kind,
            name: destination.name.clone(),
            base_url: base_url.to_string(),
        });
        catalogs.insert(
            destination.legacy.id.clone(),
            super::portable::catalog_from_portable(&destination.catalog),
        );
    }
    let mut links = Vec::new();
    for credential in credentials {
        if is_observer_purpose(credential_purpose(credential)) {
            continue;
        }
        let Some(destination) = dest_by_id.get(credential.destination_id.as_str()) else {
            continue;
        };
        if destination.legacy.kind != LegacyDestinationKindDto::PlatformParent {
            continue;
        }
        let mut group = credential.link_group.clone().unwrap_or_default();
        group.verified = false;
        group.subscription_type = None;
        links.push(PortablePlatformLink {
            account_id: credential.legacy_account_id.clone(),
            platform_account_id: destination.legacy.id.clone(),
            group,
        });
    }
    Ok((parents, links, catalogs, snapshots, versions))
}

fn cpa_from_destinations(
    destinations: &[PortableDestination],
    credentials: &[PortableCredential],
) -> (Option<String>, Option<String>) {
    let Some(destination) = destinations.iter().find(|destination| {
        destination.adapter == AdapterKindDto::Cpa
            || destination.legacy.id == crate::provider::CPA_PROVIDER_ID
    }) else {
        return (None, None);
    };
    let management_key = credentials.iter().find_map(|credential| {
        if credential_purpose(credential) == PURPOSE_CPA_OBSERVER
            || destination.observer_credential_id.as_deref() == Some(credential.id.as_str())
        {
            credential.management_key.clone()
        } else {
            None
        }
    });
    (destination.base_url.clone(), management_key)
}

fn identity_snapshot_from_credentials(
    credentials: &[PortableCredential],
    quota_pools: &[super::PortableQuotaPool],
) -> Result<Option<IdentityImportSnapshot>, TransferError> {
    let inference: Vec<_> = credentials
        .iter()
        .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
        .collect();
    if inference.is_empty() {
        if !quota_pools.is_empty() {
            return Err(TransferError::Invalid(
                "quota pool snapshot references accounts that are not in the package".to_string(),
            ));
        }
        return Ok(Some(IdentityImportSnapshot {
            identities: Vec::new(),
            accounts: Vec::new(),
            quota_pools: Vec::new(),
        }));
    }
    let mut identities = Vec::new();
    let mut seen_identities = HashSet::new();
    let mut linked = Vec::new();
    let mut account_ids = HashSet::new();
    for credential in inference {
        let account_id = credential.legacy_account_id.trim().to_string();
        account_ids.insert(account_id.clone());
        let identity_id = credential
            .identity_id
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        if seen_identities.insert(identity_id.clone()) {
            identities.push(ImportedIdentity {
                id: identity_id.clone(),
                label: credential
                    .identity_label
                    .clone()
                    .unwrap_or_else(|| credential.name.clone()),
                identity_confidence: credential
                    .identity_confidence
                    .clone()
                    .unwrap_or_else(|| "opaque".to_string()),
                authority_site: credential.authority_site.clone(),
                authority_subject: credential.authority_subject.clone(),
                enabled: credential.identity_enabled.unwrap_or(true),
                notes: credential.identity_notes.clone(),
            });
        }
        linked.push(ImportedAccountIdentity {
            account_id,
            identity_id,
            credential_id: credential.id.clone(),
            credential_version: credential.credential_version.unwrap_or(1),
            auth_state_version: credential.auth_state_version.unwrap_or(1),
            binding_id: credential
                .binding_id
                .clone()
                .unwrap_or_else(|| credential.id.clone()),
            binding_enabled: credential.binding_enabled.unwrap_or(true),
            binding_model_scope: credential.scope.clone(),
            allowed_endpoint_ids: credential.grants.allowed_endpoint_ids.clone(),
            allowed_origins: credential
                .grants
                .allowed_origins
                .iter()
                .filter_map(|origin| ocg_domain::credential::normalize_origin(origin))
                .collect(),
        });
    }
    let mut imported_pools = Vec::new();
    let mut covered = HashSet::new();
    for (index, pool) in quota_pools.iter().enumerate() {
        let prefix = || format!("quota pool {}", index + 1);
        if uuid::Uuid::parse_str(pool.id.trim()).is_err() || pool.member_account_ids.is_empty() {
            return Err(TransferError::Invalid(format!(
                "{} has an invalid id or no members",
                prefix()
            )));
        }
        match pool.subject_kind.as_str() {
            "credential" | "egress" => {}
            _ => {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid subject",
                    prefix()
                )));
            }
        }
        match pool.relation_confidence.as_str() {
            "unknown" | "declared" => {}
            "verified" => {
                return Err(TransferError::Invalid(format!(
                    "{} must not claim a verified relation",
                    prefix()
                )));
            }
            _ => {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid relation confidence",
                    prefix()
                )));
            }
        }
        match pool.policy_mode.as_str() {
            "observe_only" | "authoritative_limit" => {}
            _ => {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid policy mode",
                    prefix()
                )));
            }
        }
        let mut members = Vec::new();
        let mut seen_members = HashSet::new();
        for member in &pool.member_account_ids {
            let member = member.trim();
            if member.is_empty() || !account_ids.contains(member) || !seen_members.insert(member) {
                return Err(TransferError::Invalid(format!(
                    "{} references a missing, duplicate, or unknown account",
                    prefix()
                )));
            }
            covered.insert(member.to_string());
            members.push(member.to_string());
        }
        imported_pools.push(ImportedQuotaPool {
            id: pool.id.trim().to_string(),
            subject_kind: pool.subject_kind.clone(),
            subject_ref: pool.subject_ref.trim().to_string(),
            relation_confidence: pool.relation_confidence.clone(),
            policy_mode: pool.policy_mode.clone(),
            member_account_ids: members,
        });
    }
    let _ = covered;
    Ok(Some(IdentityImportSnapshot {
        identities,
        accounts: linked,
        quota_pools: imported_pools,
    }))
}

fn validated_accounts_from_credentials(
    destinations: &[PortableDestination],
    credentials: &[PortableCredential],
    dynamics: &[DynamicProviderRuntime],
) -> Result<Vec<ValidatedAccount>, TransferError> {
    let dest_by_id: HashMap<_, _> = destinations
        .iter()
        .map(|destination| (destination.id.as_str(), destination))
        .collect();
    let mut validated = Vec::new();
    for (index, credential) in credentials.iter().enumerate() {
        if is_observer_purpose(credential_purpose(credential)) {
            continue;
        }
        let destination = dest_by_id
            .get(credential.destination_id.as_str())
            .ok_or_else(|| {
                TransferError::Invalid(format!(
                    "credential `{}` names an unknown destination",
                    credential.id
                ))
            })?;
        let provider_id =
            credential
                .provider_id
                .clone()
                .unwrap_or_else(|| match destination.legacy.kind {
                    LegacyDestinationKindDto::Builtin | LegacyDestinationKindDto::Dynamic => {
                        destination.legacy.id.clone()
                    }
                    LegacyDestinationKindDto::CustomAccount
                    | LegacyDestinationKindDto::PlatformParent => {
                        crate::kernel::ids::CUSTOM_PROVIDER_ID.to_string()
                    }
                });
        let dynamic = dynamics
            .iter()
            .find(|runtime| crate::dynamic::provider_ids_equal(&runtime.id, &provider_id));
        let plan = builtin_provider(&provider_id);
        if plan.is_none() && dynamic.is_none() {
            return Err(TransferError::Invalid(format!(
                "credential `{}` references an unknown provider",
                credential.id
            )));
        }
        if let Some(plan) = plan
            && (plan.singleton_account_id.is_some()
                || plan.creation_availability == CreationAvailability::Unavailable)
        {
            return Err(TransferError::Invalid(format!(
                "credential `{}` uses a Plan that cannot be imported",
                credential.id
            )));
        }
        let account_type = credential
            .account_type
            .as_deref()
            .map(ModelAccountType::try_from)
            .transpose()
            .map_err(|_| {
                TransferError::Invalid(format!(
                    "credential `{}` has an invalid account type",
                    credential.id
                ))
            })?
            .unwrap_or(ModelAccountType::Key);
        let setup_step = credential
            .setup_step
            .as_deref()
            .map(ModelSetupStep::try_from)
            .transpose()
            .map_err(|_| {
                TransferError::Invalid(format!(
                    "credential `{}` has an invalid setup step",
                    credential.id
                ))
            })?
            .or_else(|| {
                credential
                    .onboarding_task
                    .as_ref()
                    .and_then(|task| ModelSetupStep::try_from(task.step.as_str()).ok())
            })
            .unwrap_or(ModelSetupStep::Ready);
        let requires_key = destination.auth_scheme != AuthSchemeDto::None;
        if account_type == ModelAccountType::Key && requires_key && credential.key.trim().is_empty()
        {
            return Err(TransferError::Invalid(format!(
                "credential `{}` is missing its account Key",
                credential.id
            )));
        }
        if let Some(plan) = plan
            && !credential.key.trim().is_empty()
        {
            crate::provider::validate_plan_key(plan, &credential.key).map_err(|_| {
                TransferError::Invalid(format!(
                    "credential `{}` has an invalid account Key",
                    credential.id
                ))
            })?;
        }
        let purchase_date = match credential.purchase_date.as_deref() {
            Some(value) if !value.trim().is_empty() => {
                normalize_purchase_date(value).map_err(|_| {
                    TransferError::Invalid(format!(
                        "credential `{}` has an invalid purchase date",
                        credential.id
                    ))
                })?
            }
            _ => String::new(),
        };
        let notes = match credential.notes.as_deref() {
            Some(value) => normalize_account_notes(value).map_err(|_| {
                TransferError::Invalid(format!("credential `{}` has invalid notes", credential.id))
            })?,
            None => None,
        };
        let verification_status = match credential.verification_status.as_deref() {
            Some(value) => ConnectionVerificationStatus::try_from(value).map_err(|_| {
                TransferError::Invalid(format!(
                    "credential `{}` has an invalid verification state",
                    credential.id
                ))
            })?,
            None => plan.map_or(
                ConnectionVerificationStatus::NotRequired,
                crate::provider::default_verification_status,
            ),
        };
        let connection_verified_at = credential
            .connection_verified_at
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| {
                TransferError::Invalid(format!(
                    "credential `{}` has an invalid verification time",
                    credential.id
                ))
            })?
            .map(|value| value.with_timezone(&Utc));
        let enabled =
            credential.enabled && (dynamic.is_some() || provider_allows_enablement(&provider_id));
        let (custom_config, capabilities) =
            custom_contract_from_destination(destination, &provider_id)?;
        let identity = credential
            .identity_id
            .as_ref()
            .map(|identity_id| ImportedAccountIdentity {
                account_id: credential.legacy_account_id.clone(),
                identity_id: identity_id.clone(),
                credential_id: credential.id.clone(),
                credential_version: credential.credential_version.unwrap_or(1),
                auth_state_version: credential.auth_state_version.unwrap_or(1),
                binding_id: credential
                    .binding_id
                    .clone()
                    .unwrap_or_else(|| credential.id.clone()),
                binding_enabled: credential.binding_enabled.unwrap_or(true),
                binding_model_scope: credential.scope.clone(),
                allowed_endpoint_ids: credential.grants.allowed_endpoint_ids.clone(),
                allowed_origins: credential
                    .grants
                    .allowed_origins
                    .iter()
                    .filter_map(|origin| ocg_domain::credential::normalize_origin(origin))
                    .collect(),
            });
        let ollama_billing_tier = match credential.ollama_billing_tier.as_deref() {
            None | Some("") => None,
            Some(value) => Some(crate::provider::OllamaBillingTier::parse(value).map_err(
                |_| {
                    TransferError::Invalid(format!(
                        "credential `{}` has an invalid Ollama billing tier",
                        credential.id
                    ))
                },
            )?),
        };
        validated.push(ValidatedAccount {
            portable_index: index,
            id: Some(credential.legacy_account_id.clone()),
            provider_id,
            name: credential.name.clone(),
            username: credential.username.clone(),
            key: Zeroizing::new(credential.key.clone()),
            password: credential
                .password
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| Zeroizing::new(value.to_string())),
            enabled,
            account_type,
            setup_step,
            purchase_date,
            expires_on: String::new(),
            notes,
            verification_status,
            connection_verified_at,
            custom_config,
            capabilities,
            ollama_billing_tier,
            credential_kind: (destination.adapter == AdapterKindDto::Http)
                .then_some(if requires_key {
                    CredentialKind::ApiKey
                } else {
                    CredentialKind::None
                })
                .or_else(|| dynamic.map(|runtime| runtime.auth_kind.credential_kind()))
                .or_else(|| plan.map(|plan| plan.credential_kind))
                .or_else(|| {
                    credential
                        .credential_kind
                        .as_deref()
                        .and_then(|value| CredentialKind::try_from(value).ok())
                })
                .unwrap_or(CredentialKind::ApiKey),
            quota_scope: dynamic
                .map(|runtime| runtime.auth_kind.quota_scope())
                .or_else(|| plan.map(|plan| plan.quota_scope))
                .or_else(|| {
                    credential
                        .quota_scope
                        .as_deref()
                        .and_then(|value| QuotaScope::try_from(value).ok())
                })
                .unwrap_or(QuotaScope::Key),
            identity,
            cooldowns: PortableCooldowns {
                until: None,
                generic: super::portable::parse_cooldown_field(
                    credential.cooldowns.generic_until.as_deref(),
                ),
                five_hours: super::portable::parse_cooldown_field(
                    credential.cooldowns.five_hour_until.as_deref(),
                ),
                week: super::portable::parse_cooldown_field(
                    credential.cooldowns.week_until.as_deref(),
                ),
                month: super::portable::parse_cooldown_field(
                    credential.cooldowns.month_until.as_deref(),
                ),
                free: super::portable::parse_cooldown_field(
                    credential.cooldowns.free_until.as_deref(),
                ),
            },
        });
    }
    Ok(validated)
}

fn custom_contract_from_destination(
    destination: &PortableDestination,
    provider_id: &str,
) -> Result<
    (
        Option<AccountCustomConfigInput>,
        Vec<AccountModelCapabilityInput>,
    ),
    TransferError,
> {
    if provider_id != crate::kernel::ids::CUSTOM_PROVIDER_ID
        || destination.legacy.kind == LegacyDestinationKindDto::PlatformParent
    {
        return Ok((None, Vec::new()));
    }
    let endpoint_url = destination
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            TransferError::Invalid(format!(
                "destination `{}` is missing its Custom Endpoint",
                destination.id
            ))
        })?;
    if endpoint_url.chars().count() > MAX_ENDPOINT_CHARS {
        return Err(TransferError::Invalid(format!(
            "destination `{}` has a Custom Endpoint that is too long",
            destination.id
        )));
    }
    let endpoint_url = crate::custom::validate_custom_endpoint_url(endpoint_url).map_err(|_| {
        TransferError::Invalid(format!(
            "destination `{}` has an invalid Custom Endpoint",
            destination.id
        ))
    })?;
    let protocol = destination
        .protocols
        .first()
        .copied()
        .map(super::portable::protocol_from_dto)
        .ok_or_else(|| {
            TransferError::Invalid(format!(
                "destination `{}` has an invalid upstream protocol",
                destination.id
            ))
        })?;
    if destination.catalog.is_empty() || destination.catalog.len() > MAX_CAPABILITIES {
        return Err(TransferError::Invalid(format!(
            "destination `{}` has an invalid model capability list",
            destination.id
        )));
    }
    let capabilities = destination
        .catalog
        .iter()
        .map(|model| AccountModelCapabilityInput {
            public_model: model.public_model.clone(),
            upstream_model: model.upstream_model.clone(),
            protocol,
            source: Some("import".to_string()),
        })
        .collect();
    Ok((
        Some(AccountCustomConfigInput {
            endpoint_url,
            upstream_protocol: protocol,
        }),
        capabilities,
    ))
}

fn portable_observer_credential(
    row: &crate::db::platform::ObserverCredentialRow,
    management_key: Option<String>,
) -> PortableCredential {
    PortableCredential {
        id: row.id.clone(),
        legacy_account_id: row.id.clone(),
        destination_id: row.destination_id.clone(),
        name: row.name.clone(),
        notes: None,
        has_secret: row.has_secret,
        enabled: false,
        routing_rank: 0,
        scope: ModelScope::All,
        grants: CredentialGrantsDto {
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        },
        auth_state: AuthState::Unknown,
        last_error: None,
        cooldowns: CredentialCooldownsDto {
            generic_until: None,
            five_hour_until: None,
            week_until: None,
            month_until: None,
            free_until: None,
        },
        quota_pool_id: None,
        onboarding_task: None,
        purchase_date: None,
        identity_id: row.identity_id.clone(),
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
        management_key,
        purpose: Some(row.purpose.clone()),
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

pub(super) fn observer_plaintext_by_parent(
    unified: &UnifiedNewModelImport,
) -> HashMap<String, String> {
    let dest_by_id: HashMap<_, _> = unified
        .destinations
        .iter()
        .map(|destination| (destination.id.as_str(), destination))
        .collect();
    let mut out = HashMap::new();
    for credential in &unified.credentials {
        if credential_purpose(credential) != PURPOSE_PLATFORM_OBSERVER {
            continue;
        }
        let Some(destination) = dest_by_id.get(credential.destination_id.as_str()) else {
            continue;
        };
        if let Some(key) = credential
            .management_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            out.insert(destination.legacy.id.clone(), key.to_string());
        }
    }
    out
}

pub(super) fn finish_new_model_migration(
    payload: &mut PortablePayload,
    exported_at: String,
) -> Result<ValidatedMigration, TransferError> {
    debug_assert!(payload.version >= V7_PAYLOAD_VERSION);
    let (accounts, unified) = validate_new_model_payload(payload)?;
    let node = validate_node_state(
        payload.node.take().expect("V7+ node was checked"),
        &accounts,
        false,
    )?;
    Ok(ValidatedMigration {
        exported_at,
        accounts,
        node: Zeroizing::new(node),
        unified,
        legacy_exclusive_radio_repair: false,
    })
}
