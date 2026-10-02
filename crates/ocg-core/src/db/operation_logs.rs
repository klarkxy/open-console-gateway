//! Schema 65 `operation_logs` and the explicit begin/finish/record API.
//!
//! One `operation_id` owns one row. `begin_operation` inserts `pending`.
//! `finish_operation` updates that same row once. A second terminal finish
//! returns the stored receipt and does not replace it. Recording errors stay
//! on this write; they do not roll back unrelated statements.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::Database;
use crate::log_types::{
    CanonicalFinish, LogReadError, OperationFinish, OperationLog, OperationLogPage,
    OperationLogQuery, canonicalize_finish, canonicalize_operation, normalize_operation_query,
    operation_from_stored,
};

const SELECT_OPERATION: &str = "SELECT operation_id, started_at, completed_at, action, source,
    actor_id, subject_type, subject_id, outcome, reason_code, metadata_json
    FROM operation_logs";

pub(super) fn ensure_v65_storage(conn: &Connection) -> Result<()> {
    // `table_info` omits generated columns, so a v65 reopen would otherwise
    // try to add `request_group_key` again.
    if !column_in_table_xinfo(conn, "forward_logs", "request_group_key")? {
        conn.execute_batch(
            "ALTER TABLE forward_logs ADD COLUMN request_group_key TEXT
             GENERATED ALWAYS AS (
                CASE
                    WHEN request_id IS NOT NULL AND length(trim(request_id, char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288))) > 0
                    THEN 'request:' || request_id
                    ELSE 'legacy:' || id
                END
             ) VIRTUAL",
        )
        .context("add forward_logs.request_group_key")?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS operation_logs (
            operation_id TEXT PRIMARY KEY,
            started_at TEXT NOT NULL,
            completed_at TEXT,
            action TEXT NOT NULL,
            source TEXT NOT NULL CHECK (source IN ('dashboard', 'cli', 'desktop')),
            actor_id TEXT,
            subject_type TEXT,
            subject_id TEXT,
            outcome TEXT NOT NULL CHECK (
                outcome IN ('pending', 'success', 'rejected', 'failed', 'partial', 'compensated')
            ),
            reason_code TEXT,
            metadata_json TEXT NOT NULL,
            CHECK (
                (outcome = 'pending' AND completed_at IS NULL)
                OR (outcome <> 'pending' AND completed_at IS NOT NULL)
            )
         );
         CREATE INDEX IF NOT EXISTS idx_operation_logs_started
            ON operation_logs(started_at DESC, operation_id ASC);
         CREATE INDEX IF NOT EXISTS idx_operation_logs_subject
            ON operation_logs(subject_type, subject_id, started_at DESC);
         CREATE INDEX IF NOT EXISTS idx_forward_logs_request_group
            ON forward_logs(request_group_key, attempt, id);",
    )
    .context("create operation_logs")?;
    Ok(())
}

impl Database {
    pub fn record_existing_operation(
        data_dir: &std::path::Path,
        operation: &OperationLog,
    ) -> Result<OperationLog> {
        record_existing_operation(data_dir, operation)
    }
    pub(crate) fn pending_operations_by_related_id(
        &self,
        action: &str,
        related_id: &str,
    ) -> Result<Vec<OperationLog>> {
        let mut statement = self.conn.prepare(&format!(
            "{SELECT_OPERATION} WHERE action = ?1 AND outcome = 'pending'
             AND EXISTS (SELECT 1 FROM json_each(metadata_json, '$.relatedIds') WHERE value = ?2)"
        ))?;
        let rows = statement.query_map(params![action, related_id], operation_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
    /// Insert one receipt. If `operation_id` already exists, return that row
    /// unchanged.
    pub fn record_operation(&self, operation: &OperationLog) -> Result<OperationLog> {
        record_operation_on(&self.conn, operation)
    }

    /// Insert the pending row for one operation. A duplicate id is rejected
    /// and the stored row is left unchanged.
    pub fn begin_operation(&self, operation: &OperationLog) -> Result<OperationLog> {
        if operation.outcome != crate::log_types::OperationOutcome::Pending {
            anyhow::bail!("invalid operation outcome");
        }
        let canonical =
            canonicalize_operation(operation).map_err(|message| anyhow::anyhow!("{message}"))?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let result = (|| -> Result<OperationLog> {
            let inserted = tx
                .execute(
                    "INSERT INTO operation_logs (
                        operation_id, started_at, completed_at, action, source, actor_id,
                        subject_type, subject_id, outcome, reason_code, metadata_json
                     ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9)
                     ON CONFLICT(operation_id) DO NOTHING",
                    params![
                        canonical.operation_id,
                        canonical.started_at,
                        canonical.action,
                        canonical.source.as_str(),
                        canonical.actor_id,
                        canonical.subject_type,
                        canonical.subject_id,
                        canonical.reason_code,
                        canonical.metadata_json,
                    ],
                )
                .context("operation log write")?;
            if inserted == 0 {
                anyhow::bail!("operation already exists");
            }
            load_operation(&tx, &canonical.operation_id)?
                .context("operation log write missed the stored row")
        })();
        finish_write(tx, result)
    }

    /// Move one pending row to a terminal outcome. If that id is already
    /// terminal, keep the first receipt and return it.
    pub fn finish_operation(
        &self,
        operation_id: &str,
        finish: &OperationFinish,
    ) -> Result<OperationLog> {
        let (operation_id, finish) = canonicalize_finish(operation_id, finish)
            .map_err(|message| anyhow::anyhow!("{message}"))?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let result = apply_finish(&tx, &operation_id, &finish);
        finish_write(tx, result)
    }

    pub fn query_operation_logs(
        &self,
        query: &OperationLogQuery,
    ) -> std::result::Result<OperationLogPage, LogReadError> {
        query_operation_logs_on(&self.conn, query)
    }
}

fn column_in_table_xinfo(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_xinfo({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(names
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column))
}

fn apply_finish(
    tx: &Transaction<'_>,
    operation_id: &str,
    finish: &CanonicalFinish,
) -> Result<OperationLog> {
    tx.execute(
        "UPDATE operation_logs
         SET completed_at = ?2, outcome = ?3, reason_code = ?4, metadata_json = ?5
         WHERE operation_id = ?1 AND outcome = 'pending'",
        params![
            operation_id,
            finish.completed_at,
            finish.outcome.as_str(),
            finish.reason_code,
            finish.metadata_json,
        ],
    )
    .context("operation log write")?;
    load_operation(tx, operation_id)?.context("operation not found")
}

fn finish_write(tx: Transaction<'_>, result: Result<OperationLog>) -> Result<OperationLog> {
    match result {
        Ok(operation) => {
            tx.commit().context("operation log write")?;
            Ok(operation)
        }
        Err(error) => {
            let _ = tx.rollback();
            Err(error)
        }
    }
}

fn load_operation(conn: &Connection, operation_id: &str) -> Result<Option<OperationLog>> {
    conn.query_row(
        &format!("{SELECT_OPERATION} WHERE operation_id = ?1"),
        [operation_id],
        operation_from_row,
    )
    .optional()
    .context("operation log read")
}

fn operation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationLog> {
    operation_from_stored(
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    )
    .map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
    })
}

fn operation_filter(
    query: &crate::log_types::NormalizedOperationQuery,
) -> (String, Vec<Option<String>>) {
    let mut filter = String::new();
    let mut params = Vec::new();
    let mut push = |column: &str, value: Option<String>| {
        let index = params.len() + 1;
        if filter.is_empty() {
            filter.push_str(" WHERE ");
        } else {
            filter.push_str(" AND ");
        }
        filter.push_str(&format!("(?{index} IS NULL OR {column} = ?{index})"));
        params.push(value);
    };
    push("action", query.action.clone());
    push(
        "source",
        query.source.map(|source| source.as_str().to_string()),
    );
    push(
        "outcome",
        query.outcome.map(|outcome| outcome.as_str().to_string()),
    );
    push("subject_type", query.subject_type.clone());
    push("subject_id", query.subject_id.clone());
    let start_index = params.len() + 1;
    let end_index = params.len() + 2;
    if filter.is_empty() {
        filter.push_str(" WHERE ");
    } else {
        filter.push_str(" AND ");
    }
    filter.push_str(&format!(
        "(?{start_index} IS NULL OR julianday(started_at) >= julianday(?{start_index}))
         AND (?{end_index} IS NULL OR julianday(started_at) <= julianday(?{end_index}))"
    ));
    params.push(query.start_time.clone());
    params.push(query.end_time.clone());
    (filter, params)
}

pub(crate) fn query_operation_logs_on(
    conn: &rusqlite::Connection,
    query: &OperationLogQuery,
) -> std::result::Result<OperationLogPage, LogReadError> {
    let query = normalize_operation_query(query).map_err(LogReadError::Invalid)?;
    let (filter, params) = operation_filter(&query);
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM operation_logs{filter}"),
            rusqlite::params_from_iter(params.iter()),
            |row| row.get(0),
        )
        .map_err(LogReadError::from)?;
    let mut item_params = params;
    item_params.push(Some(query.limit.to_string()));
    item_params.push(Some(query.offset.to_string()));
    let mut stmt = conn
        .prepare(&format!(
            "{SELECT_OPERATION}{filter}
             ORDER BY started_at DESC, operation_id ASC
             LIMIT ? OFFSET ?"
        ))
        .map_err(LogReadError::from)?;
    let items = stmt
        .query_map(
            rusqlite::params_from_iter(item_params.iter()),
            operation_from_row,
        )
        .map_err(LogReadError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(LogReadError::from)?;
    Ok(OperationLogPage {
        items,
        total,
        limit: query.limit,
        offset: query.offset,
    })
}

fn record_operation_on(conn: &Connection, operation: &OperationLog) -> Result<OperationLog> {
    let canonical =
        canonicalize_operation(operation).map_err(|message| anyhow::anyhow!("{message}"))?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let result = (|| -> Result<OperationLog> {
        tx.execute(
            "INSERT INTO operation_logs (
                    operation_id, started_at, completed_at, action, source, actor_id,
                    subject_type, subject_id, outcome, reason_code, metadata_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(operation_id) DO NOTHING",
            params![
                canonical.operation_id,
                canonical.started_at,
                canonical.completed_at,
                canonical.action,
                canonical.source.as_str(),
                canonical.actor_id,
                canonical.subject_type,
                canonical.subject_id,
                canonical.outcome.as_str(),
                canonical.reason_code,
                canonical.metadata_json,
            ],
        )
        .context("operation log write")?;
        load_operation(&tx, &canonical.operation_id)?
            .context("operation log write missed the stored row")
    })();
    finish_write(tx, result)
}

/// Best-effort attachment for standalone commands after their domain work.
/// Opens only existing storage: no schema initialization, cipher, or directory.
fn record_existing_operation(
    data_dir: &std::path::Path,
    operation: &OperationLog,
) -> Result<OperationLog> {
    let path = if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(data_dir)
    }
    .join("data.sqlite");
    for ancestor in path.ancestors() {
        let meta = std::fs::symlink_metadata(ancestor)?;
        anyhow::ensure!(
            !crate::fs_privacy::metadata_is_reparse(&meta),
            "linked operation storage"
        );
    }
    let conn = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(100))?;
    record_operation_on(&conn, operation)
}
