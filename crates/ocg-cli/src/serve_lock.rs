//! Exclusive lock for one `serve` process per data directory.
//!
//! `cli-listener.json` only projects this process's bound address. It is not
//! an ownership lock. This file is the lock. The CLI crate needs `fs2 = "0.4"`,
//! the same crate ocg-core already uses for its database lock. Do not unlink
//! this file: every serve must lock the same inode.

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = ".cli-serve.lock";

#[derive(Debug)]
pub struct ServeLock {
    _file: File,
}

impl ServeLock {
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("failed to create {}", data_dir.display()))?;
        let path = lock_path(data_dir);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Self { _file: file }),
            Err(error) if lock_is_contended(&error) => {
                bail!(
                    "another ocg serve process holds {}. Stop that process, or use api --endpoint against its listener. This command did not open the database.",
                    path.display()
                )
            }
            Err(error) => Err(error).context("acquire serve lock"),
        }
    }
}

pub fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

fn lock_is_contended(error: &std::io::Error) -> bool {
    error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

#[cfg(test)]
#[path = "serve_lock/tests.rs"]
mod tests;
