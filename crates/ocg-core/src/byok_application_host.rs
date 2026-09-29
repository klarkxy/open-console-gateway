//! Native-host implementation of BYOK application configuration.
//!
//! Four fixed adapters write OCG-owned provider entries. Inspection never
//! mutates files. Receipts, backups, and journals live under the Host data
//! directory and are never serialized onto the wire.

use crate::byok_application::{
    ByokClient, ByokError, ByokErrorKind, ByokHostRequest, ByokInspection, ByokModel, ByokResult,
    ByokStatus,
};
use crate::dsh_application_host::{absolute_host_path, is_link_or_reparse, user_home};
use crate::runtime_log::Level;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

mod adapters;
mod fs;
mod lock;
mod paths;
mod receipt;

use adapters::{adapter, validate_models};
use fs::{canonical_lexical_path, read_regular_file};
use lock::{CrossProcessLock, LockPolicy};
use paths::{DiscoveredPaths, ResolvedTarget, catalog_path, parent_is_safe};
use receipt::{PendingKind, Receipt, Store, fingerprint, has_backup};

#[cfg(test)]
#[path = "byok_application_host/tests.rs"]
mod tests;

pub fn register(core: &crate::state::CoreState) {
    let user_home = absolute_host_path(user_home());
    let data_dir = absolute_host_path(core.data_dir());
    let host = Arc::new(ByokNativeHost::new(
        data_dir,
        DiscoveredPaths::from_env(user_home),
        LockPolicy::default(),
    ));
    core.set_byok_application_host(Arc::new(move |request| host.execute(request)));
}

/// Where operational host events go. Production writes process stderr; tests
/// record instead so the emitted events can be asserted without a pipe.
#[derive(Clone, Default)]
enum HostConsole {
    #[default]
    Stderr,
    #[cfg(test)]
    Recording(Arc<Mutex<Vec<(Level, String)>>>),
}

struct ByokNativeHost {
    data_dir: PathBuf,
    paths: DiscoveredPaths,
    lock_policy: LockPolicy,
    operations: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    console: HostConsole,
    #[cfg(test)]
    fail_after_writes: Mutex<Option<usize>>,
}

impl ByokNativeHost {
    fn new(data_dir: PathBuf, paths: DiscoveredPaths, lock_policy: LockPolicy) -> Self {
        Self {
            data_dir,
            paths,
            lock_policy,
            operations: Mutex::new(HashMap::new()),
            console: HostConsole::default(),
            #[cfg(test)]
            fail_after_writes: Mutex::new(None),
        }
    }

    /// Emit one operational line. Messages name the client and the file only:
    /// Keys, request bodies, and credential-bearing URLs never reach here.
    fn report(&self, level: Level, message: impl std::fmt::Display) {
        match &self.console {
            HostConsole::Stderr => crate::runtime_log::console(level, message),
            #[cfg(test)]
            HostConsole::Recording(lines) => {
                if let Ok(mut lines) = lines.lock() {
                    lines.push((level, message.to_string()));
                }
            }
        }
    }

    /// Record why a mutation ended. Only the error kind is logged; the message
    /// itself stays with the response so nothing unsanitized is written out.
    fn report_outcome(
        &self,
        operation: &str,
        client: ByokClient,
        target: &ResolvedTarget,
        result: &ByokResult<ByokInspection>,
    ) {
        let at = target.path.display();
        match result {
            Ok(_) => self.report(
                Level::Info,
                format!("{operation} finished for {} at {at}", client.id()),
            ),
            Err(error) => self.report(
                Level::Warn,
                format!(
                    "{operation} refused for {} at {at}: {}",
                    client.id(),
                    error_kind_label(&error.kind)
                ),
            ),
        }
    }

    fn report_external_change(&self, client: ByokClient, target: &ResolvedTarget) {
        self.report(
            Level::Warn,
            format!(
                "{} at {} was changed outside OCG; the owned fields were not overwritten",
                client.id(),
                target.path.display()
            ),
        );
    }

    fn execute(&self, request: ByokHostRequest) -> ByokResult<ByokInspection> {
        match request {
            ByokHostRequest::Inspect {
                client,
                target_path,
            } => self.with_target_lock(client, target_path.as_deref(), |target| {
                self.inspect(client, target)
            }),
            ByokHostRequest::Configure {
                client,
                target_path,
                expected_fingerprint,
                gateway_v1_url,
                secret,
                models,
                default_model_id,
                client_closed,
            } => {
                let secret_text = secret.expose_to_host().to_string();
                self.mutate(
                    client,
                    target_path.as_deref(),
                    &secret_text,
                    "configure",
                    |target| {
                        self.configure(
                            client,
                            target,
                            &expected_fingerprint,
                            &gateway_v1_url,
                            &secret_text,
                            &models,
                            default_model_id.as_deref(),
                            client_closed,
                        )
                    },
                )
            }
            ByokHostRequest::Remove {
                client,
                target_path,
                expected_fingerprint,
                client_closed,
            } => self.mutate(client, target_path.as_deref(), "", "remove", |target| {
                self.remove(client, target, &expected_fingerprint, client_closed)
            }),
            ByokHostRequest::Recover {
                client,
                target_path,
                expected_fingerprint,
                client_closed,
            } => self.mutate(client, target_path.as_deref(), "", "recover", |target| {
                self.recover(client, target, &expected_fingerprint, client_closed)
            }),
        }
    }

    fn with_target_lock<T>(
        &self,
        client: ByokClient,
        target_path: Option<&str>,
        op: impl FnOnce(&ResolvedTarget) -> ByokResult<T>,
    ) -> ByokResult<T> {
        let target = self.paths.resolve(client, target_path)?;
        let key = target.path.to_string_lossy().into_owned();
        let lock = {
            let mut map = self
                .operations
                .lock()
                .map_err(|_| ByokError::internal("BYOK operation lock is poisoned"))?;
            map.entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = match lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(ByokError::internal("BYOK operation lock is poisoned"));
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                self.report(
                    Level::Info,
                    format!(
                        "waiting for another OCG operation on {}",
                        target.path.display()
                    ),
                );
                lock.lock()
                    .map_err(|_| ByokError::internal("BYOK operation lock is poisoned"))?
            }
        };
        op(&target)
    }

    fn mutate(
        &self,
        client: ByokClient,
        target_path: Option<&str>,
        secret: &str,
        operation: &str,
        op: impl FnOnce(&ResolvedTarget) -> ByokResult<ByokInspection>,
    ) -> ByokResult<ByokInspection> {
        self.with_target_lock(client, target_path, |target| {
            let result = sanitize(op(target), secret);
            self.report_outcome(operation, client, target, &result);
            result
        })
    }

    fn inspect(&self, client: ByokClient, target: &ResolvedTarget) -> ByokResult<ByokInspection> {
        let target = normalize_target(target)?;
        let store = Store::open(&self.data_dir, &target)?;
        let receipt = load_receipt(&store, &target)?;
        let journal = store.journal_bytes()?;
        let target_bytes = read_optional(&target.path)?;
        let catalog = catalog_path(&target);
        let catalog_bytes = match &catalog {
            Some(path) => read_optional(path)?,
            None => None,
        };
        Ok(self.view(
            client,
            &target,
            &store,
            receipt.as_ref(),
            target_bytes.as_deref(),
            catalog_bytes.as_deref(),
            journal.as_deref(),
            false,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn configure(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        expected_fingerprint: &str,
        gateway_v1_url: &str,
        secret: &str,
        models: &[ByokModel],
        default_model_id: Option<&str>,
        client_closed: bool,
    ) -> ByokResult<ByokInspection> {
        require_closed(client, client_closed)?;
        validate_models(client, models)?;
        if let Some(id) = default_model_id
            && !models.iter().any(|model| model.id == id)
        {
            return Err(ByokError::invalid(
                "defaultModelId must be one of the selected models",
            ));
        }
        if gateway_v1_url.trim().is_empty() {
            return Err(ByokError::invalid("gatewayV1Url is required"));
        }
        let target = normalize_target(target)?;
        prepare_lock_parent(&target)?;
        let _cross = CrossProcessLock::acquire(client, &target.path, &self.lock_policy)?;
        self.require_fingerprint(client, &target, expected_fingerprint)?;
        let store = Store::open(&self.data_dir, &target)?;
        #[cfg(test)]
        if let (Ok(mut injected), Ok(mut slot)) = (
            self.fail_after_writes.lock(),
            store.fail_after_writes.lock(),
        ) {
            *slot = injected.take();
        }
        let receipt = load_receipt(&store, &target)?;
        if receipt.as_ref().is_some_and(|r| r.pending.is_some()) || store.load_journal()?.is_some()
        {
            self.report(
                Level::Warn,
                format!(
                    "{} at {} has an interrupted write; recover it before writing again",
                    client.id(),
                    target.path.display()
                ),
            );
            return Err(ByokError::precondition(
                "Recover the interrupted write before configuring",
            ));
        }
        let target_bytes = read_optional(&target.path)?;
        let catalog = catalog_path(&target);
        let catalog_bytes = match &catalog {
            Some(path) => read_optional(path)?,
            None => None,
        };
        let parsed = adapter(client).inspect_bytes(
            target_bytes.as_deref(),
            catalog_bytes.as_deref(),
            receipt.as_ref(),
        );
        if let Some(detail) = parsed.incompatible {
            return Err(ByokError::invalid(detail));
        }
        if parsed.collision {
            self.report(
                Level::Warn,
                format!(
                    "{} at {} already holds a configuration named ocg that OCG does not own",
                    client.id(),
                    target.path.display()
                ),
            );
            return Err(ByokError::conflict(
                "An unowned ocg provider already exists in this configuration",
            ));
        }
        if parsed.user_changed_owned {
            self.report_external_change(client, &target);
            return Err(ByokError::conflict(
                "Owned fields changed outside OCG; the configuration was not overwritten",
            ));
        }
        let default_model_id = default_model_id.or_else(|| {
            parsed
                .current_default
                .as_deref()
                .filter(|id| models.iter().any(|model| model.id == *id))
                .or_else(|| models.first().map(|model| model.id.as_str()))
        });
        let plan = adapter(client).configure(
            &target.path,
            catalog.as_deref(),
            target_bytes.as_deref(),
            catalog_bytes.as_deref(),
            receipt.as_ref(),
            adapters::ConfigureInput {
                gateway_v1_url,
                secret,
                models,
                default_model_id,
            },
        )?;
        _cross.assert_held()?;
        store.apply(&target, receipt, plan, PendingKind::Configure)?;
        self.inspect(client, &target).map(|mut view| {
            view.activation_required = true;
            view
        })
    }

    fn remove(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        expected_fingerprint: &str,
        client_closed: bool,
    ) -> ByokResult<ByokInspection> {
        require_closed(client, client_closed)?;
        let target = normalize_target(target)?;
        prepare_lock_parent(&target)?;
        let _cross = CrossProcessLock::acquire(client, &target.path, &self.lock_policy)?;
        self.require_fingerprint(client, &target, expected_fingerprint)?;
        let store = Store::open(&self.data_dir, &target)?;
        let receipt = load_receipt(&store, &target)?
            .ok_or_else(|| ByokError::precondition("No OCG-owned configuration to remove"))?;
        if receipt.pending.is_some() || store.load_journal()?.is_some() {
            self.report(
                Level::Warn,
                format!(
                    "{} at {} has an interrupted write; recover it before removing",
                    client.id(),
                    target.path.display()
                ),
            );
            return Err(ByokError::precondition(
                "Recover the interrupted write before removing",
            ));
        }
        let target_bytes = read_optional(&target.path)?;
        let catalog = catalog_path(&target);
        let catalog_bytes = match &catalog {
            Some(path) => read_optional(path)?,
            None => None,
        };
        let parsed = adapter(client).inspect_bytes(
            target_bytes.as_deref(),
            catalog_bytes.as_deref(),
            Some(&receipt),
        );
        if parsed.user_changed_owned {
            self.report_external_change(client, &target);
            return Err(ByokError::conflict(
                "Owned fields changed outside OCG; the configuration was not overwritten",
            ));
        }
        let plan = adapter(client).remove(
            &target.path,
            catalog.as_deref(),
            target_bytes.as_deref(),
            catalog_bytes.as_deref(),
            &receipt,
        )?;
        _cross.assert_held()?;
        store.apply(&target, Some(receipt), plan, PendingKind::Remove)?;
        self.inspect(client, &target).map(|mut view| {
            view.activation_required = true;
            view
        })
    }

    fn recover(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        expected_fingerprint: &str,
        client_closed: bool,
    ) -> ByokResult<ByokInspection> {
        require_closed(client, client_closed)?;
        let target = normalize_target(target)?;
        prepare_lock_parent(&target)?;
        let _cross = CrossProcessLock::acquire(client, &target.path, &self.lock_policy)?;
        self.require_fingerprint(client, &target, expected_fingerprint)?;
        let store = Store::open(&self.data_dir, &target)?;
        let Some(journal) = store.load_journal()? else {
            return Err(ByokError::precondition(
                "No interrupted BYOK write to recover",
            ));
        };
        _cross.assert_held()?;
        store.recover_journal(&target, &journal)?;
        self.report(
            Level::Info,
            format!(
                "rolled back an interrupted {} write at {}",
                client.id(),
                target.path.display()
            ),
        );
        self.inspect(client, &target).map(|mut view| {
            view.activation_required = true;
            view
        })
    }

    fn require_fingerprint(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        expected: &str,
    ) -> ByokResult<()> {
        let store = Store::open(&self.data_dir, target)?;
        let receipt = load_receipt(&store, target)?;
        let target_bytes = read_optional(&target.path)?;
        let catalog_bytes = match catalog_path(target) {
            Some(path) => read_optional(&path)?,
            None => None,
        };
        let journal = store.journal_bytes()?;
        let actual = fingerprint(
            target,
            target_bytes.as_deref(),
            catalog_bytes.as_deref(),
            receipt.as_ref(),
            journal.as_deref(),
        );
        if actual != expected {
            let _ = client;
            self.report(
                Level::Warn,
                format!(
                    "{} at {} changed after it was read; the operation was abandoned",
                    client.id(),
                    target.path.display()
                ),
            );
            return Err(ByokError::conflict(
                "Configuration changed; refresh and retry",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn view(
        &self,
        client: ByokClient,
        target: &ResolvedTarget,
        store: &Store,
        receipt: Option<&Receipt>,
        target_bytes: Option<&[u8]>,
        catalog_bytes: Option<&[u8]>,
        journal_bytes: Option<&[u8]>,
        activation_required: bool,
    ) -> ByokInspection {
        let parsed = adapter(client).inspect_bytes(target_bytes, catalog_bytes, receipt);
        let pending = journal_bytes.is_some() || receipt.is_some_and(|r| r.pending.is_some());
        let detected = target_bytes.is_some();
        let (status, detail, configure_supported, remove_supported, recovery_supported) = if pending
        {
            (
                ByokStatus::RecoveryRequired,
                Some("An interrupted write can be recovered".into()),
                false,
                false,
                true,
            )
        } else if let Some(detail) = parsed.incompatible.clone() {
            (ByokStatus::Incompatible, Some(detail), false, false, false)
        } else if parsed.collision || parsed.user_changed_owned {
            (
                ByokStatus::Conflict,
                Some("An ocg entry exists that OCG does not own or no longer matches".into()),
                false,
                false,
                false,
            )
        } else if receipt.is_some() && !parsed.configured_model_ids.is_empty() {
            (
                ByokStatus::Configured,
                Some("OCG provider entries are present".into()),
                true,
                true,
                false,
            )
        } else if detected {
            (
                ByokStatus::Ready,
                Some("Configuration file is present and can be updated".into()),
                true,
                receipt.is_some(),
                false,
            )
        } else {
            (
                ByokStatus::NotDetected,
                Some("Configuration file is missing and can be created".into()),
                parent_is_safe(&target.path) || target.path.parent().is_some(),
                // A receipt with no file is the deleted-target case: removal
                // retires the receipt, so it stays available instead of leaving
                // the entry unmanageable.
                receipt.is_some(),
                false,
            )
        };
        let mut target_paths = vec![target.path.display().to_string()];
        if let Some(path) = catalog_path(target) {
            target_paths.push(path.display().to_string());
        }
        ByokInspection {
            client,
            status,
            detected,
            config_path: target.path.display().to_string(),
            discovery_source: target.discovery_source.clone(),
            target_paths,
            configure_supported,
            remove_supported,
            recovery_supported,
            requires_closed_client: client.requires_closed_client(),
            activation_required,
            fingerprint: Some(fingerprint(
                target,
                target_bytes,
                catalog_bytes,
                receipt,
                journal_bytes,
            )),
            configured_model_ids: parsed.configured_model_ids,
            default_model_id: parsed.current_default,
            backup_path: has_backup(store).then(|| store.backup_path_display()),
            detail,
        }
    }
}

fn normalize_target(target: &ResolvedTarget) -> ByokResult<ResolvedTarget> {
    let path = canonical_lexical_path(&target.path)?;
    if is_link_or_reparse(&path) {
        return Err(ByokError::conflict("BYOK target path is a link"));
    }
    Ok(ResolvedTarget {
        client: target.client,
        path,
        discovery_source: target.discovery_source.clone(),
    })
}

fn load_receipt(store: &Store, target: &ResolvedTarget) -> ByokResult<Option<Receipt>> {
    match store.load()? {
        None => Ok(None),
        Some(receipt) => {
            store.validate_identity(target, &receipt)?;
            Ok(Some(receipt))
        }
    }
}

fn read_optional(path: &Path) -> ByokResult<Option<Vec<u8>>> {
    read_regular_file(path)
}

fn prepare_lock_parent(target: &ResolvedTarget) -> ByokResult<()> {
    if let Some(parent) = target.path.parent() {
        fs::ensure_safe_directory_chain(parent)?;
    }
    Ok(())
}

fn require_closed(client: ByokClient, client_closed: bool) -> ByokResult<()> {
    if client.requires_closed_client() && !client_closed {
        return Err(ByokError::precondition(
            "Close the client before changing this configuration",
        ));
    }
    Ok(())
}

/// Operator-facing reason for a refused mutation. The error message stays
/// with the response; only this kind label reaches the console sink.
fn error_kind_label(kind: &ByokErrorKind) -> &'static str {
    match kind {
        ByokErrorKind::Invalid => "invalid request",
        ByokErrorKind::Precondition => "precondition not met",
        ByokErrorKind::Conflict => "conflict",
        ByokErrorKind::Internal => "internal error",
    }
}

fn sanitize(result: ByokResult<ByokInspection>, secret: &str) -> ByokResult<ByokInspection> {
    match result {
        Ok(value) => Ok(redact_inspection(value, secret)),
        Err(error) if !secret.is_empty() && error.message.contains(secret) => Err(ByokError {
            kind: error.kind,
            message: "BYOK operation failed".into(),
        }),
        Err(error) => Err(error),
    }
}

fn redact_inspection(mut view: ByokInspection, secret: &str) -> ByokInspection {
    if secret.is_empty() {
        return view;
    }
    if view
        .detail
        .as_ref()
        .is_some_and(|detail| detail.contains(secret))
    {
        view.detail = Some("BYOK operation completed".into());
    }
    view
}
