//! Database open preflight: schema peek, ciphertext probes, and backups.
//!
//! Database::open_internal owns connection order and calls these helpers.
//! Backup naming, cipher repair, and check sequencing stay unchanged.

use super::*;

pub(super) fn schema_version_on(conn: &Connection) -> Result<i32> {
    if !table_exists(conn, "schema_version")? {
        return Ok(0);
    }
    Ok(conn
        .query_row(
            "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0))
}

/// Reads the persisted schema version without opening through [`Database`]: no
/// migrations, no open guard, no cipher. Hosts probe with this before bringing
/// up their UI so a newer-than-supported database fails with an actionable
/// prompt instead of a setup error.
pub fn peek_schema_version(data_dir: &Path) -> Result<Option<i32>> {
    let db_path = data_dir.join("data.sqlite");
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open {} read-only", db_path.display()))?;
    Ok(Some(schema_version_on(&conn)?))
}

/// Reads the persisted AppConfig without opening through [`Database`], for
/// hosts that must act (e.g. check for updates) before the full database open
/// is allowed. Returns None when the file or setting is missing or unreadable.
pub fn peek_app_config(data_dir: &Path) -> Option<AppConfig> {
    let db_path = data_dir.join("data.sqlite");
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let json = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'config'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()??;
    serde_json::from_str(&json).ok()
}

pub(super) fn verify_schema_backup(path: &Path, prefix: &str, source_version: i32) -> Result<()> {
    let backup = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open {prefix} backup {}", path.display()))?;
    let version = schema_version_on(&backup)?;
    anyhow::ensure!(
        version == source_version,
        "refusing to reuse invalid {prefix} backup {} (expected schema version {source_version}, found {version})",
        path.display()
    );
    Ok(())
}

pub(super) fn ensure_schema_backup(
    conn: &Connection,
    db_path: &Path,
    prefix: &str,
    source_version: i32,
) -> Result<()> {
    let data_dir = db_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("database path has no parent: {}", db_path.display()))?;
    let mut existing_backups = std::fs::read_dir(data_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".bak"))
        })
        .collect::<Vec<_>>();
    existing_backups.sort();
    if let Some(valid) = existing_backups
        .iter()
        .rev()
        .find(|path| verify_schema_backup(path, prefix, source_version).is_ok())
    {
        return verify_schema_backup(valid, prefix, source_version);
    }

    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%9fZ");
    let backup_path = db_path.with_file_name(format!("{prefix}{timestamp}.bak"));

    // VACUUM INTO is SQLite's consistent online snapshot mechanism: unlike a
    // raw file copy it also includes committed pages still resident in WAL.
    // SQLite refuses to overwrite an existing target, preserving the first
    // rollback point across retries.
    let backup_value = backup_path.to_string_lossy().into_owned();
    match conn.execute("VACUUM main INTO ?1", [&backup_value]) {
        Ok(_) => verify_schema_backup(&backup_path, prefix, source_version),
        Err(error) if backup_path.exists() => {
            verify_schema_backup(&backup_path, prefix, source_version).with_context(|| {
                format!("{prefix} backup appeared concurrently after SQLite reported: {error}")
            })
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to create {prefix} database backup {}",
                backup_path.display()
            )
        }),
    }
}

pub(super) fn has_unversioned_legacy_tables(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "accounts")?
        || table_exists(conn, "settings")?
        || table_exists(conn, "forward_logs")?)
}

pub(super) fn ensure_pre_v22_backup(conn: &Connection, db_path: &Path) -> Result<()> {
    let source_version = schema_version_on(conn)?;
    let unversioned_legacy = source_version == 0 && has_unversioned_legacy_tables(conn)?;
    if !(1..22).contains(&source_version) && !unversioned_legacy {
        return Ok(());
    }
    ensure_schema_backup(conn, db_path, PRE_V22_BACKUP_FILE_PREFIX, source_version)
}

pub(super) fn ensure_pre_v23_backup(conn: &Connection, db_path: &Path) -> Result<()> {
    let source_version = schema_version_on(conn)?;
    let unversioned_legacy = source_version == 0 && has_unversioned_legacy_tables(conn)?;
    if !(1..23).contains(&source_version) && !unversioned_legacy {
        return Ok(());
    }
    ensure_schema_backup(conn, db_path, PRE_V23_BACKUP_FILE_PREFIX, source_version)
}

pub(super) fn is_fresh_empty_database(conn: &Connection, source_version: i32) -> Result<bool> {
    Ok(source_version == 0 && !has_unversioned_legacy_tables(conn)?)
}

pub(super) fn sqlite_data_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA data_version", [], |row| row.get(0))?)
}

pub(super) fn sqlite_quick_check(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA quick_check")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0].eq_ignore_ascii_case("ok"),
        "sqlite quick_check failed: {}",
        rows.join("; ")
    );
    Ok(())
}

pub(super) fn sqlite_foreign_key_check(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(rows.is_empty(), "sqlite foreign_key_check failed: {rows:?}");
    Ok(())
}

pub(super) fn probe_account_cipher(
    cipher: Option<&dyn KeyCipher>,
    id: &str,
    column: &str,
    value: &str,
) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let Some(cipher) = cipher else {
        anyhow::bail!(
            "database open requires the host encryption cipher to migrate account {id}.{column}; use Database::open_with_cipher"
        );
    };
    cipher.decrypt(value).with_context(|| {
        format!("host cipher rejected account {id}.{column}; ciphertext bytes were not rewritten")
    })?;
    Ok(())
}

pub(super) fn preflight_ciphertext_probes(
    conn: &Connection,
    cipher: Option<&dyn KeyCipher>,
) -> Result<()> {
    if table_exists(conn, "accounts")? {
        if table_has_column(conn, "accounts", "key_cipher")? {
            let mut stmt = conn.prepare("SELECT id, key_cipher FROM accounts")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, value) in rows {
                probe_account_cipher(cipher, &id, "key_cipher", &value)?;
            }
        }
        if table_has_column(conn, "accounts", "password_cipher")? {
            let mut stmt = conn.prepare(
                "SELECT id, password_cipher FROM accounts WHERE password_cipher IS NOT NULL AND password_cipher <> ''",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, value) in rows {
                probe_account_cipher(cipher, &id, "password_cipher", &value)?;
            }
        }
    }
    if table_exists(conn, "credentials")? && table_has_column(conn, "credentials", "key_cipher")? {
        let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
            "WHERE COALESCE(credential_purpose, 'inference') = 'inference'"
        } else {
            ""
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT legacy_account_id, key_cipher, password_cipher FROM credentials
             {purpose_filter}"
        ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, key, password) in rows {
            probe_account_cipher(cipher, &id, "key_cipher", &key)?;
            if let Some(value) = password.as_deref() {
                probe_account_cipher(cipher, &id, "password_cipher", value)?;
            }
        }
    }
    Ok(())
}

pub(super) fn repair_legacy_account_ciphertext(
    conn: &Connection,
    cipher: &dyn KeyCipher,
) -> Result<()> {
    let (select_sql, both_sql, key_sql, password_sql) = if table_exists(conn, "credentials")?
        && table_has_column(conn, "credentials", "key_cipher")?
    {
        (
            if table_has_column(conn, "credentials", "credential_purpose")? {
                "SELECT legacy_account_id, key_cipher, password_cipher FROM credentials
                 WHERE COALESCE(credential_purpose, 'inference') = 'inference'"
            } else {
                "SELECT legacy_account_id, key_cipher, password_cipher FROM credentials"
            },
            "UPDATE credentials SET key_cipher = ?1, password_cipher = ?2 WHERE legacy_account_id = ?3",
            "UPDATE credentials SET key_cipher = ?1 WHERE legacy_account_id = ?2",
            "UPDATE credentials SET password_cipher = ?1 WHERE legacy_account_id = ?2",
        )
    } else if table_exists(conn, "accounts")? {
        let has_key = table_has_column(conn, "accounts", "key_cipher")?;
        let has_password = table_has_column(conn, "accounts", "password_cipher")?;
        if !has_key && !has_password {
            return Ok(());
        }
        (
            match (has_key, has_password) {
                (true, true) => "SELECT id, key_cipher, password_cipher FROM accounts",
                (true, false) => "SELECT id, key_cipher, NULL FROM accounts",
                (false, true) => "SELECT id, '', password_cipher FROM accounts",
                (false, false) => return Ok(()),
            },
            "UPDATE accounts SET key_cipher = ?1, password_cipher = ?2 WHERE id = ?3",
            "UPDATE accounts SET key_cipher = ?1 WHERE id = ?2",
            "UPDATE accounts SET password_cipher = ?1 WHERE id = ?2",
        )
    } else {
        return Ok(());
    };

    let rows = {
        let mut stmt = conn.prepare(select_sql)?;
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    if !rows.iter().any(|(_, key, password)| {
        is_legacy_local_ciphertext(key)
            || password.as_deref().is_some_and(is_legacy_local_ciphertext)
    }) {
        return Ok(());
    }

    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let rows = {
        let mut stmt = tx.prepare(select_sql)?;
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut updates: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for (id, key, password) in rows {
        let new_key = if is_legacy_local_ciphertext(&key) {
            let plaintext = cipher.decrypt(&key).with_context(|| {
                format!("host cipher rejected account {id}.key_cipher during v2 repair")
            })?;
            Some(
                cipher
                    .encrypt(&plaintext)
                    .with_context(|| format!("failed to rewrite account {id}.key_cipher to v2"))?,
            )
        } else {
            None
        };
        let new_password = if let Some(value) = password.as_deref() {
            if is_legacy_local_ciphertext(value) {
                let plaintext = cipher.decrypt(value).with_context(|| {
                    format!("host cipher rejected account {id}.password_cipher during v2 repair")
                })?;
                Some(cipher.encrypt(&plaintext).with_context(|| {
                    format!("failed to rewrite account {id}.password_cipher to v2")
                })?)
            } else {
                None
            }
        } else {
            None
        };
        if new_key.is_some() || new_password.is_some() {
            updates.push((id, new_key, new_password));
        }
    }

    for (id, new_key, new_password) in updates {
        match (new_key, new_password) {
            (Some(key), Some(password)) => {
                tx.execute(both_sql, params![key, password, id])?;
            }
            (Some(key), None) => {
                tx.execute(key_sql, params![key, id])?;
            }
            (None, Some(password)) => {
                tx.execute(password_sql, params![password, id])?;
            }
            (None, None) => {}
        }
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to read {} for SHA-256 evidence", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; BACKUP_HASH_BUFFER_LEN];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn sync_file(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .or_else(|_| std::fs::File::open(path))
        .with_context(|| format!("failed to open {} for durability sync", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(())
}

pub(super) fn sync_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(parent).with_context(|| {
            format!(
                "failed to open directory {} for durability sync",
                parent.display()
            )
        })?;
        dir.sync_all()
            .with_context(|| format!("failed to sync directory {}", parent.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

pub(super) fn write_backup_sha256_evidence(backup_path: &Path) -> Result<String> {
    sync_file(backup_path)?;
    let digest = sha256_file(backup_path)?;
    let file_name = backup_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("backup path is not UTF-8: {}", backup_path.display()))?;
    let evidence_path = backup_path.with_file_name(format!("{file_name}.sha256"));
    let tmp_path = backup_path.with_file_name(format!(
        "{file_name}.sha256.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let mut tmp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| {
                format!(
                    "failed to create SHA-256 evidence temp {}",
                    tmp_path.display()
                )
            })?;
        tmp.write_all(format!("{digest}  {file_name}\n").as_bytes())
            .with_context(|| {
                format!(
                    "failed to write SHA-256 evidence temp {}",
                    tmp_path.display()
                )
            })?;
        tmp.flush()?;
        tmp.sync_all()?;
    }
    std::fs::rename(&tmp_path, &evidence_path).with_context(|| {
        format!(
            "failed to publish SHA-256 evidence {} -> {}",
            tmp_path.display(),
            evidence_path.display()
        )
    })?;
    sync_parent_dir(&evidence_path)?;
    Ok(digest)
}

pub(super) fn verify_pre_v3_backup(path: &Path) -> Result<()> {
    sqlite_quick_check(&Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)?;
    verify_schema_backup(path, PRE_V3_BACKUP_FILE_PREFIX, V26_SCHEMA_VERSION)?;
    let backup = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to reopen pre-v3 backup {}", path.display()))?;
    sqlite_quick_check(&backup)?;
    Ok(())
}

pub(super) fn create_pre_v3_backup(conn: &Connection, db_path: &Path) -> Result<PathBuf> {
    for _ in 0..8 {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%9fZ");
        let backup_path =
            db_path.with_file_name(format!("{PRE_V3_BACKUP_FILE_PREFIX}{timestamp}.bak"));
        if backup_path.exists() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        let backup_value = backup_path.to_string_lossy().into_owned();
        #[cfg(test)]
        let vacuum_race = v27_test_hooks::install_vacuum_race(db_path, &backup_path);
        let vacuum = conn.execute("VACUUM main INTO ?1", [&backup_value]);
        #[cfg(test)]
        if let Some(race) = vacuum_race {
            race.finish();
        }
        vacuum.with_context(|| {
            format!(
                "failed to create pre-v3 database backup {}",
                backup_path.display()
            )
        })?;
        verify_pre_v3_backup(&backup_path)?;
        write_backup_sha256_evidence(&backup_path)?;
        return Ok(backup_path);
    }
    anyhow::bail!("failed to allocate a unique pre-v3 backup filename")
}
