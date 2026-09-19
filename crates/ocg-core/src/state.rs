use crate::crypto::KeyCipher;
use crate::db::Database;
use crate::desktop::DesktopCapabilities;
use crate::gateway_runtime::GatewayRebindHost;
use crate::kernel::pricing::{PricingEstimate, PricingSnapshot};
use crate::models::{
    AppConfig, normalize_client_root_url, normalize_opencode_invite_url, normalize_proxy_url,
};
use crate::pricing::{embedded_seed, ensure_current_adjustment_policy, ensure_seed_model_coverage};
use crate::routing_runtime::RoutingRuntime;
use parking_lot::{Mutex, RwLock};
use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub use crate::desktop::{
    AutoStartSync, DesktopUpdatePhase, DesktopUpdateStartError, DesktopUpdateStarter,
    DesktopUpdateStatus, DockVisibilitySync,
};
pub use crate::gateway_runtime::GatewayHandle;

const CLIENT_ROOT_URL_ENV: &str = "OCG_CLIENT_ROOT_URL";

// Note: Mutex lock ordering is (1) settings_update, (2) db, (3) config,
// (4) http_client, (5) gateway, (6) pricing, (7) zen_free_models,
// (8) cpa_models, (9) unpublished_public_models, (10) provider_contracts,
// (11) dynamic_providers, (12) routing, (13) credential_snapshot.
// The CPA runtime status mutex is never held while acquiring another sync lock.
// `activate_zen_free_model_catalog` acquires db → http_client →
// zen_free_models → provider_contracts, then drops those before
// `routing.reset()`. `reload_provider_contracts_locked` may already hold db
// and then takes zen_free_models (read, dropped) before provider_contracts
// (write). The desktop update-status mutex and the async pricing_refresh
// guard are never held while acquiring another sync lock. The credential
// snapshot write lock is always taken last (after db/config reads) and never
// held while acquiring another lock; auth-path readers take only the
// snapshot read lock. Never acquire in reverse order; always drop one before
// acquiring another where possible. Do not hold the routing lock across DB
// or network I/O. `gateway_clock` is immutable after construction and
// lock-free to sample; the executor samples wall/mono before the db lock.
// Async gates: `settings_host_effects` (settings persist → listener rebind →
// compensation) is acquired before `gateway_lifecycle` when a settings write
// also rebinds. Never hold a parking_lot lock across those awaits.
// Account, key, and usage-sync writers take `settings_update` only.
pub struct CoreStateInner {
    pub db: Mutex<Database>,
    pub config: Mutex<AppConfig>,
    client_root_url_override: Option<String>,
    gateway_port_override: OnceLock<u16>,
    pub settings_update: Mutex<()>,
    /// Serializes settings persist → listener rebind → compensation. This async
    /// gate may span listener bind awaits; the synchronous `settings_update`
    /// mutex may not. Account, key, and usage-sync writers do not take it.
    settings_host_effects: tokio::sync::Mutex<()>,
    settings_revision: AtomicU64,
    /// Dashboard V3 process generation. Assigned once per CoreState, never
    /// persisted, and independent of `settings_revision` so a CAS token from a
    /// previous process cannot be reused after restart.
    process_generation: u64,
    /// Authenticating credentials (value -> id/name) covering the primary
    /// key and enabled sub keys; see `gateway_keys` for the invalidation
    /// model. Written only under `settings_update` (key API) or from
    /// `set_config` (primary refresh).
    pub credential_snapshot: RwLock<crate::gateway_keys::CredentialSnapshot>,
    pub gateway: Mutex<Option<GatewayHandle>>,
    /// Serializes complete listener replacement transitions. This async gate
    /// may span listener shutdown awaits; the synchronous `gateway` mutex may
    /// not.
    gateway_lifecycle: tokio::sync::Mutex<()>,
    pub dashboard_session_token: Mutex<String>,
    dashboard_local_mode: AtomicBool,
    /// Number of spawned non-loopback listener tasks that have not yet
    /// terminated. This makes the shared trust flag fail-closed even for
    /// directly bound handles that have not been installed into `gateway`.
    dashboard_public_listeners: AtomicU64,
    /// Process-level auto-start, Dock, and desktop-update hooks. Unset in CLI/Docker.
    desktop: DesktopCapabilities,
    pub dashboard_dir: Mutex<Option<PathBuf>>,
    http_client: Mutex<Arc<crate::http_client::ForwardRouteSet>>,
    pricing: RwLock<Arc<PricingSnapshot>>,
    pub pricing_refresh: tokio::sync::Mutex<()>,
    zen_free_models: RwLock<Arc<crate::kernel::zen::ZenFreeModelCatalog>>,
    cpa_models: RwLock<Arc<Vec<String>>>,
    unpublished_public_models: RwLock<Arc<HashSet<String>>>,
    pub zen_free_models_refresh: tokio::sync::Mutex<()>,
    pub provider_models_refresh: tokio::sync::Mutex<()>,
    pub provider_usage_refresh: tokio::sync::Mutex<()>,
    /// Serializes typed operations against the one local CPA integration.
    /// Network calls may hold this async gate but never the SQLite mutex.
    pub cpa_operations: tokio::sync::Mutex<()>,
    /// Process-owned CPA runtime Host. Registered by the desktop app and CLI
    /// at startup on Windows/Unix; unset only on other targets. Dashboard CPA
    /// mutations are serialized by `cpa_operations`.
    pub(crate) cpa_runtime: crate::cpa_runtime::CpaRuntimeCapabilities,
    provider_contracts: RwLock<Arc<crate::provider_contracts::EffectiveContractSet>>,
    dynamic_providers: RwLock<Arc<Vec<crate::dynamic::DynamicProviderRuntime>>>,
    pub routing: RoutingRuntime,
    /// Ephemeral resource admission. Never held while acquiring the database lock.
    pub(crate) recovery: Arc<crate::gateway::recovery::RecoveryRuntime>,
    pub browser: crate::browser::BrowserRuntime,
    /// Official Go usage sync gates (concurrency, dedupe, clock/jitter seams).
    /// The background loop is started from gateway startup, not construction.
    pub usage_sync: crate::usage_sync::UsageSyncRuntime,
    /// Host-private dual clock for Gateway selection (wall + monotonic).
    /// Distinct from `usage_sync`'s calendar clock and not stored on request
    /// snapshots. Sampled through [`Self::sample_gateway_clock`].
    gateway_clock: crate::gateway_clock::GatewayClock,
    pub data_dir: PathBuf,
    pub cipher: Arc<dyn KeyCipher + Send + Sync>,
}

pub type CoreState = Arc<CoreStateInner>;

pub(crate) struct ImportedNodeRuntime {
    config: AppConfig,
    http_client: crate::http_client::ForwardRouteSet,
    zen_free_models: crate::kernel::zen::ZenFreeModelCatalog,
    provider_contracts: crate::provider_contracts::EffectiveContractSet,
    dynamic_providers: Vec<crate::dynamic::DynamicProviderRuntime>,
    credentials: crate::gateway_keys::CredentialSnapshot,
}

/// One exact upstream model id the executor can send, used by Settings
/// `proxy_supported_models` and list-mode route-set construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyModelCandidate {
    pub id: String,
    pub preferred_protocol: String,
    pub zen_free: bool,
}

pub(crate) fn build_proxy_model_candidates(
    contracts: &crate::provider_contracts::EffectiveContractSet,
    custom_runtimes: &[crate::custom::CustomAccountRuntime],
    dynamic_providers: &[crate::dynamic::DynamicProviderRuntime],
    cpa_models: &[String],
) -> Vec<ProxyModelCandidate> {
    use crate::kernel::ids::normalize_model_name;
    use crate::provider::ProviderAdapterKind;
    use std::collections::BTreeMap;

    let mut by_key: BTreeMap<String, ProxyModelCandidate> = BTreeMap::new();
    let mut insert = |id: &str, preferred_protocol: &str, zen_free: bool| {
        let id = id.trim();
        if id.is_empty() {
            return;
        }
        let key = normalize_model_name(id);
        let entry = by_key.entry(key).or_insert_with(|| ProxyModelCandidate {
            id: id.to_string(),
            preferred_protocol: preferred_protocol.to_string(),
            zen_free,
        });
        if zen_free {
            entry.zen_free = true;
        }
    };

    for contract in contracts.providers.values() {
        let zen_free = contract.adapter_kind == ProviderAdapterKind::ZenFree;
        for model in contract.models.values() {
            if !model.has_enabled_protocol() {
                continue;
            }
            let protocol = if model
                .protocols
                .get(model.preferred_protocol.as_str())
                .is_some_and(|row| row.enabled)
            {
                model.preferred_protocol.as_str()
            } else {
                model
                    .enabled_protocols()
                    .first()
                    .map(|protocol| protocol.as_str())
                    .unwrap_or(model.preferred_protocol.as_str())
            };
            insert(&model.model_id, protocol, zen_free);
        }
    }

    for runtime in custom_runtimes.iter().filter(|runtime| runtime.eligible()) {
        for capability in &runtime.capabilities {
            insert(
                &capability.upstream_model,
                capability.protocol.as_str(),
                false,
            );
        }
    }

    for provider in dynamic_providers {
        for mapping in &provider.mappings {
            let protocol = provider.effective_route(mapping).protocol;
            insert(&mapping.upstream_model, protocol.as_str(), false);
        }
    }

    for id in cpa_models {
        insert(id, "chat_completions", false);
    }

    by_key.into_values().collect()
}

fn proxy_candidate_ids(candidates: &[ProxyModelCandidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect()
}

fn build_proxy_route_set(
    config: &crate::models::AppConfig,
    contracts: &crate::provider_contracts::EffectiveContractSet,
    custom_runtimes: &[crate::custom::CustomAccountRuntime],
    dynamic_providers: &[crate::dynamic::DynamicProviderRuntime],
    cpa_models: &[String],
) -> crate::Result<crate::http_client::ForwardRouteSet> {
    let candidates =
        build_proxy_model_candidates(contracts, custom_runtimes, dynamic_providers, cpa_models);
    crate::http_client::build_route_set_from_known_models(config, &proxy_candidate_ids(&candidates))
}

/// Host-effect failures from [`CoreStateInner::apply_host_settings`].
///
/// Adapters map variants onto their existing status/code/message without
/// changing V2 or V3 DTO shapes.
#[derive(Debug)]
pub enum HostSettingsError {
    AutoStartUnsupported,
    DockVisibilityUnsupported,
    Persist(anyhow::Error),
    Sync(String),
    GatewayBind(anyhow::Error),
}

impl HostSettingsError {
    pub const AUTO_START_UNAVAILABLE: &'static str = crate::desktop::AUTO_START_UNAVAILABLE;
    pub const DOCK_VISIBILITY_UNAVAILABLE: &'static str =
        crate::desktop::DOCK_VISIBILITY_UNAVAILABLE;
}

impl fmt::Display for HostSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AutoStartUnsupported => f.write_str(Self::AUTO_START_UNAVAILABLE),
            Self::DockVisibilityUnsupported => f.write_str(Self::DOCK_VISIBILITY_UNAVAILABLE),
            Self::Persist(error) => write!(f, "{error}"),
            Self::Sync(message) => f.write_str(message),
            Self::GatewayBind(error) => write!(f, "failed to rebind gateway listener: {error}"),
        }
    }
}

impl std::error::Error for HostSettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Persist(error) | Self::GatewayBind(error) => Some(error.as_ref()),
            Self::AutoStartUnsupported | Self::DockVisibilityUnsupported | Self::Sync(_) => None,
        }
    }
}

impl CoreStateInner {
    pub fn new(
        db: Database,
        data_dir: PathBuf,
        cipher: Arc<dyn KeyCipher + Send + Sync>,
    ) -> crate::Result<Self> {
        let client_root_url_override = client_root_url_override_from_env()?;
        Self::new_with_client_root_url_override(db, data_dir, cipher, client_root_url_override)
    }

    /// Test-support constructor: inject immutable wall/mono sources at
    /// construction. Production callers must use [`Self::new`], which always
    /// samples `Utc::now` / `Instant::now`. Integration tests link this crate
    /// without `cfg(test)`, so the seam is `#[doc(hidden)]` rather than a
    /// live mutation API on a public clock type.
    #[doc(hidden)]
    pub fn new_with_test_gateway_clock(
        db: Database,
        data_dir: PathBuf,
        cipher: Arc<dyn KeyCipher + Send + Sync>,
        wall: impl Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync + 'static,
        mono: impl Fn() -> std::time::Instant + Send + Sync + 'static,
    ) -> crate::Result<Self> {
        let client_root_url_override = client_root_url_override_from_env()?;
        Self::construct(
            db,
            data_dir,
            cipher,
            client_root_url_override,
            crate::gateway_clock::GatewayClock::from_sources(wall, mono),
        )
    }

    fn new_with_client_root_url_override(
        db: Database,
        data_dir: PathBuf,
        cipher: Arc<dyn KeyCipher + Send + Sync>,
        client_root_url_override: Option<String>,
    ) -> crate::Result<Self> {
        Self::construct(
            db,
            data_dir,
            cipher,
            client_root_url_override,
            crate::gateway_clock::GatewayClock::system(),
        )
    }

    fn construct(
        db: Database,
        data_dir: PathBuf,
        cipher: Arc<dyn KeyCipher + Send + Sync>,
        client_root_url_override: Option<String>,
        gateway_clock: crate::gateway_clock::GatewayClock,
    ) -> crate::Result<Self> {
        crate::auth::bootstrap_admin_from_env(&db)?;
        let browser_recovery =
            crate::browser::recover_staged_browser_profiles(&data_dir, |account_id| {
                Ok(db.get_account(account_id)?.is_some())
            });
        if browser_recovery.has_activity() {
            let summary = browser_recovery.summary();
            let level = if browser_recovery.issues.is_empty() {
                "info"
            } else {
                eprintln!("warning: {summary}: {}", browser_recovery.issues.join("; "));
                "warn"
            };
            let _ = db.log_gateway(level, "browser", &summary);
        }
        let (config, needs_persist) = load_config(&db)?;
        config.validate().map_err(anyhow::Error::msg)?;
        if needs_persist {
            // Persist generated defaults and drop fields removed from AppConfig.
            save_config(&db, &config)?;
        }
        let credential_snapshot =
            crate::gateway_keys::build_credential_snapshot(&db, &config.gateway_key)?;
        let pricing = match db.latest_pricing_snapshot()? {
            Some(snapshot) => {
                let previous_revision = snapshot.revision.clone();
                let snapshot = ensure_current_adjustment_policy(snapshot);
                let snapshot = ensure_seed_model_coverage(snapshot);
                if snapshot.revision != previous_revision {
                    db.insert_pricing_snapshot(&snapshot)?;
                }
                snapshot
            }
            None => {
                let snapshot = embedded_seed();
                db.insert_pricing_snapshot(&snapshot)?;
                snapshot
            }
        };
        let zen_free_models = db.zen_free_model_catalog()?.unwrap_or_default();
        let cpa_models = db
            .cpa_model_catalog()?
            .map(|catalog| crate::db::CpaCatalogModel::enabled_ids(&catalog.models))
            .unwrap_or_default();
        let unpublished_public_models = db
            .list_unpublished_public_models()?
            .into_iter()
            .collect::<HashSet<_>>();
        let custom_runtimes = db.list_custom_account_runtimes()?;
        let dynamic_providers = db.list_dynamic_providers()?;
        let provider_contracts = crate::provider_contracts::build_effective_contracts(
            &zen_free_models,
            &custom_runtimes,
            db.load_persisted_contracts()?,
        );
        let http_client = build_proxy_route_set(
            &config,
            &provider_contracts,
            &custom_runtimes,
            &dynamic_providers,
            &cpa_models,
        )?;
        Ok(Self {
            db: Mutex::new(db),
            config: Mutex::new(config),
            client_root_url_override,
            gateway_port_override: OnceLock::new(),
            settings_update: Mutex::new(()),
            settings_host_effects: tokio::sync::Mutex::new(()),
            // Use a per-runtime random epoch so a browser tab left open across a
            // process restart cannot accidentally match the new runtime's first
            // revision. The low 48 bits leave ample room for monotonic increments.
            settings_revision: AtomicU64::new(
                (uuid::Uuid::new_v4().as_u128() as u64) & 0x0000_FFFF_FFFF_FFFF,
            ),
            process_generation: (uuid::Uuid::new_v4().as_u128() as u64) & 0x0000_FFFF_FFFF_FFFF,
            credential_snapshot: RwLock::new(credential_snapshot),
            gateway: Mutex::new(None),
            gateway_lifecycle: tokio::sync::Mutex::new(()),
            dashboard_session_token: Mutex::new(uuid::Uuid::new_v4().simple().to_string()),
            dashboard_local_mode: AtomicBool::new(false),
            dashboard_public_listeners: AtomicU64::new(0),
            desktop: DesktopCapabilities::new(),
            dashboard_dir: Mutex::new(None),
            http_client: Mutex::new(Arc::new(http_client)),
            pricing: RwLock::new(Arc::new(pricing)),
            pricing_refresh: tokio::sync::Mutex::new(()),
            zen_free_models: RwLock::new(Arc::new(zen_free_models)),
            cpa_models: RwLock::new(Arc::new(cpa_models)),
            unpublished_public_models: RwLock::new(Arc::new(unpublished_public_models)),
            zen_free_models_refresh: tokio::sync::Mutex::new(()),
            provider_models_refresh: tokio::sync::Mutex::new(()),
            provider_usage_refresh: tokio::sync::Mutex::new(()),
            cpa_operations: tokio::sync::Mutex::new(()),
            cpa_runtime: crate::cpa_runtime::CpaRuntimeCapabilities::new(),
            provider_contracts: RwLock::new(Arc::new(provider_contracts)),
            dynamic_providers: RwLock::new(Arc::new(dynamic_providers)),
            routing: RoutingRuntime::new(),
            recovery: Arc::new(crate::gateway::recovery::RecoveryRuntime::default()),
            browser: crate::browser::BrowserRuntime::new(),
            usage_sync: crate::usage_sync::UsageSyncRuntime::new(),
            gateway_clock,
            data_dir,
            cipher,
        })
    }

    /// One wall+mono pair for a Gateway outer-fallback decision. Production
    /// clocks are `Utc::now` / `Instant::now`; tests inject sources at
    /// construction.
    pub(crate) fn sample_gateway_clock(
        &self,
    ) -> (chrono::DateTime<chrono::Utc>, std::time::Instant) {
        (self.gateway_clock.now_wall(), self.gateway_clock.now_mono())
    }

    pub fn config(&self) -> AppConfig {
        self.config.lock().clone()
    }

    pub fn settings_config(&self) -> AppConfig {
        let mut config = self.config();
        if let Some(client_root_url) = &self.client_root_url_override {
            config.client_root_url.clone_from(client_root_url);
        }
        if let Some(gateway_port) = self.gateway_port_override.get() {
            config.gateway_port = *gateway_port;
        }
        config
    }

    pub fn settings_revision(&self) -> u64 {
        self.settings_revision.load(Ordering::Acquire)
    }

    pub fn process_generation(&self) -> u64 {
        self.process_generation
    }

    /// Advances the settings revision for mutations that bypass
    /// `set_config` (the sub key lifecycle API), keeping the shared
    /// optimistic-lock scheme meaningful across every writer.
    pub fn bump_settings_revision(&self) -> u64 {
        self.settings_revision.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn client_root_url_from_env(&self) -> bool {
        self.client_root_url_override.is_some()
    }

    /// Registers the desktop Host's immutable runtime port override before the
    /// listener starts. CLI and other hosts keep using the persisted port.
    pub fn register_gateway_port_override(&self, port: u16) -> crate::Result<()> {
        if port == 0 {
            return Err(anyhow::anyhow!("Gateway port must be between 1 and 65535"));
        }
        self.gateway_port_override
            .set(port)
            .map_err(|_| anyhow::anyhow!("Gateway port override is already registered"))
    }

    pub fn gateway_port_from_env(&self) -> bool {
        self.gateway_port_override.get().is_some()
    }

    pub fn upstream_context(&self) -> (AppConfig, reqwest::Client) {
        let config = self.config.lock();
        let client = self.http_client.lock();
        (config.clone(), client.default_client().clone())
    }

    /// Clones the whole route set as one snapshot: routing metadata and both
    /// leg clients come from the same `set_config` generation, so in-flight
    /// requests fly on internally consistent routing even across hot config
    /// switches.
    pub(crate) fn forward_route_set(&self) -> Arc<crate::http_client::ForwardRouteSet> {
        self.http_client.lock().clone()
    }

    pub fn pricing_snapshot(&self) -> Arc<PricingSnapshot> {
        self.pricing.read().clone()
    }

    pub fn zen_free_model_catalog(&self) -> Arc<crate::kernel::zen::ZenFreeModelCatalog> {
        self.zen_free_models.read().clone()
    }

    pub fn cpa_model_catalog(&self) -> Arc<Vec<String>> {
        self.cpa_models.read().clone()
    }

    pub fn unpublished_public_models(&self) -> Arc<HashSet<String>> {
        self.unpublished_public_models.read().clone()
    }

    pub fn unpublished_public_model_list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.unpublished_public_models().iter().cloned().collect();
        names.sort();
        names
    }

    /// Hide or restore one public name on `GET /v1/models`. Routing is unchanged.
    /// `public_model` must already be a normalized key.
    pub fn set_public_model_published(
        &self,
        public_model: &str,
        published: bool,
    ) -> crate::Result<Vec<String>> {
        let db = self.db.lock();
        if published {
            db.remove_unpublished_public_model(public_model)?;
        } else {
            db.upsert_unpublished_public_model(public_model)?;
        }
        let unpublished = db.list_unpublished_public_models()?;
        *self.unpublished_public_models.write() = Arc::new(unpublished.iter().cloned().collect());
        Ok(unpublished)
    }

    pub fn activate_cpa_model_catalog(
        &self,
        models: Vec<crate::db::CpaCatalogModel>,
        source_url: &str,
        refreshed_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::Result<()> {
        let ids = crate::db::CpaCatalogModel::enabled_ids(&models);
        let custom = self.db.lock().list_custom_account_runtimes()?;
        let route_set = build_proxy_route_set(
            &self.config(),
            &self.provider_contracts(),
            &custom,
            &self.dynamic_providers(),
            &ids,
        )?;
        {
            let db = self.db.lock();
            let mut http_client = self.http_client.lock();
            let mut active = self.cpa_models.write();
            db.replace_cpa_model_catalog(&models, source_url, refreshed_at)?;
            *http_client = Arc::new(route_set);
            *active = Arc::new(ids);
        }
        self.routing.reset();
        Ok(())
    }

    /// Replace the routed CPA catalog subset. Unknown IDs are rejected; an
    /// empty selection publishes no CPA models.
    pub fn set_cpa_model_routing(&self, enabled_ids: &[String]) -> crate::Result<()> {
        let catalog = {
            let db = self.db.lock();
            db.cpa_model_catalog()?
        };
        let Some(catalog) = catalog else {
            anyhow::bail!("CPA model catalog has not been refreshed");
        };
        let known: std::collections::HashSet<&str> = catalog
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        let enabled_ids: Vec<String> = enabled_ids.iter().map(|id| id.trim().to_string()).collect();
        for id in &enabled_ids {
            anyhow::ensure!(
                known.contains(id.as_str()),
                "enabledIds must be models from the saved CPA catalog"
            );
        }
        let selected: std::collections::HashSet<&str> =
            enabled_ids.iter().map(String::as_str).collect();
        let models = catalog
            .models
            .into_iter()
            .map(|mut model| {
                model.enabled = selected.contains(model.id.as_str());
                model
            })
            .collect();
        self.activate_cpa_model_catalog(
            models,
            &catalog.source_url,
            catalog.refreshed_at.unwrap_or_else(chrono::Utc::now),
        )
    }

    /// Atomically remove OCG-owned CPA configuration, singleton account, and
    /// catalog snapshot. CPA auth files and OAuth state remain external.
    pub fn disconnect_cpa_integration(&self) -> crate::Result<()> {
        let custom = self.db.lock().list_custom_account_runtimes()?;
        let route_set = build_proxy_route_set(
            &self.config(),
            &self.provider_contracts(),
            &custom,
            &self.dynamic_providers(),
            &[],
        )?;
        {
            let db = self.db.lock();
            let mut http_client = self.http_client.lock();
            let mut active = self.cpa_models.write();
            db.delete_cpa_integration()?;
            *http_client = Arc::new(route_set);
            *active = Arc::new(Vec::new());
        }
        self.routing.reset();
        Ok(())
    }

    pub fn activate_zen_free_model_catalog(
        &self,
        catalog: crate::kernel::zen::ZenFreeModelCatalog,
    ) -> crate::Result<()> {
        let previous_models = self
            .provider_contracts()
            .scope(&crate::provider_contracts::ContractScope::provider(
                crate::kernel::ids::OPENCODE_ZEN_FREE_PROVIDER_ID,
            ))
            .map(|contract| contract.catalog.models.clone())
            .unwrap_or_default();
        let dynamics = self.dynamic_providers();
        let cpa_models = self.cpa_model_catalog();
        let config = self.config();
        let (new_contracts, route_set) = {
            let db = self.db.lock();
            db.set_zen_free_model_catalog_with_default_off(&catalog, &previous_models)?;
            let custom = db.list_custom_account_runtimes()?;
            let persisted = db.load_persisted_contracts()?;
            let new_contracts =
                crate::provider_contracts::build_effective_contracts(&catalog, &custom, persisted);
            let route_set =
                build_proxy_route_set(&config, &new_contracts, &custom, &dynamics, &cpa_models)?;
            (new_contracts, route_set)
        };
        {
            let mut http_client = self.http_client.lock();
            let mut active = self.zen_free_models.write();
            let mut contracts = self.provider_contracts.write();
            *http_client = Arc::new(route_set);
            *active = Arc::new(catalog);
            *contracts = Arc::new(new_contracts);
        }
        self.routing.reset();
        Ok(())
    }

    pub fn dynamic_providers(&self) -> Arc<Vec<crate::dynamic::DynamicProviderRuntime>> {
        self.dynamic_providers.read().clone()
    }

    pub fn reload_dynamic_providers(&self) -> crate::Result<()> {
        let db = self.db.lock();
        self.reload_dynamic_providers_locked(&db)
    }

    pub fn reload_dynamic_providers_locked(&self, db: &Database) -> crate::Result<()> {
        let loaded = db.list_dynamic_providers()?;
        let custom = db.list_custom_account_runtimes()?;
        let route_set = build_proxy_route_set(
            &self.config(),
            &self.provider_contracts(),
            &custom,
            &loaded,
            &self.cpa_model_catalog(),
        )?;
        *self.http_client.lock() = Arc::new(route_set);
        *self.dynamic_providers.write() = Arc::new(loaded);
        Ok(())
    }

    pub fn provider_contracts(&self) -> Arc<crate::provider_contracts::EffectiveContractSet> {
        self.provider_contracts.read().clone()
    }

    pub fn reload_provider_contracts(&self) -> crate::Result<()> {
        let db = self.db.lock();
        self.reload_provider_contracts_locked(&db)
    }

    /// Build every fallible runtime snapshot from an uncommitted V2 node
    /// migration. The caller must hold the database transaction open while
    /// passing the same connection view here.
    pub(crate) fn prepare_imported_node_runtime(
        &self,
        db: &Database,
    ) -> crate::Result<ImportedNodeRuntime> {
        let (config, needs_persist) = load_config(db)?;
        config.validate().map_err(anyhow::Error::msg)?;
        // Sanitized config JSON can differ in field order and legacy defaults
        // can normalize on load. The typed V2 payload was validated before the
        // transaction, so semantic normalization is enough for this preflight;
        // startup may rewrite the byte representation later.
        let _ = needs_persist;
        let zen = db.zen_free_model_catalog()?.unwrap_or_default();
        let custom = db.list_custom_account_runtimes()?;
        let contracts = crate::provider_contracts::build_effective_contracts(
            &zen,
            &custom,
            db.load_persisted_contracts()?,
        );
        let dynamic_providers = db.list_dynamic_providers()?;
        let cpa_models = self.cpa_model_catalog();
        let route_set = build_proxy_route_set(
            &config,
            &contracts,
            &custom,
            &dynamic_providers,
            &cpa_models,
        )?;
        let credentials = crate::gateway_keys::build_credential_snapshot(db, &config.gateway_key)?;
        Ok(ImportedNodeRuntime {
            config,
            http_client: route_set,
            zen_free_models: zen,
            provider_contracts: contracts,
            dynamic_providers,
            credentials,
        })
    }

    /// Install a runtime snapshot whose fallible construction completed before
    /// the matching database transaction committed.
    pub(crate) fn install_imported_node_runtime(&self, runtime: ImportedNodeRuntime) {
        *self.config.lock() = runtime.config;
        *self.http_client.lock() = Arc::new(runtime.http_client);
        *self.zen_free_models.write() = Arc::new(runtime.zen_free_models);
        *self.provider_contracts.write() = Arc::new(runtime.provider_contracts);
        *self.dynamic_providers.write() = Arc::new(runtime.dynamic_providers);
        self.routing.reset();
        *self.credential_snapshot.write() = runtime.credentials;
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
    }

    /// Install a dynamic Provider snapshot. The fallible route set is built
    /// from the incoming snapshot before the in-memory assignment.
    pub(crate) fn install_dynamic_providers_snapshot(
        &self,
        providers: Vec<crate::dynamic::DynamicProviderRuntime>,
    ) -> crate::Result<()> {
        let custom = self.db.lock().list_custom_account_runtimes()?;
        let route_set = build_proxy_route_set(
            &self.config(),
            &self.provider_contracts(),
            &custom,
            &providers,
            &self.cpa_model_catalog(),
        )?;
        *self.http_client.lock() = Arc::new(route_set);
        *self.dynamic_providers.write() = Arc::new(providers);
        self.routing.reset();
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// A committed catalog removal must restrict admission even if unrelated
    /// persisted data prevents a full reload. Caller holds settings_update.
    pub(crate) fn restrict_provider_catalog_after_reload_failure(
        &self,
        row: &crate::provider_contracts::PersistedScopeRow,
    ) {
        {
            let mut active = self.provider_contracts.write();
            let set = Arc::make_mut(&mut active);
            if let Some(contract) = set.providers.get_mut(row.scope.id()) {
                contract
                    .models
                    .retain(|_, model| row.catalog_models.contains(&model.model_id));
                contract.catalog.models.clone_from(&row.catalog_models);
                contract.revision = row.revision;
            }
        }

        // The durable removal must also make a stale proxy-list membership
        // inert. Prefer a full candidate rebuild from the now-restricted
        // contract snapshot. If the same corrupt persistence that broke the
        // reload also prevents reading Custom runtimes, install the
        // conservative empty-known-model route set: whitelist falls back to
        // direct and blacklist falls back to proxy for every stale entry.
        let config = self.config();
        let rebuilt = self
            .db
            .lock()
            .list_custom_account_runtimes()
            .and_then(|custom| {
                build_proxy_route_set(
                    &config,
                    &self.provider_contracts(),
                    &custom,
                    &self.dynamic_providers(),
                    &self.cpa_model_catalog(),
                )
            })
            .or_else(|_| crate::http_client::build_route_set_from_known_models(&config, &[]));
        if let Ok(route_set) = rebuilt {
            *self.http_client.lock() = Arc::new(route_set);
        }
    }

    pub fn reload_provider_contracts_locked(&self, db: &Database) -> crate::Result<()> {
        let zen = self.zen_free_model_catalog();
        let custom = db.list_custom_account_runtimes()?;
        let set = crate::provider_contracts::build_effective_contracts(
            &zen,
            &custom,
            db.load_persisted_contracts()?,
        );
        let route_set = build_proxy_route_set(
            &self.config(),
            &set,
            &custom,
            &self.dynamic_providers(),
            &self.cpa_model_catalog(),
        )?;
        *self.http_client.lock() = Arc::new(route_set);
        *self.provider_contracts.write() = Arc::new(set);
        Ok(())
    }

    pub fn activate_pricing_snapshot(&self, snapshot: PricingSnapshot) -> crate::Result<()> {
        // Keep database persistence and the in-memory active pointer behind the
        // documented db -> pricing lock order, so readers never observe a
        // partially activated revision.
        let db = self.db.lock();
        let mut active = self.pricing.write();
        db.insert_pricing_snapshot(&snapshot)?;
        *active = Arc::new(snapshot);
        Ok(())
    }

    pub fn estimate_cost(
        &self,
        model: &str,
        prompt: i64,
        completion: i64,
        cached: i64,
        cache_creation: i64,
        service_tier: Option<&str>,
    ) -> PricingEstimate {
        self.pricing_snapshot().estimate(
            model,
            prompt,
            completion,
            cached,
            cache_creation,
            service_tier,
        )
    }

    pub fn active_gateway_port(&self) -> u16 {
        let configured = self.settings_config().gateway_port;
        self.gateway
            .lock()
            .as_ref()
            .map(|handle| handle.port)
            .unwrap_or(configured)
    }

    pub(crate) async fn lock_gateway_lifecycle(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.gateway_lifecycle.lock().await
    }

    pub(crate) fn register_dashboard_public_listener(&self) {
        self.dashboard_public_listeners
            .fetch_add(1, Ordering::AcqRel);
        self.set_dashboard_local_mode(false);
    }

    /// Removes one live public-listener registration and reports whether it
    /// was the last one. The listener lifecycle uses the transition to zero
    /// to schedule a serialized dashboard-trust recomputation; doing that
    /// work directly from the registration guard's `Drop` would require an
    /// async lock and can deadlock a listener shutdown.
    pub(crate) fn unregister_dashboard_public_listener(&self) -> bool {
        let previous = self
            .dashboard_public_listeners
            .fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "public listener count must not underflow");
        previous == 1
    }

    pub(crate) fn has_dashboard_public_listener(&self) -> bool {
        self.dashboard_public_listeners.load(Ordering::Acquire) != 0
    }

    /// Lifecycle evidence for integration tests that need to await an exact
    /// public registration transition without probing the TCP accept backlog
    /// and perturbing graceful shutdown. This doc-hidden observation is
    /// available in every profile because integration tests link the library
    /// without `cfg(test)` and release verification disables debug assertions.
    #[doc(hidden)]
    pub fn dashboard_public_listener_count(&self) -> u64 {
        self.dashboard_public_listeners.load(Ordering::Acquire)
    }

    pub fn set_dashboard_local_mode(&self, local: bool) {
        self.dashboard_local_mode.store(local, Ordering::Release);
    }

    pub fn dashboard_local_mode(&self) -> bool {
        self.dashboard_local_mode.load(Ordering::Acquire)
    }

    pub fn set_auto_start_sync(&self, sync: AutoStartSync) {
        self.desktop.set_auto_start_sync(sync);
    }

    pub fn auto_start_supported(&self) -> bool {
        self.desktop.auto_start_supported()
    }

    pub fn sync_auto_start(&self, enabled: bool) -> crate::Result<()> {
        self.desktop.sync_auto_start(enabled)
    }

    pub fn set_dock_visibility_sync(&self, sync: DockVisibilitySync) {
        self.desktop.set_dock_visibility_sync(sync);
    }

    pub fn dock_visibility_supported(&self) -> bool {
        self.desktop.dock_visibility_supported()
    }

    pub fn sync_dock_visibility(&self, visible: bool) -> crate::Result<()> {
        self.desktop.sync_dock_visibility(visible)
    }

    pub fn set_desktop_update_starter(&self, starter: DesktopUpdateStarter) {
        self.desktop.set_desktop_update_starter(starter);
    }

    pub fn desktop_update_supported(&self) -> bool {
        self.desktop.desktop_update_supported()
    }

    pub fn set_dsh_application_host(&self, host: crate::dsh_application::DshApplicationHost) {
        self.desktop.set_dsh_application_host(host);
    }

    pub fn dsh_application_host(&self) -> Option<crate::dsh_application::DshApplicationHost> {
        self.desktop.dsh_application_host()
    }

    pub fn desktop_update_status(&self) -> DesktopUpdateStatus {
        self.desktop.desktop_update_status()
    }

    pub fn start_desktop_update(
        &self,
        expected_version: String,
    ) -> Result<(), DesktopUpdateStartError> {
        self.desktop.start_desktop_update(expected_version)
    }

    pub fn set_desktop_update_progress(&self, downloaded: u64, total: Option<u64>) -> bool {
        self.desktop.set_desktop_update_progress(downloaded, total)
    }

    pub fn set_desktop_update_installing(&self) -> bool {
        self.desktop.set_desktop_update_installing()
    }

    pub fn set_desktop_update_failed(&self, error: impl Into<String>) {
        self.desktop.set_desktop_update_failed(error);
    }

    pub fn set_desktop_update_idle(&self) {
        self.desktop.set_desktop_update_idle();
    }

    fn prepare_config(
        &self,
        mut config: AppConfig,
    ) -> crate::Result<(AppConfig, crate::http_client::ForwardRouteSet)> {
        if self.client_root_url_override.is_some() || self.gateway_port_override.get().is_some() {
            let persisted = self.config.lock();
            if self.client_root_url_override.is_some() {
                config
                    .client_root_url
                    .clone_from(&persisted.client_root_url);
            }
            if self.gateway_port_override.get().is_some() {
                config.gateway_port = persisted.gateway_port;
            }
        }
        config.opencode_invite_url = normalize_opencode_invite_url(&config.opencode_invite_url)
            .map_err(anyhow::Error::msg)?;
        config.proxy_url = normalize_proxy_url(config.proxy_mode, &config.proxy_url)
            .map_err(anyhow::Error::msg)?;
        // validate() enforces the non-blank primary key on every write path.
        config.validate().map_err(anyhow::Error::msg)?;
        let custom = self.db.lock().list_custom_account_runtimes()?;
        let http_client = build_proxy_route_set(
            &config,
            &self.provider_contracts(),
            &custom,
            &self.dynamic_providers(),
            &self.cpa_model_catalog(),
        )?;
        Ok((config, http_client))
    }

    pub fn set_config(&self, config: AppConfig) -> crate::Result<()> {
        let (config, http_client) = self.prepare_config(config)?;
        let config_json = serde_json::to_string(&config)?;
        {
            let db = self.db.lock();
            db.set_config(&config_json)?;
        }
        self.apply_persisted_config(config, http_client);
        Ok(())
    }

    /// Persists `next`, then reasserts every supported auto-start / Dock hook.
    ///
    /// Callers must hold `settings_update` and finish protocol-specific
    /// validation/CAS first. Unsupported capability deltas fail before
    /// persistence. After a successful `set_config`, every supported hook is
    /// invoked with the persisted values even when those fields did not
    /// change. Hook failure rolls the config back, then best-effort restores
    /// both host hooks.
    pub fn apply_host_settings(
        &self,
        previous: &AppConfig,
        next: AppConfig,
    ) -> Result<(), HostSettingsError> {
        let next_auto_start = next.auto_start;
        let next_show_dock_icon = next.show_dock_icon;
        let auto_start_supported = self.auto_start_supported();
        let dock_visibility_supported = self.dock_visibility_supported();
        if !auto_start_supported && next_auto_start != previous.auto_start {
            return Err(HostSettingsError::AutoStartUnsupported);
        }
        if !dock_visibility_supported && next_show_dock_icon != previous.show_dock_icon {
            return Err(HostSettingsError::DockVisibilityUnsupported);
        }

        self.set_config(next).map_err(HostSettingsError::Persist)?;
        let runtime_sync = (|| -> crate::Result<()> {
            if auto_start_supported {
                self.sync_auto_start(next_auto_start)?;
            }
            if dock_visibility_supported {
                self.sync_dock_visibility(next_show_dock_icon)?;
            }
            Ok(())
        })();
        if let Err(sync_error) = runtime_sync {
            let config_rollback_error = self.set_config(previous.clone()).err();
            let auto_start_rollback_error = auto_start_supported
                .then(|| self.sync_auto_start(previous.auto_start).err())
                .flatten();
            let dock_rollback_error = dock_visibility_supported
                .then(|| self.sync_dock_visibility(previous.show_dock_icon).err())
                .flatten();
            let mut message = format!("failed to synchronize desktop settings: {sync_error}");
            if let Some(error) = config_rollback_error {
                message.push_str(&format!("; failed to restore settings: {error}"));
            }
            if let Some(error) = auto_start_rollback_error {
                message.push_str(&format!("; failed to restore auto-start state: {error}"));
            }
            if let Some(error) = dock_rollback_error {
                message.push_str(&format!("; failed to restore Dock visibility: {error}"));
            }
            return Err(HostSettingsError::Sync(message));
        }
        Ok(())
    }

    pub(crate) async fn lock_settings_host_effects(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.settings_host_effects.lock().await
    }

    /// Persist `next`, then rebind a running listener and conditionally
    /// compensate. Serializes the full settings host-effect transaction.
    /// Callers must not hold `settings_update` across this await.
    pub async fn apply_host_settings_and_rebind_listener(
        self: &Arc<Self>,
        previous: AppConfig,
        next: AppConfig,
        wait_for_previous: bool,
    ) -> Result<(), HostSettingsError> {
        let _effects = self.lock_settings_host_effects().await;
        let committed_revision = {
            let _settings_update = self.settings_update.lock();
            self.apply_host_settings(&previous, next.clone())?;
            self.settings_revision()
        };
        self.rebind_listener_after_settings_commit(
            previous,
            next,
            committed_revision,
            wait_for_previous,
        )
        .await
    }

    /// Listener follow-up for a settings persist. Callers must already hold
    /// `settings_host_effects` and must not hold `settings_update`. A failed
    /// rebind restores only the previous Gateway port when the live port is
    /// still the failed committed port. Other live AppConfig fields may have
    /// been updated by independent Key or Claude writers while the bind was
    /// pending and must be preserved.
    pub(crate) async fn rebind_listener_after_settings_commit(
        self: &Arc<Self>,
        previous: AppConfig,
        committed: AppConfig,
        committed_revision: u64,
        wait_for_previous: bool,
    ) -> Result<(), HostSettingsError> {
        if let Err(error) = self
            .rebind_gateway_listener_if_port_changed(
                previous.gateway_port,
                committed.gateway_port,
                wait_for_previous,
            )
            .await
        {
            if let Err(rollback_error) =
                self.compensate_failed_listener_rebind(&committed, previous, committed_revision)
            {
                return Err(HostSettingsError::GatewayBind(anyhow::anyhow!(
                    "{error}; failed to restore the configured Gateway port: {rollback_error}"
                )));
            }
            return Err(error);
        }
        Ok(())
    }

    /// Restore only the previous port after a failed listener rebind, but only
    /// while the live port still equals the failed committed port. The restore
    /// clones the live config so later Key, Claude mapping, and other AppConfig
    /// writes survive. A later port commit is authoritative and skips restore.
    /// Persistence failure is returned to the caller instead of being hidden
    /// behind the original bind error.
    pub fn compensate_failed_listener_rebind(
        &self,
        committed: &AppConfig,
        previous: AppConfig,
        committed_revision: u64,
    ) -> Result<bool, HostSettingsError> {
        let _settings_update = self.settings_update.lock();
        let live_revision = self.settings_revision();
        let mut live = self.config();
        if live.gateway_port != committed.gateway_port {
            return Ok(false);
        }
        // The failed committed port is the transaction identity. Revision-only
        // changes and unrelated AppConfig writes must not skip compensation.
        debug_assert!(
            live_revision >= committed_revision,
            "settings revision must not move backwards during compensation"
        );
        live.gateway_port = previous.gateway_port;
        self.set_config(live).map_err(HostSettingsError::Persist)?;
        Ok(true)
    }

    /// Rebind a running listener onto `next_port` at the same listen IP.
    ///
    /// No-ops when the port is unchanged or no listener is installed in
    /// `gateway`. Listener-only: does not start, cancel, or duplicate the
    /// process-level usage worker. Callers must not hold `settings_update`
    /// across this await. HTTP settings handlers must pass
    /// `wait_for_previous = false` so they do not await the listener that is
    /// serving the current request.
    pub async fn rebind_gateway_listener_if_port_changed(
        self: &Arc<Self>,
        previous_port: u16,
        next_port: u16,
        wait_for_previous: bool,
    ) -> Result<(), HostSettingsError> {
        if previous_port == next_port {
            return Ok(());
        }
        let Some(listen_addr) = self
            .gateway
            .lock()
            .as_ref()
            .map(|handle| handle.listen_addr)
        else {
            return Ok(());
        };
        let target = SocketAddr::new(listen_addr.ip(), next_port);
        let rebind = if wait_for_previous {
            GatewayRebindHost::rebind(self, target).await
        } else {
            GatewayRebindHost::rebind_from_serving_request(self, target).await
        };
        rebind.map(|_| ()).map_err(HostSettingsError::GatewayBind)
    }

    fn apply_persisted_config(
        &self,
        config: AppConfig,
        http_client: crate::http_client::ForwardRouteSet,
    ) {
        let should_reset_routing = {
            let mut current_config = self.config.lock();
            let mut current_client = self.http_client.lock();
            // Sticky routing resets when the routing fields change or the
            // primary key value rotates (its previous value stops
            // authenticating). Sub key revocations reset explicitly from
            // their endpoints; renaming or adding keys never clears live
            // sessions.
            let should_reset = current_config.routing_mode != config.routing_mode
                || current_config.conversation_sticky != config.conversation_sticky
                || current_config.gateway_key != config.gateway_key;
            *current_config = config.clone();
            *current_client = Arc::new(http_client);
            should_reset
        };
        // Refresh the primary entry in the credential snapshot so auth stops
        // accepting the old value immediately. Cross-tier uniqueness (API
        // gates) guarantees no other snapshot entry holds the new value; the
        // warn-only check below is defense in depth for future unchecked
        // writers.
        //
        // Ordering note: persistence precedes the snapshot swap on purpose.
        // A failed save returns before any mutation (consistent state); the
        // only gap is a panic between the in-memory swap above and this
        // snapshot block, transiently leaving the database and in-memory
        // config on the new value while the snapshot still authenticates the
        // old one — it heals on restart or at the next key API entry point
        // (both rebuild the snapshot from the database). Swapping the
        // snapshot first would instead leave an unpersisted credential
        // authenticating after a failed save — a divergence that outlives
        // the process.
        {
            let mut snapshot = self.credential_snapshot.write();
            if let Some(existing) = snapshot.get(&config.gateway_key)
                && existing.id != crate::gateway_keys::PRIMARY_KEY_ID
            {
                eprintln!(
                    "warning: primary key value collides with sub key `{}`; \
                         the API-layer gate should have rejected this write",
                    existing.name
                );
            }
            let stale_value = snapshot
                .iter()
                .find(|(_, entry)| entry.id == crate::gateway_keys::PRIMARY_KEY_ID)
                .map(|(value, _)| value.clone());
            if let Some(value) = stale_value {
                snapshot.remove(&value);
            }
            snapshot.insert(
                config.gateway_key.clone(),
                crate::gateway_keys::CredentialEntry {
                    id: crate::gateway_keys::PRIMARY_KEY_ID.to_string(),
                    name: crate::gateway_keys::PRIMARY_KEY_NAME.to_string(),
                },
            );
        }
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        if should_reset_routing {
            self.routing.reset();
        }
    }

    /// Resolves an authenticating credential by presented value; used by the
    /// auth hot path without touching the config or db locks.
    pub fn credential_entry_for_value(
        &self,
        value: &str,
    ) -> Option<crate::gateway_keys::CredentialEntry> {
        self.credential_snapshot.read().get(value).cloned()
    }

    /// Write-time name snapshot for a credential id (primary resolves to the
    /// fixed "Primary"); serves forward log attribution without a db lookup.
    pub fn client_key_name(&self, id: &str) -> Option<String> {
        self.credential_snapshot
            .read()
            .values()
            .find(|entry| entry.id == id)
            .map(|entry| entry.name.clone())
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    /// Persist one low-frequency lifecycle/control-plane event without making
    /// observability a prerequisite for the operation that already succeeded.
    /// Callers must pass an already-sanitized message: never include Keys,
    /// request bodies, authorization headers, or credential-bearing URLs.
    pub fn log_runtime_event(&self, level: &str, category: &str, message: &str) {
        if let Err(error) = self.db.lock().log_gateway(level, category, message) {
            eprintln!("warning: failed to persist runtime event category={category}: {error}");
        }
    }

    pub fn recover_browser_profiles_for_account(
        &self,
        account_id: &str,
    ) -> crate::Result<crate::browser::BrowserProfileRecoveryReport> {
        let db = self.db.lock();
        let account_exists = db.get_account(account_id)?.is_some();
        let report = crate::browser::recover_staged_browser_profiles_for_account(
            &self.data_dir,
            account_id,
            account_exists,
        );
        drop(db);
        report
    }

    pub fn set_dashboard_dir(&self, dir: Option<PathBuf>) {
        *self.dashboard_dir.lock() = dir;
    }

    pub fn dashboard_dir(&self) -> Option<PathBuf> {
        self.dashboard_dir.lock().clone()
    }

    pub fn encrypt_key(&self, plaintext: &str) -> crate::Result<String> {
        self.cipher.encrypt(plaintext)
    }

    pub fn decrypt_key(&self, ciphertext: &str) -> crate::Result<String> {
        self.cipher.decrypt(ciphertext)
    }
}

impl crate::gateway_keys::KeyStore for Database {
    fn list_active_sub_gateway_keys(&self) -> anyhow::Result<Vec<crate::models::SubGatewayKey>> {
        Database::list_active_sub_gateway_keys(self)
    }
    fn get_sub_gateway_key(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<crate::models::SubGatewayKey>> {
        Database::get_sub_gateway_key(self, id)
    }
    fn count_active_sub_gateway_keys(&self) -> anyhow::Result<usize> {
        Database::count_active_sub_gateway_keys(self)
    }
    fn insert_sub_gateway_key(&self, key: &crate::models::SubGatewayKey) -> anyhow::Result<()> {
        Database::insert_sub_gateway_key(self, key)
    }
    fn rename_sub_gateway_key(&self, id: &str, name: &str) -> anyhow::Result<bool> {
        Database::rename_sub_gateway_key(self, id, name)
    }
    fn set_sub_gateway_key_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<bool> {
        Database::set_sub_gateway_key_enabled(self, id, enabled)
    }
    fn update_sub_gateway_key_value(&self, id: &str, new_value: &str) -> anyhow::Result<bool> {
        Database::update_sub_gateway_key_value(self, id, new_value)
    }
    fn soft_delete_sub_gateway_key(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<bool> {
        Database::soft_delete_sub_gateway_key(self, id, now)
    }
    fn active_sub_gateway_key_values(&self) -> anyhow::Result<Vec<String>> {
        Database::active_sub_gateway_key_values(self)
    }
    fn sub_gateway_key_value_exists(&self, value: &str) -> anyhow::Result<bool> {
        Database::sub_gateway_key_value_exists(self, value)
    }
    fn random_word(&self) -> String {
        random_word()
    }
}

impl crate::gateway_keys::KeyStore for CoreStateInner {
    fn list_active_sub_gateway_keys(&self) -> anyhow::Result<Vec<crate::models::SubGatewayKey>> {
        Database::list_active_sub_gateway_keys(&self.db.lock())
    }
    fn get_sub_gateway_key(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<crate::models::SubGatewayKey>> {
        Database::get_sub_gateway_key(&self.db.lock(), id)
    }
    fn count_active_sub_gateway_keys(&self) -> anyhow::Result<usize> {
        Database::count_active_sub_gateway_keys(&self.db.lock())
    }
    fn insert_sub_gateway_key(&self, key: &crate::models::SubGatewayKey) -> anyhow::Result<()> {
        Database::insert_sub_gateway_key(&self.db.lock(), key)
    }
    fn rename_sub_gateway_key(&self, id: &str, name: &str) -> anyhow::Result<bool> {
        Database::rename_sub_gateway_key(&self.db.lock(), id, name)
    }
    fn set_sub_gateway_key_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<bool> {
        Database::set_sub_gateway_key_enabled(&self.db.lock(), id, enabled)
    }
    fn update_sub_gateway_key_value(&self, id: &str, new_value: &str) -> anyhow::Result<bool> {
        Database::update_sub_gateway_key_value(&self.db.lock(), id, new_value)
    }
    fn soft_delete_sub_gateway_key(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<bool> {
        Database::soft_delete_sub_gateway_key(&self.db.lock(), id, now)
    }
    fn active_sub_gateway_key_values(&self) -> anyhow::Result<Vec<String>> {
        Database::active_sub_gateway_key_values(&self.db.lock())
    }
    fn sub_gateway_key_value_exists(&self, value: &str) -> anyhow::Result<bool> {
        Database::sub_gateway_key_value_exists(&self.db.lock(), value)
    }
    fn random_word(&self) -> String {
        random_word()
    }
}

impl crate::gateway_keys::KeyHost for CoreStateInner {
    fn primary_gateway_key(&self) -> String {
        self.config.lock().gateway_key.clone()
    }
    fn clone_credential_snapshot(&self) -> crate::gateway_keys::CredentialSnapshot {
        self.credential_snapshot.read().clone()
    }
    fn replace_credential_snapshot(&self, snapshot: crate::gateway_keys::CredentialSnapshot) {
        *self.credential_snapshot.write() = snapshot;
    }
    fn with_credential_snapshot_mut<R>(
        &self,
        f: impl FnOnce(&mut crate::gateway_keys::CredentialSnapshot) -> R,
    ) -> R {
        f(&mut self.credential_snapshot.write())
    }
    fn load_unique_value_inputs(
        &self,
    ) -> anyhow::Result<(Vec<String>, crate::gateway_keys::CredentialSnapshot)> {
        let db = self.db.lock();
        let stored = Database::active_sub_gateway_key_values(&db)?;
        let snapshot = self.credential_snapshot.read().clone();
        Ok((stored, snapshot))
    }
    fn load_snapshot_rebuild_inputs(
        &self,
    ) -> anyhow::Result<(Vec<crate::models::SubGatewayKey>, String)> {
        let db = self.db.lock();
        let keys = Database::list_active_sub_gateway_keys(&db)?;
        let primary = Database::primary_access_key_value(&db)?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| self.config.lock().gateway_key.clone());
        Ok((keys, primary))
    }
}

impl crate::account_control::AccountControlHost for CoreStateInner {
    fn with_settings_update<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = self.settings_update.lock();
        f()
    }

    fn encrypt_key(&self, plaintext: &str) -> anyhow::Result<String> {
        CoreStateInner::encrypt_key(self, plaintext)
    }

    fn bump_settings_revision(&self) -> u64 {
        CoreStateInner::bump_settings_revision(self)
    }

    fn settings_revision(&self) -> u64 {
        CoreStateInner::settings_revision(self)
    }

    fn process_generation(&self) -> u64 {
        CoreStateInner::process_generation(self)
    }

    fn recover_browser_profiles_for_account(&self, account_id: &str) -> anyhow::Result<()> {
        CoreStateInner::recover_browser_profiles_for_account(self, account_id).map(|_| ())
    }

    fn data_dir(&self) -> PathBuf {
        CoreStateInner::data_dir(self)
    }

    fn reload_provider_contracts(&self) -> anyhow::Result<()> {
        CoreStateInner::reload_provider_contracts(self)
    }

    fn ensure_provider_can_enable(
        &self,
        provider_id: &str,
    ) -> Result<(), crate::provider::ProviderBindingError> {
        match crate::provider::ensure_provider_can_enable(provider_id) {
            Ok(()) => Ok(()),
            Err(crate::provider::ProviderBindingError::UnknownProvider { .. }) => {
                if crate::dynamic::find_runtime(&self.dynamic_providers(), provider_id).is_some() {
                    Ok(())
                } else {
                    Err(crate::provider::ProviderBindingError::UnknownProvider {
                        provider_id: provider_id.to_string(),
                    })
                }
            }
            Err(error) => Err(error),
        }
    }

    fn create_account_with_contract(&self, account: &crate::models::Account) -> anyhow::Result<()> {
        self.db
            .lock()
            .create_account_with_contract(account, None, &[])
    }

    fn update_account(
        &self,
        id: &str,
        update: &crate::models::AccountUpdate,
    ) -> anyhow::Result<()> {
        self.db.lock().update_account(id, update, None, None)
    }

    fn get_account(&self, id: &str) -> anyhow::Result<Option<crate::models::Account>> {
        Database::get_account(&self.db.lock(), id)
    }

    fn account_verification_status(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Option<crate::provider::ConnectionVerificationStatus>> {
        Ok(self
            .db
            .lock()
            .account_verification_state(account_id)?
            .map(|state| state.status))
    }

    fn delete_account_row(&self, id: &str) -> anyhow::Result<()> {
        self.db.lock().delete_account(id)
    }

    fn log_gateway(&self, level: &str, category: &str, message: &str) -> anyhow::Result<()> {
        Database::log_gateway(&self.db.lock(), level, category, message)
    }

    fn stop_browser_account(
        &self,
        account_id: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        self.browser.stop_account(account_id)
    }
}

impl crate::usage_sync::UsageSyncStore for Database {
    fn list_accounts(&self) -> anyhow::Result<Vec<crate::models::Account>> {
        Database::list_accounts(self)
    }
    fn get_account(&self, account_id: &str) -> anyhow::Result<Option<crate::models::Account>> {
        Database::get_account(self, account_id)
    }
    fn account_usage_sync_state(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Option<crate::models::ProviderUsageSyncState>> {
        Database::account_usage_sync_state(self, account_id)
    }
    fn pull_account_usage_sync_next_eligible(
        &self,
        account_id: &str,
        proposal: chrono::DateTime<chrono::Utc>,
        respect_failure_backoff: bool,
    ) -> anyhow::Result<()> {
        Database::pull_account_usage_sync_next_eligible(
            self,
            account_id,
            proposal,
            respect_failure_backoff,
        )
    }
    fn account_has_local_activity_since(
        &self,
        account_id: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<bool> {
        Database::account_has_local_activity_since(self, account_id, since)
    }
    fn account_usage_with_limits(
        &self,
        account_id: &str,
        limits: &crate::kernel::pricing::PricingLimits,
    ) -> anyhow::Result<crate::models::UsageWindow> {
        Database::account_usage_with_limits(self, account_id, limits)
    }
    fn commit_official_usage_sync_success(
        &self,
        account_id: &str,
        expected_key_cipher: &str,
        snapshot: &crate::go_usage::GoUsageSnapshot,
        limits: &crate::kernel::pricing::PricingLimits,
        metadata: crate::usage_sync::OfficialUsageSyncSuccessMetadata,
    ) -> anyhow::Result<Option<crate::models::UsageWindow>> {
        Database::commit_official_usage_sync_success(
            self,
            account_id,
            expected_key_cipher,
            &crate::db::AccountUsageCalibrationSnapshot {
                rolling_percent: snapshot.rolling_percent,
                weekly_percent: snapshot.weekly_percent,
                monthly_percent: snapshot.monthly_percent,
                rolling_resets_in_minutes: snapshot.rolling_resets_in_minutes,
                weekly_resets_in_minutes: snapshot.weekly_resets_in_minutes,
            },
            limits,
            crate::db::AccountUsageSyncSuccessMetadata {
                now: metadata.now,
                next_eligible_at: metadata.next_eligible_at,
                mark_expedited: metadata.mark_expedited,
            },
        )
    }
    fn record_account_usage_sync_failure(
        &self,
        account_id: &str,
        now: chrono::DateTime<chrono::Utc>,
        failure_streak: i64,
        next_eligible_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<()> {
        Database::record_account_usage_sync_failure(
            self,
            account_id,
            now,
            failure_streak,
            next_eligible_at,
        )
    }
    fn log_gateway(&self, level: &str, category: &str, message: &str) -> anyhow::Result<()> {
        Database::log_gateway(self, level, category, message)
    }
}

impl crate::usage_sync::UsageSyncHost for CoreState {
    type Weak = std::sync::Weak<CoreStateInner>;
    type Store = Database;

    fn downgrade(&self) -> Self::Weak {
        Arc::downgrade(self)
    }
    fn upgrade(weak: &Self::Weak) -> Option<Self> {
        weak.upgrade()
    }
    fn usage_runtime(&self) -> &crate::usage_sync::UsageSyncRuntime {
        &self.usage_sync
    }
    fn pricing_limits(&self) -> crate::kernel::pricing::PricingLimits {
        self.pricing_snapshot().limits.clone()
    }
    fn config(&self) -> AppConfig {
        CoreStateInner::config(self)
    }
    fn decrypt_account_key(&self, ciphertext: &str) -> anyhow::Result<String> {
        self.decrypt_key(ciphertext)
    }
    fn with_sync_store<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Self::Store) -> R,
    {
        let db = self.db.lock();
        f(&db)
    }

    fn with_authorized_sync_store<F, R>(
        &self,
        authorization: &crate::usage_sync::UsageSyncCommitAuthorization,
        f: F,
    ) -> Result<R, crate::usage_sync::UsageSyncCommitAuthorizationRejected>
    where
        F: FnOnce(&Self::Store) -> R,
    {
        match authorization {
            crate::usage_sync::UsageSyncCommitAuthorization::Unconditional => {
                Ok(self.with_sync_store(f))
            }
            crate::usage_sync::UsageSyncCommitAuthorization::ControlRevision {
                expected_revision,
                process_generation,
            } => {
                // Lock order remains settings_update -> db. This synchronous
                // reservation is acquired only after outbound work completes
                // and is released before the coordinator awaits again.
                let _settings_update = self.settings_update.lock();
                if *expected_revision != self.settings_revision()
                    || *process_generation != self.process_generation()
                {
                    return Err(crate::usage_sync::UsageSyncCommitAuthorizationRejected);
                }
                let db = self.db.lock();
                Ok(f(&db))
            }
        }
    }
}

fn client_root_url_override_from_env() -> crate::Result<Option<String>> {
    match std::env::var(CLIENT_ROOT_URL_ENV) {
        Ok(value) => normalize_client_root_url_override(Some(&value))
            .map_err(|error| anyhow::anyhow!("{CLIENT_ROOT_URL_ENV}: {error}")),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(anyhow::anyhow!(
            "{CLIENT_ROOT_URL_ENV} must contain valid Unicode"
        )),
    }
}

fn normalize_client_root_url_override(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = normalize_client_root_url(value)?;
    Ok((!value.is_empty()).then_some(value))
}

/// Loads persisted config. The `bool` marks config that needs canonical rewriting.
fn load_config(db: &Database) -> crate::Result<(AppConfig, bool)> {
    let mut config = AppConfig::default();
    let mut stored_gateway_key = String::new();
    let mut needs_persist = if let Some(value) = db.get_setting("config")? {
        config = serde_json::from_str(&value)?;
        stored_gateway_key = config.gateway_key.clone();
        let mut compare = config.clone();
        compare.gateway_key = stored_gateway_key.clone();
        serde_json::to_string(&compare)? != value
    } else {
        true
    };
    if let Some(primary) = db.primary_access_key_value()? {
        config.gateway_key = primary;
    }
    if !stored_gateway_key.trim().is_empty() {
        // Sanitized config JSON is no longer the database authority for the
        // primary key; rewrite leftover plaintext out of settings.
        needs_persist = true;
    }
    let invite_url =
        normalize_opencode_invite_url(&config.opencode_invite_url).map_err(anyhow::Error::msg)?;
    if invite_url != config.opencode_invite_url {
        config.opencode_invite_url = invite_url;
        needs_persist = true;
    }
    let proxy_url =
        normalize_proxy_url(config.proxy_mode, &config.proxy_url).map_err(anyhow::Error::msg)?;
    if proxy_url != config.proxy_url {
        config.proxy_url = proxy_url;
        needs_persist = true;
    }
    // v1.4.2 shipped 30/120/300 as one default tuple. Migrate that exact,
    // untouched tuple once while preserving every user-customized combination.
    if (
        config.connect_timeout_secs,
        config.non_stream_timeout_secs,
        config.stream_idle_timeout_secs,
    ) == (30, 120, 300)
    {
        config.non_stream_timeout_secs = 900;
        needs_persist = true;
    }
    if config.gateway_key.trim().is_empty() {
        // Mint before validate: a fresh, pre-multi-key, or whitespace-corrupt
        // config always ends up with a usable primary key (validate rejects
        // blank-after-trim values, so the guard here must be trim-aware).
        config.gateway_key = generate_gateway_key();
        needs_persist = true;
    }
    Ok((config, needs_persist))
}

fn save_config(db: &Database, config: &AppConfig) -> crate::Result<()> {
    db.set_config(&serde_json::to_string(config)?)?;
    Ok(())
}

fn generate_gateway_key() -> String {
    format!("ocg-{}-{}", random_word(), random_word())
}

pub fn random_word() -> String {
    // Use UUID v4 for proper randomness (122 bits entropy)
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

#[cfg(test)]
mod tests;
