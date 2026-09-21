//! A directory is cold only while no returned Database holds its shared lock.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;

pub(super) struct DatabaseOpenGuard {
    // On failed initialization, release the lifetime lock before the open gate
    // so the next opener can acquire EX and recover abandoned receipts.
    file: File,
    gate: Option<File>,
    exclusive: bool,
}

impl DatabaseOpenGuard {
    pub(super) fn acquire(data_dir: &Path) -> Result<Self> {
        // Serialize initialization, including failed opens and the EX -> SH
        // handoff. Returned databases do not retain this gate.
        // Never unlink or replace either file: openers must lock the same inodes.
        let gate = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join(".database-open-gate.lock"))
            .context("open database initialization gate")?;
        FileExt::lock_exclusive(&gate).context("acquire database initialization gate")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join(".database-open.lock"))
            .context("open database lifetime lock")?;
        let exclusive = match FileExt::try_lock_exclusive(&file) {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                FileExt::lock_shared(&file).context("acquire shared database lifetime lock")?;
                false
            }
            Err(error) => return Err(error).context("acquire exclusive database lifetime lock"),
        };
        Ok(Self {
            file,
            gate: Some(gate),
            exclusive,
        })
    }

    pub(super) fn can_recover_pending(&self) -> bool {
        self.exclusive
    }

    pub(super) fn finish_open(&mut self) -> Result<()> {
        if self.exclusive {
            // Windows does not support implicit EX -> SH conversion. Retain the
            // open gate until SH is held, including if either operation fails.
            FileExt::unlock(&self.file).context("release exclusive database lifetime lock")?;
            self.exclusive = false;
            FileExt::lock_shared(&self.file).context("acquire shared database lifetime lock")?;
        }
        drop(self.gate.take());
        Ok(())
    }
}

#[cfg(test)]
mod tests;
