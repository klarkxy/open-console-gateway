//! Pure account identity, setup state, and cooldown policy.

use crate::catalog::{CredentialKind, QuotaScope};
use crate::ids::ZEN_FREE_ACCOUNT_ID;
use crate::provider::{
    ProviderBindingError, default_credential_kind, default_provider_id, default_quota_scope,
    validate_account_binding,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Ready Key accounts are created disabled. The operator enables them after
/// checking the connection. Import, toggle, and managed Key-verify keep their
/// own stored or completed state. Connection tests never flip this.
pub const NEW_READY_KEY_ACCOUNT_ENABLED: bool = false;

/// The default All-scope inference binding stays authorized. The account
/// switch is the draft/live gate; Edit binding can still disable that Key.
pub const DEFAULT_INFERENCE_BINDING_ENABLED: bool = true;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    #[serde(default = "default_provider_id")]
    pub provider_id: String,
    #[serde(default = "default_credential_kind")]
    pub credential_kind: CredentialKind,
    #[serde(default = "default_quota_scope")]
    pub quota_scope: QuotaScope,
    pub name: String,
    pub username: Option<String>,
    pub password_cipher: Option<String>,
    pub key_cipher: String,
    pub enabled: bool,
    #[serde(default)]
    pub account_type: AccountType,
    #[serde(default)]
    pub setup_step: AccountSetupStep,
    pub referral_code: Option<String>,
    #[serde(alias = "recharge_date")]
    pub purchase_date: String,
    #[serde(default)]
    pub expires_on: String,
    /// Derived: when the account becomes usable after every active cooldown expires.
    /// Kept for backwards compatibility; `None` means currently available.
    pub cooldown_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cooldown_generic_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cooldown_5h_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cooldown_week_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cooldown_month_until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cooldown_free_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// A persisted upstream 401 marker. Accounts with an auth error remain
    /// enabled for management purposes, but are excluded from gateway routing
    /// until their key is replaced or a direct ping proves the key works again.
    #[serde(default)]
    pub auth_error: Option<String>,
    /// Optional freeform note. Empty or omitted is valid.
    #[serde(default)]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountType {
    #[default]
    Key,
    Managed,
}

impl AccountType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Managed => "managed",
        }
    }
}

impl TryFrom<&str> for AccountType {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "key" => Ok(Self::Key),
            "managed" => Ok(Self::Managed),
            _ => Err(format!("unknown account type `{value}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountSetupStep {
    GoogleAccount,
    OpencodeRegistration,
    Payment,
    KeyVerification,
    #[default]
    Ready,
}

impl AccountSetupStep {
    pub fn next(self) -> Option<Self> {
        match self {
            Self::GoogleAccount => Some(Self::OpencodeRegistration),
            Self::OpencodeRegistration => Some(Self::Payment),
            Self::Payment => Some(Self::KeyVerification),
            Self::KeyVerification | Self::Ready => None,
        }
    }

    pub fn is_ready(self) -> bool {
        self == Self::Ready
    }

    /// Wizard progress index for unfinished steps. `ready` is not part of the wizard.
    pub const fn wizard_index(self) -> Option<u8> {
        match self {
            Self::GoogleAccount => Some(0),
            Self::OpencodeRegistration => Some(1),
            Self::Payment => Some(2),
            Self::KeyVerification => Some(3),
            Self::Ready => None,
        }
    }

    /// Forward exactly one step, or rewind to any earlier unfinished step.
    pub fn can_transition_to(self, to: Self) -> bool {
        let Some(from_i) = self.wizard_index() else {
            return false;
        };
        let Some(to_i) = to.wizard_index() else {
            return false;
        };
        to_i == from_i + 1 || to_i < from_i
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GoogleAccount => "google_account",
            Self::OpencodeRegistration => "opencode_registration",
            Self::Payment => "payment",
            Self::KeyVerification => "key_verification",
            Self::Ready => "ready",
        }
    }
}

impl TryFrom<&str> for AccountSetupStep {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "google_account" => Ok(Self::GoogleAccount),
            "opencode_registration" => Ok(Self::OpencodeRegistration),
            "payment" => Ok(Self::Payment),
            "key_verification" => Ok(Self::KeyVerification),
            "ready" => Ok(Self::Ready),
            _ => Err(format!("unknown account setup step `{value}`")),
        }
    }
}

impl Account {
    /// Compatibility wrapper around [`crate::provider::validate_account_binding`].
    pub fn validate_provider_binding(&self) -> Result<(), ProviderBindingError> {
        validate_account_binding(
            &self.id,
            &self.provider_id,
            self.credential_kind,
            self.quota_scope,
        )
    }

    pub fn is_zen_free(&self) -> bool {
        self.id == ZEN_FREE_ACCOUNT_ID
    }

    /// Latest end among every cooldown window (UI / any-channel busy).
    pub fn cooldown_ends_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        [
            self.cooldown_generic_until,
            self.cooldown_5h_until,
            self.cooldown_week_until,
            self.cooldown_month_until,
            self.cooldown_free_until,
        ]
        .into_iter()
        .flatten()
        .filter(|until| *until > now)
        .max()
    }

    pub fn is_cooling_at(&self, now: DateTime<Utc>) -> bool {
        self.cooldown_ends_at(now).is_some()
    }

    /// Go routing ignores free promo cooldown; free routing ignores Go usage windows.
    /// Free 429s are IP-shared: the selector treats any active `cooldown_free_until`
    /// as exhausting the whole free channel.
    pub fn cooldown_ends_at_for(
        &self,
        channel: UpstreamChannel,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let windows: &[Option<DateTime<Utc>>] = match channel {
            UpstreamChannel::Go => &[
                self.cooldown_generic_until,
                self.cooldown_5h_until,
                self.cooldown_week_until,
                self.cooldown_month_until,
            ],
            UpstreamChannel::Free => &[self.cooldown_generic_until, self.cooldown_free_until],
        };
        windows
            .iter()
            .copied()
            .flatten()
            .filter(|until| *until > now)
            .max()
    }

    pub fn is_cooling_for(&self, channel: UpstreamChannel, now: DateTime<Utc>) -> bool {
        self.cooldown_ends_at_for(channel, now).is_some()
    }
}

/// Upstream product channel for account selection and cooldown windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamChannel {
    Go,
    Free,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID};
    use chrono::Duration;

    fn utc(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        DateTime::from_naive_utc_and_offset(
            chrono::NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(hour, 0, 0)
                .unwrap(),
            Utc,
        )
    }

    fn sample_account() -> Account {
        Account {
            id: "account-1".into(),
            provider_id: OPENCODE_PROVIDER_ID.into(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: "go".into(),
            username: None,
            password_cipher: None,
            key_cipher: "cipher".into(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: utc(2026, 1, 1, 0),
            updated_at: utc(2026, 1, 1, 0),
        }
    }

    #[test]
    fn account_type_parses_known_values_and_rejects_unknown() {
        assert_eq!(AccountType::Key.as_str(), "key");
        assert_eq!(AccountType::Managed.as_str(), "managed");
        assert_eq!(AccountType::try_from("key").unwrap(), AccountType::Key);
        assert_eq!(
            AccountType::try_from("managed").unwrap(),
            AccountType::Managed
        );
        assert_eq!(AccountType::default(), AccountType::Key);
        assert_eq!(
            AccountType::try_from("cookie").unwrap_err(),
            "unknown account type `cookie`"
        );
        assert_eq!(
            serde_json::to_value(AccountType::Managed).unwrap(),
            serde_json::json!("managed")
        );
        assert_eq!(
            serde_json::from_value::<AccountType>(serde_json::json!("key")).unwrap(),
            AccountType::Key
        );
        assert!(serde_json::from_value::<AccountType>(serde_json::json!("gateway")).is_err());
    }

    #[test]
    fn setup_step_parses_and_enforces_single_step_or_rewind() {
        assert_eq!(AccountSetupStep::GoogleAccount.as_str(), "google_account");
        assert_eq!(
            AccountSetupStep::try_from("opencode_registration").unwrap(),
            AccountSetupStep::OpencodeRegistration
        );
        assert_eq!(
            AccountSetupStep::try_from("payment").unwrap(),
            AccountSetupStep::Payment
        );
        assert_eq!(
            AccountSetupStep::try_from("key_verification").unwrap(),
            AccountSetupStep::KeyVerification
        );
        assert_eq!(
            AccountSetupStep::try_from("ready").unwrap(),
            AccountSetupStep::Ready
        );
        assert_eq!(
            AccountSetupStep::try_from("done").unwrap_err(),
            "unknown account setup step `done`"
        );
        assert_eq!(AccountSetupStep::default(), AccountSetupStep::Ready);
        assert!(AccountSetupStep::Ready.is_ready());
        assert!(!AccountSetupStep::KeyVerification.is_ready());
        assert_eq!(
            AccountSetupStep::GoogleAccount.next(),
            Some(AccountSetupStep::OpencodeRegistration)
        );
        assert_eq!(
            AccountSetupStep::OpencodeRegistration.next(),
            Some(AccountSetupStep::Payment)
        );
        assert_eq!(
            AccountSetupStep::Payment.next(),
            Some(AccountSetupStep::KeyVerification)
        );
        assert_eq!(AccountSetupStep::KeyVerification.next(), None);
        assert_eq!(AccountSetupStep::Ready.next(), None);
        assert_eq!(AccountSetupStep::Ready.wizard_index(), None);

        assert!(
            AccountSetupStep::GoogleAccount
                .can_transition_to(AccountSetupStep::OpencodeRegistration)
        );
        assert!(AccountSetupStep::Payment.can_transition_to(AccountSetupStep::GoogleAccount));
        assert!(!AccountSetupStep::GoogleAccount.can_transition_to(AccountSetupStep::Payment));
        assert!(!AccountSetupStep::GoogleAccount.can_transition_to(AccountSetupStep::Ready));
        assert!(!AccountSetupStep::KeyVerification.can_transition_to(AccountSetupStep::Ready));
        assert!(!AccountSetupStep::Ready.can_transition_to(AccountSetupStep::GoogleAccount));
        assert!(!AccountSetupStep::Payment.can_transition_to(AccountSetupStep::Payment));
        assert_eq!(
            serde_json::from_value::<AccountSetupStep>(serde_json::json!("key_verification"))
                .unwrap(),
            AccountSetupStep::KeyVerification
        );
    }

    #[test]
    fn provider_binding_accepts_matching_pairs_and_protects_zen_singleton() {
        let go = sample_account();
        go.validate_provider_binding().unwrap();
        assert!(!go.is_zen_free());

        let mut zen = sample_account();
        zen.id = ZEN_FREE_ACCOUNT_ID.into();
        zen.provider_id = OPENCODE_ZEN_FREE_PROVIDER_ID.into();
        zen.credential_kind = CredentialKind::None;
        zen.quota_scope = QuotaScope::EgressIp;
        zen.validate_provider_binding().unwrap();
        assert!(zen.is_zen_free());

        let mut reserved = sample_account();
        reserved.id = ZEN_FREE_ACCOUNT_ID.into();
        assert!(matches!(
            reserved.validate_provider_binding(),
            Err(ProviderBindingError::ReservedAccountId(id)) if id == ZEN_FREE_ACCOUNT_ID
        ));

        let mut singleton = sample_account();
        singleton.provider_id = OPENCODE_ZEN_FREE_PROVIDER_ID.into();
        singleton.credential_kind = CredentialKind::None;
        singleton.quota_scope = QuotaScope::EgressIp;
        assert!(matches!(
            singleton.validate_provider_binding(),
            Err(ProviderBindingError::SingletonAccountRequired(id)) if id == ZEN_FREE_ACCOUNT_ID
        ));
        assert!(!singleton.is_zen_free());

        let mut mismatch = sample_account();
        mismatch.credential_kind = CredentialKind::None;
        assert!(matches!(
            mismatch.validate_provider_binding(),
            Err(ProviderBindingError::BindingMismatch { .. })
        ));

        let mut unknown = sample_account();
        unknown.provider_id = "unknown".into();
        assert!(matches!(
            unknown.validate_provider_binding(),
            Err(ProviderBindingError::UnknownProvider { .. })
        ));
    }

    #[test]
    fn zen_identity_is_the_reserved_account_id() {
        let mut account = sample_account();
        assert!(!account.is_zen_free());
        account.id = ZEN_FREE_ACCOUNT_ID.into();
        assert!(account.is_zen_free());
    }

    #[test]
    fn cooldown_windows_are_channel_specific_and_ignore_derived_until() {
        let now = utc(2026, 8, 23, 12);
        let past = now - Duration::hours(1);
        let soon = now + Duration::hours(1);
        let later = now + Duration::hours(3);

        let mut account = sample_account();
        account.cooldown_until = Some(later);
        assert!(!account.is_cooling_at(now));
        assert_eq!(account.cooldown_ends_at(now), None);

        account.cooldown_5h_until = Some(past);
        account.cooldown_week_until = Some(soon);
        account.cooldown_free_until = Some(later);
        account.cooldown_generic_until = Some(past);

        assert_eq!(account.cooldown_ends_at(now), Some(later));
        assert!(account.is_cooling_at(now));
        assert_eq!(
            account.cooldown_ends_at_for(UpstreamChannel::Go, now),
            Some(soon)
        );
        assert!(account.is_cooling_for(UpstreamChannel::Go, now));
        assert_eq!(
            account.cooldown_ends_at_for(UpstreamChannel::Free, now),
            Some(later)
        );
        assert!(account.is_cooling_for(UpstreamChannel::Free, now));

        account.cooldown_week_until = Some(past);
        assert!(!account.is_cooling_for(UpstreamChannel::Go, now));
        assert!(account.is_cooling_for(UpstreamChannel::Free, now));

        account.cooldown_free_until = Some(now);
        assert!(!account.is_cooling_for(UpstreamChannel::Free, now));
        assert!(!account.is_cooling_at(now));
    }

    #[test]
    fn generic_cooldown_blocks_both_channels() {
        let now = utc(2026, 8, 23, 12);
        let mut account = sample_account();
        account.cooldown_generic_until = Some(now + Duration::minutes(15));
        assert!(account.is_cooling_for(UpstreamChannel::Go, now));
        assert!(account.is_cooling_for(UpstreamChannel::Free, now));
        assert_eq!(
            account.cooldown_ends_at_for(UpstreamChannel::Go, now),
            account.cooldown_generic_until
        );
        assert_eq!(
            account.cooldown_ends_at_for(UpstreamChannel::Free, now),
            account.cooldown_generic_until
        );
    }
}
