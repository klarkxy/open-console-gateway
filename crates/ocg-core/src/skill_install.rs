//! Synchronize the skill embedded in the desktop and CLI binaries.

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const SKILL_NAME: &str = "ocg-manager";
const MARKER: &str = ".ocg-managed";
const FILES: &[(&str, &str)] = &[
    (
        "SKILL.md",
        include_str!("../../../skills/ocg-manager/SKILL.md"),
    ),
    (
        "references/install.md",
        include_str!("../../../skills/ocg-manager/references/install.md"),
    ),
    (
        "references/configure.md",
        include_str!("../../../skills/ocg-manager/references/configure.md"),
    ),
    (
        "references/operations.md",
        include_str!("../../../skills/ocg-manager/references/operations.md"),
    ),
    (
        "references/secrets.md",
        include_str!("../../../skills/ocg-manager/references/secrets.md"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSyncStatus {
    Installed,
    Updated,
    UpToDate,
}

#[derive(Debug)]
pub struct SkillSyncResult {
    pub path: PathBuf,
    pub status: SkillSyncStatus,
}

pub fn sync_user_skill() -> Result<SkillSyncResult> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME")
    }
    .map(PathBuf::from)
    .context("cannot locate the user home for Codex skill installation")?;
    sync_at(&home)
}

fn sync_at(home: &Path) -> Result<SkillSyncResult> {
    if !home.is_absolute() {
        bail!("user home must be an absolute path");
    }
    let home = fs::canonicalize(home).context("resolving the user home")?;
    let home_metadata = fs::symlink_metadata(&home).context("checking the user home")?;
    if !home_metadata.is_dir() || is_reparse(&home_metadata) {
        bail!("user home is not a regular directory");
    }
    let agents = home.join(".agents");
    ensure_real_dir(&agents)?;
    let skills = agents.join("skills");
    ensure_real_dir(&skills)?;

    let lock_path = agents.join(".ocg-manager-skill.lock");
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.is_file() && !is_reparse(&metadata) => {}
        Ok(_) => bail!("OCG skill lock path is not a regular file"),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("checking OCG skill lock"),
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening OCG skill lock at {}", lock_path.display()))?;
    lock.try_lock_exclusive()
        .context("another OCG process may be synchronizing the skill")?;
    let result = sync_locked(&agents, &skills);
    drop(lock);
    result
}

fn sync_locked(agents: &Path, skills: &Path) -> Result<SkillSyncResult> {
    let destination = skills.join(SKILL_NAME);
    let previous = match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse(&metadata) {
                bail!("existing OCG skill path is not a regular directory");
            }
            let marker_path = destination.join(MARKER);
            let marker_metadata = fs::symlink_metadata(&marker_path)
                .context("existing ocg-manager skill is not OCG-managed; left unchanged")?;
            if !marker_metadata.is_file() || is_reparse(&marker_metadata) {
                bail!("existing ocg-manager skill has no regular OCG ownership marker");
            }
            let marker = fs::read_to_string(&marker_path)?;
            if !marker.starts_with("ocg-manager\n") {
                bail!("existing ocg-manager skill is not OCG-managed; left unchanged");
            }
            if marker == bundled_marker() {
                if installed_files_match(&destination)? {
                    return Ok(SkillSyncResult {
                        path: destination,
                        status: SkillSyncStatus::UpToDate,
                    });
                }
                bail!("current-version OCG skill has local edits; left unchanged");
            }
            true
        }
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("checking installed OCG skill"),
    };

    let stage = agents.join(format!(".ocg-manager-stage-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage).context("creating OCG skill staging directory")?;
    if let Err(error) = write_skill(&stage) {
        let _ = remove_real_dir(&stage);
        return Err(error).context("writing staged OCG skill");
    }
    if let Err(error) = verify_bundled_skill(&stage) {
        let _ = remove_real_dir(&stage);
        return Err(error).context("staged OCG skill verification failed");
    }

    let backup = if previous {
        let backups = agents.join("skill-backups");
        ensure_real_dir(&backups)?;
        let backup = backups.join(format!("ocg-manager-{}", uuid::Uuid::new_v4()));
        fs::rename(&destination, &backup).context("backing up the previous OCG-managed skill")?;
        Some(backup)
    } else {
        None
    };

    if let Err(error) = fs::rename(&stage, &destination) {
        let _ = remove_real_dir(&stage);
        if let Some(backup) = &backup {
            fs::rename(backup, &destination)
                .context("restoring OCG skill after installation failed")?;
        }
        return Err(error).context("installing the embedded OCG skill");
    }
    if let Err(error) = verify_bundled_skill(&destination) {
        remove_real_dir(&destination).context("removing failed OCG skill installation")?;
        if let Some(backup) = &backup {
            fs::rename(backup, &destination)
                .context("restoring previous OCG skill after verification failed")?;
        }
        return Err(error).context("installed OCG skill failed readback verification");
    }

    Ok(SkillSyncResult {
        path: destination,
        status: if previous {
            SkillSyncStatus::Updated
        } else {
            SkillSyncStatus::Installed
        },
    })
}

fn write_skill(target: &Path) -> Result<()> {
    for (relative, contents) in FILES {
        let path = target.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    fs::write(target.join(MARKER), bundled_marker())?;
    Ok(())
}

fn bundled_marker() -> String {
    let mut digest = Sha256::new();
    for (path, contents) in FILES {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(contents.as_bytes());
        digest.update([0]);
    }
    format!("ocg-manager\n{}\n", hex::encode(digest.finalize()))
}

fn verify_bundled_skill(target: &Path) -> Result<()> {
    if !installed_files_match(target)?
        || fs::read_to_string(target.join(MARKER))? != bundled_marker()
    {
        bail!("skill files differ from the embedded bundle");
    }
    Ok(())
}

fn installed_files_match(target: &Path) -> Result<bool> {
    let mut expected = BTreeSet::from([PathBuf::from(MARKER)]);
    for (relative, _) in FILES {
        let path = PathBuf::from(relative);
        expected.insert(path.clone());
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            expected.insert(parent.to_path_buf());
        }
    }
    let mut found = BTreeSet::new();
    read_paths(target, target, &mut found)?;
    if found != expected {
        return Ok(false);
    }
    for (relative, contents) in FILES {
        if fs::read(target.join(relative))? != contents.as_bytes() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_paths(root: &Path, current: &Path, found: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if is_reparse(&metadata) {
            bail!("OCG skill contains a symlink or reparse point");
        }
        found.insert(path.strip_prefix(root)?.to_path_buf());
        if metadata.is_dir() {
            read_paths(root, &path, found)?;
        } else if !metadata.is_file() {
            bail!("OCG skill contains an unsupported file type");
        }
    }
    Ok(())
}

fn ensure_real_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !is_reparse(&metadata) => Ok(()),
        Ok(_) => bail!(
            "OCG skill parent is not a regular directory: {}",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir(path)
            .with_context(|| format!("creating OCG skill directory {}", path.display())),
        Err(error) => Err(error).with_context(|| format!("checking {}", path.display())),
    }
}

fn remove_real_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        bail!("refusing to remove a non-directory or reparse point");
    }
    fs::remove_dir_all(path)?;
    Ok(())
}

#[cfg(windows)]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests;
