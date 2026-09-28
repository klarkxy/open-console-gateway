//! Private receipt, origin backup, operation journal, and fingerprint.
//!
//! Origin backups are the first-adoption snapshot and are never overwritten.
//! `old-*` / `new-*` files are per-operation rollback data. A journal records
//! the prior receipt (or its absence) so crash recovery restores ownership
//! together with file bytes.
use super::fs::{
    content_hash, ensure_safe_directory_chain, read_regular_file, remove_regular_file,
    write_private_atomic,
};
use super::paths::{ResolvedTarget, catalog_path};
use super::{ByokError, ByokResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub const RECEIPT_VERSION: u32 = 1;
const PROVIDER_ID: &str = "ocg";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    Target,
    Catalog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    Configure,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedSnapshot {
    pub provider_id: String,
    pub model_ids: Vec<String>,
    pub owned: serde_json::Value,
    pub applied_default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingFile {
    pub role: FileRole,
    pub old_hash: String,
    pub new_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOperation {
    pub kind: PendingKind,
    pub files: Vec<PendingFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub prior_receipt: Option<Receipt>,
    pub kind: PendingKind,
    pub files: Vec<PendingFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub version: u32,
    pub client: String,
    pub target_path: String,
    pub identity: String,
    pub created_target: bool,
    pub created_catalog: bool,
    pub baseline_default: Option<String>,
    pub last_applied_default: Option<String>,
    pub first_owned: serde_json::Value,
    pub last_managed: ManagedSnapshot,
    pub pending: Option<PendingOperation>,
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub role: FileRole,
    pub path: PathBuf,
    pub new_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct ApplyPlan {
    pub files: Vec<PlannedFile>,
    pub created_target: bool,
    pub created_catalog: bool,
    pub baseline_default: Option<String>,
    pub last_applied_default: Option<String>,
    pub managed: ManagedSnapshot,
    pub first_owned: serde_json::Value,
}

pub struct Store {
    pub dir: PathBuf,
    #[cfg(test)]
    pub fail_after_writes: std::sync::Mutex<Option<usize>>,
}

impl Store {
    pub fn open(data_dir: &Path, target: &ResolvedTarget) -> ByokResult<Self> {
        let hash = super::fs::sha256_hex(target.path.to_string_lossy().as_bytes());
        let dir = data_dir
            .join("applications")
            .join("byok")
            .join(target.client.id())
            .join(hash);
        Ok(Self {
            dir,
            #[cfg(test)]
            fail_after_writes: std::sync::Mutex::new(None),
        })
    }

    pub fn receipt_path(&self) -> PathBuf {
        self.dir.join("receipt.json")
    }

    pub fn journal_path(&self) -> PathBuf {
        self.dir.join("journal.json")
    }

    pub fn backup_dir(&self) -> PathBuf {
        self.dir.join("backup")
    }

    pub fn origin_dir(&self) -> PathBuf {
        self.dir.join("origin")
    }

    pub fn backup_path_display(&self) -> String {
        self.origin_dir().display().to_string()
    }

    pub fn load(&self) -> ByokResult<Option<Receipt>> {
        let Some(bytes) = read_regular_file(&self.receipt_path())? else {
            return Ok(None);
        };
        let receipt: Receipt = serde_json::from_slice(&bytes)
            .map_err(|_| ByokError::conflict("BYOK receipt is unreadable"))?;
        if receipt.version != RECEIPT_VERSION {
            return Err(ByokError::conflict("BYOK receipt version is unsupported"));
        }
        Ok(Some(receipt))
    }

    pub fn load_journal(&self) -> ByokResult<Option<Journal>> {
        let Some(bytes) = read_regular_file(&self.journal_path())? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| ByokError::conflict("BYOK journal is unreadable"))
    }

    pub fn journal_bytes(&self) -> ByokResult<Option<Vec<u8>>> {
        read_regular_file(&self.journal_path())
    }

    pub fn validate_identity(&self, target: &ResolvedTarget, receipt: &Receipt) -> ByokResult<()> {
        if receipt.client != target.client.id() {
            return Err(ByokError::conflict(
                "BYOK receipt client does not match the target",
            ));
        }
        if receipt.target_path != target.path.to_string_lossy() {
            return Err(ByokError::conflict(
                "BYOK receipt target does not match the path",
            ));
        }
        if receipt.identity.is_empty() {
            return Err(ByokError::conflict("BYOK receipt identity is missing"));
        }
        Ok(())
    }

    pub fn allowed_path(&self, target: &ResolvedTarget, role: FileRole) -> ByokResult<PathBuf> {
        match role {
            FileRole::Target => Ok(target.path.clone()),
            FileRole::Catalog => catalog_path(target)
                .ok_or_else(|| ByokError::invalid("This client has no catalog file")),
        }
    }

    fn role_backup(&self, role: FileRole, side: &str) -> PathBuf {
        self.backup_dir()
            .join(format!("{side}-{}.bin", role_name(role)))
    }

    pub fn save_receipt(&self, receipt: &Receipt) -> ByokResult<()> {
        ensure_safe_directory_chain(&self.dir)?;
        let bytes = serde_json::to_vec_pretty(receipt)
            .map_err(|_| ByokError::internal("failed to encode BYOK receipt"))?;
        write_private_atomic(&self.receipt_path(), &bytes)
    }

    fn save_journal(&self, journal: &Journal) -> ByokResult<()> {
        ensure_safe_directory_chain(&self.dir)?;
        let bytes = serde_json::to_vec_pretty(journal)
            .map_err(|_| ByokError::internal("failed to encode BYOK journal"))?;
        write_private_atomic(&self.journal_path(), &bytes)
    }

    fn clear_journal(&self) -> ByokResult<()> {
        remove_regular_file(&self.journal_path())
    }

    fn retire(&self) -> ByokResult<()> {
        remove_regular_file(&self.receipt_path())?;
        self.clear_journal()?;
        self.clear_origin()
    }

    fn clear_origin(&self) -> ByokResult<()> {
        let origin = self.origin_dir();
        for name in ["target.bin", "catalog.bin"] {
            remove_regular_file(&origin.join(name))?;
        }
        match fs::remove_dir(&origin) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Ok(()),
        }
    }

    fn restore_prior(&self, prior: Option<&Receipt>) -> ByokResult<()> {
        match prior {
            Some(receipt) => {
                let mut restored = receipt.clone();
                restored.pending = None;
                self.save_receipt(&restored)
            }
            None => remove_regular_file(&self.receipt_path()),
        }
    }

    fn write_hash_backup(
        &self,
        role: FileRole,
        side: &str,
        bytes: Option<&[u8]>,
    ) -> ByokResult<()> {
        let path = self.role_backup(role, side);
        match bytes {
            Some(bytes) => write_private_atomic(&path, bytes),
            None => remove_regular_file(&path),
        }
    }

    pub fn apply(
        &self,
        target: &ResolvedTarget,
        prior: Option<Receipt>,
        plan: ApplyPlan,
        kind: PendingKind,
    ) -> ByokResult<Option<Receipt>> {
        if let Some(receipt) = &prior {
            self.validate_identity(target, receipt)?;
        }
        ensure_safe_directory_chain(&self.backup_dir())?;
        let mut pending_files = Vec::new();
        for file in &plan.files {
            let allowed = self.allowed_path(target, file.role)?;
            if allowed != file.path {
                return Err(ByokError::conflict(
                    "BYOK write path is not derived from the current target",
                ));
            }
            let current = read_regular_file(&file.path)?;
            let old_hash = content_hash(current.as_deref());
            let new_hash = content_hash(file.new_bytes.as_deref());
            self.write_hash_backup(file.role, "old", current.as_deref())?;
            self.write_hash_backup(file.role, "new", file.new_bytes.as_deref())?;
            pending_files.push(PendingFile {
                role: file.role,
                old_hash,
                new_hash,
            });
        }
        let journal = Journal {
            prior_receipt: prior.clone(),
            kind,
            files: pending_files,
        };
        self.save_journal(&journal)?;

        if let Err(error) = self.write_plan(&plan) {
            if self.rollback_from_journal(target, &journal).is_ok()
                && self.restore_prior(prior.as_ref()).is_ok()
            {
                let _ = self.clear_journal();
            }
            return Err(error);
        }

        if kind == PendingKind::Remove {
            self.retire()?;
            return Ok(None);
        }

        let first_adoption = prior.is_none();
        let mut receipt = prior.unwrap_or_else(|| new_receipt(target));
        receipt.first_owned = plan.first_owned.clone();
        receipt.baseline_default = plan.baseline_default.clone();
        if first_adoption {
            receipt.created_target = plan.created_target;
            receipt.created_catalog = plan.created_catalog;
            self.save_origin_from_old_backups(&plan)?;
        }
        receipt.last_managed = plan.managed;
        receipt.last_applied_default = plan.last_applied_default;
        receipt.pending = None;
        self.save_receipt(&receipt)?;
        self.clear_journal()?;
        Ok(Some(receipt))
    }

    fn save_origin_from_old_backups(&self, plan: &ApplyPlan) -> ByokResult<()> {
        ensure_safe_directory_chain(&self.origin_dir())?;
        for file in &plan.files {
            let dest = self
                .origin_dir()
                .join(format!("{}.bin", role_name(file.role)));
            match read_regular_file(&self.role_backup(file.role, "old"))? {
                Some(bytes) => write_private_atomic(&dest, &bytes)?,
                None => remove_regular_file(&dest)?,
            }
        }
        Ok(())
    }

    fn write_plan(&self, plan: &ApplyPlan) -> ByokResult<()> {
        let fail_after = {
            #[cfg(test)]
            {
                self.fail_after_writes.lock().ok().and_then(|g| *g)
            }
            #[cfg(not(test))]
            {
                None::<usize>
            }
        };
        for (index, file) in plan.files.iter().enumerate() {
            if fail_after == Some(index) {
                return Err(ByokError::internal("injected write interruption"));
            }
            match &file.new_bytes {
                Some(bytes) => write_private_atomic(&file.path, bytes)?,
                None => remove_regular_file(&file.path)?,
            }
        }
        Ok(())
    }

    fn validate_prior_receipt(&self, target: &ResolvedTarget, journal: &Journal) -> ByokResult<()> {
        match &journal.prior_receipt {
            None => Ok(()),
            Some(receipt) => {
                if receipt.version != RECEIPT_VERSION {
                    return Err(ByokError::conflict(
                        "BYOK journal prior receipt version is unsupported",
                    ));
                }
                self.validate_identity(target, receipt)
            }
        }
    }

    fn rollback_from_journal(&self, target: &ResolvedTarget, journal: &Journal) -> ByokResult<()> {
        self.validate_prior_receipt(target, journal)?;
        self.validate_journal_states(target, journal)?;
        for entry in &journal.files {
            let path = self.allowed_path(target, entry.role)?;
            let current = read_regular_file(&path)?;
            let hash = content_hash(current.as_deref());
            if hash == entry.old_hash {
                continue;
            }
            self.restore_old(entry, &path)?;
        }
        Ok(())
    }

    fn validate_journal_states(
        &self,
        target: &ResolvedTarget,
        journal: &Journal,
    ) -> ByokResult<()> {
        for entry in &journal.files {
            let path = self.allowed_path(target, entry.role)?;
            let current = read_regular_file(&path)?;
            let hash = content_hash(current.as_deref());
            if hash != entry.old_hash && hash != entry.new_hash {
                return Err(ByokError::conflict(
                    "Current files changed after the interrupted write; recovery refused",
                ));
            }
            let old_backup = read_regular_file(&self.role_backup(entry.role, "old"))?;
            if content_hash(old_backup.as_deref()) != entry.old_hash {
                return Err(ByokError::conflict(
                    "BYOK backup no longer matches the journal",
                ));
            }
        }
        Ok(())
    }

    fn restore_old(&self, entry: &PendingFile, path: &Path) -> ByokResult<()> {
        if entry.old_hash == "absent" {
            return remove_regular_file(path);
        }
        let backup = read_regular_file(&self.role_backup(entry.role, "old"))?;
        let Some(bytes) = backup else {
            return Err(ByokError::conflict(
                "BYOK backup no longer matches the journal",
            ));
        };
        write_private_atomic(path, &bytes)
    }

    pub fn recover_journal(&self, target: &ResolvedTarget, journal: &Journal) -> ByokResult<()> {
        self.validate_prior_receipt(target, journal)?;
        self.rollback_from_journal(target, journal)?;
        self.restore_prior(journal.prior_receipt.as_ref())?;
        self.clear_journal()
    }
}

pub fn new_receipt(target: &ResolvedTarget) -> Receipt {
    Receipt {
        version: RECEIPT_VERSION,
        client: target.client.id().into(),
        target_path: target.path.to_string_lossy().into_owned(),
        identity: uuid::Uuid::new_v4().to_string(),
        created_target: false,
        created_catalog: false,
        baseline_default: None,
        last_applied_default: None,
        first_owned: serde_json::Value::Null,
        last_managed: ManagedSnapshot {
            provider_id: PROVIDER_ID.into(),
            model_ids: Vec::new(),
            owned: serde_json::Value::Null,
            applied_default: None,
        },
        pending: None,
    }
}

pub fn fingerprint(
    target: &ResolvedTarget,
    target_bytes: Option<&[u8]>,
    catalog_bytes: Option<&[u8]>,
    receipt: Option<&Receipt>,
    journal_bytes: Option<&[u8]>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(target.client.id().as_bytes());
    digest.update([0xff]);
    digest.update(target.path.to_string_lossy().as_bytes());
    digest.update([0xff]);
    digest.update(content_hash(target_bytes).as_bytes());
    digest.update([0xff]);
    digest.update(content_hash(catalog_bytes).as_bytes());
    digest.update([0xff]);
    if let Some(receipt) = receipt {
        digest.update(receipt.identity.as_bytes());
        digest.update([0xff]);
        digest.update(serde_json::to_vec(receipt).unwrap_or_default());
    } else {
        digest.update(b"none");
    }
    digest.update([0xff]);
    digest.update(content_hash(journal_bytes).as_bytes());
    hex::encode(digest.finalize())
}

fn role_name(role: FileRole) -> &'static str {
    match role {
        FileRole::Target => "target",
        FileRole::Catalog => "catalog",
    }
}

pub fn has_backup(store: &Store) -> bool {
    store.origin_dir().is_dir()
}
