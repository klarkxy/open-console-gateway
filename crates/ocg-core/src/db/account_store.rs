//! Credential-backed account row store (schema v52+).
//!
//! While `accounts` still exists (migrate-up before DROP), reads prefer that
//! table so `project()` can rebuild the shadow. After the drop, reconstruction
//! uses `credentials` (joined to `destinations` only for destination identity).

use super::*;
use ocg_domain::credential::credential_id_for_legacy_account;
use ocg_domain::destination::{
    destination_id_for_builtin, destination_id_for_custom_account, destination_id_for_dynamic,
    destination_id_for_platform_account,
};
use ocg_domain::ids::CUSTOM_PROVIDER_ID;

pub(crate) const ACCOUNT_SELECT_FROM_ACCOUNTS: &str = "SELECT id, name, username, password_cipher, key_cipher, enabled, referral_code, recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, created_at, updated_at, auth_error, account_type, setup_step, notes, provider_id, credential_kind, quota_scope FROM accounts";

pub(crate) const ACCOUNT_SELECT_FROM_CREDENTIALS: &str = "SELECT legacy_account_id, name, username, password_cipher, key_cipher, enabled, referral_code, purchase_date, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, created_at, updated_at, auth_error, account_type, setup_step, notes, provider_id, credential_kind, quota_scope FROM credentials";

pub(crate) const V52_CREDENTIAL_COLUMNS: &[(&str, &str)] = &[
    ("username", "TEXT"),
    ("referral_code", "TEXT"),
    ("cooldown_until", "TEXT"),
    ("created_at", "TEXT"),
    ("updated_at", "TEXT"),
    ("auth_error", "TEXT"),
    ("account_type", "TEXT"),
    ("setup_step", "TEXT"),
    ("provider_id", "TEXT"),
    ("credential_kind", "TEXT"),
    ("quota_scope", "TEXT"),
    ("identity_id", "TEXT"),
    ("verification_status", "TEXT"),
    ("connection_verified_at", "TEXT"),
    ("verification_error", "TEXT"),
    ("usage_5h_window_started_at", "TEXT"),
    ("usage_5h_window_cost_offset", "REAL NOT NULL DEFAULT 0"),
    ("usage_week_window_started_at", "TEXT"),
    ("usage_week_window_cost_offset", "REAL NOT NULL DEFAULT 0"),
    ("usage_month_window_cost_offset", "REAL NOT NULL DEFAULT 0"),
];

const CHILD_TABLES_WITH_ACCOUNTS_FK: &[&str] = &[
    "account_custom_configs",
    "account_model_capabilities",
    "ollama_cloud_billing",
    "credential_state",
    "credential_bindings",
    "onboarding_tasks",
    "subscription_records",
    "quota_pool_members",
    "platform_links",
    "quota_windows",
    "credit_balances",
    "provider_usage_sync_state",
    "cpa_integration",
];

#[derive(Debug, Clone, Copy)]
pub(crate) struct AccountRowSource {
    pub table: &'static str,
    pub id_col: &'static str,
    pub sort_col: &'static str,
    #[allow(dead_code)]
    pub purchase_col: &'static str,
}

pub(crate) fn account_row_source(conn: &Connection) -> Result<AccountRowSource> {
    if table_exists(conn, "accounts")? {
        Ok(AccountRowSource {
            table: "accounts",
            id_col: "id",
            sort_col: "sort_order",
            purchase_col: "recharge_date",
        })
    } else {
        Ok(AccountRowSource {
            table: "credentials",
            id_col: "legacy_account_id",
            sort_col: "routing_rank",
            purchase_col: "purchase_date",
        })
    }
}

pub(crate) fn accounts_table_exists(conn: &Connection) -> Result<bool> {
    table_exists(conn, "accounts")
}

fn inference_predicate(conn: &Connection, table: &str) -> Result<String> {
    if table == "credentials" && table_has_column(conn, "credentials", "credential_purpose")? {
        Ok(format!(
            " AND COALESCE({table}.credential_purpose, '{}') = '{}'",
            crate::db::platform::CREDENTIAL_PURPOSE_INFERENCE,
            crate::db::platform::CREDENTIAL_PURPOSE_INFERENCE
        ))
    } else {
        Ok(String::new())
    }
}

pub(crate) fn get_account_on(conn: &Connection, id: &str) -> Result<Option<Account>> {
    let source = account_row_source(conn)?;
    let sql = format!(
        "{} WHERE {} = ?1{}",
        account_select_sql(source.table),
        source.id_col,
        inference_predicate(conn, source.table)?
    );
    let mut stmt = conn.prepare(&sql)?;
    let account = stmt.query_row([id], account_from_row).optional()?;
    Ok(account)
}

pub(crate) fn list_accounts_on(conn: &Connection) -> Result<Vec<Account>> {
    let source = account_row_source(conn)?;
    let sql = format!(
        "{} WHERE 1=1{} ORDER BY {}.{} ASC, {}.created_at ASC, {}.{} ASC",
        account_select_sql(source.table),
        inference_predicate(conn, source.table)?,
        source.table,
        source.sort_col,
        source.table,
        source.table,
        source.id_col
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], account_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn account_select_sql(table: &str) -> &'static str {
    if table == "accounts" {
        ACCOUNT_SELECT_FROM_ACCOUNTS
    } else {
        ACCOUNT_SELECT_FROM_CREDENTIALS
    }
}

#[allow(dead_code)]
pub(crate) fn account_exists_on(conn: &Connection, id: &str) -> Result<bool> {
    Ok(get_account_on(conn, id)?.is_some())
}

pub(crate) fn select_account_identity_id(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<String>> {
    let source = account_row_source(conn)?;
    let sql = format!(
        "SELECT identity_id FROM {} WHERE {} = ?1{}",
        source.table,
        source.id_col,
        inference_predicate(conn, source.table)?
    );
    Ok(conn
        .query_row(&sql, [account_id], |row| row.get(0))
        .optional()?
        .flatten())
}

pub(crate) fn update_account_identity_id(
    conn: &Connection,
    account_id: &str,
    identity_id: &str,
) -> Result<usize> {
    let source = account_row_source(conn)?;
    let sql = format!(
        "UPDATE {} SET identity_id = ?2 WHERE {} = ?1",
        source.table, source.id_col
    );
    Ok(conn.execute(&sql, params![account_id, identity_id])?)
}

pub(crate) fn count_accounts_with_identity(
    conn: &Connection,
    identity_id: &str,
    except_account_id: Option<&str>,
) -> Result<i64> {
    let source = account_row_source(conn)?;
    let (sql, params): (String, Vec<&str>) = match except_account_id {
        Some(except) => (
            format!(
                "SELECT COUNT(*) FROM {} WHERE identity_id = ?1 AND {} <> ?2{}",
                source.table,
                source.id_col,
                inference_predicate(conn, source.table)?
            ),
            vec![identity_id, except],
        ),
        None => (
            format!(
                "SELECT COUNT(*) FROM {} WHERE identity_id = ?1{}",
                source.table,
                inference_predicate(conn, source.table)?
            ),
            vec![identity_id],
        ),
    };
    Ok(conn.query_row(&sql, params_from_iter(params), |row| row.get(0))?)
}

pub(crate) fn count_accounts_missing_identity_column(
    conn: &Connection,
    column: &str,
) -> Result<i64> {
    let source = account_row_source(conn)?;
    if source.table != "credentials" || !table_has_column(conn, "credentials", column)? {
        return Ok(0);
    }
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM credentials
             WHERE ({column} IS NULL OR {column} = ''){}",
            inference_predicate(conn, "credentials")?
        ),
        [],
        |row| row.get(0),
    )?)
}

pub(crate) fn count_accounts_missing_identity(conn: &Connection) -> Result<i64> {
    let source = account_row_source(conn)?;
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM {} WHERE (identity_id IS NULL OR identity_id = ''){}",
            source.table,
            inference_predicate(conn, source.table)?
        ),
        [],
        |row| row.get(0),
    )?)
}

pub(crate) fn count_accounts_missing_child(
    conn: &Connection,
    child_table: &str,
    child_account_col: &str,
) -> Result<i64> {
    let source = account_row_source(conn)?;
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM {} a
             WHERE NOT EXISTS (SELECT 1 FROM {child_table} c WHERE c.{child_account_col} = a.{}){}",
            source.table,
            source.id_col,
            inference_predicate(conn, source.table)?.replacen("credentials.", "a.", 1)
        ),
        [],
        |row| row.get(0),
    )?)
}

pub(crate) fn select_account_provider_id(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<String>> {
    let source = account_row_source(conn)?;
    Ok(conn
        .query_row(
            &format!(
                "SELECT provider_id FROM {} WHERE {} = ?1",
                source.table, source.id_col
            ),
            [account_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn select_account_sort_order(conn: &Connection, account_id: &str) -> Result<i64> {
    let source = account_row_source(conn)?;
    Ok(conn.query_row(
        &format!(
            "SELECT {} FROM {} WHERE {} = ?1",
            source.sort_col, source.table, source.id_col
        ),
        [account_id],
        |row| row.get(0),
    )?)
}

pub(crate) fn list_account_ids_for_identity(
    conn: &Connection,
    identity_id: &str,
) -> Result<Vec<String>> {
    let source = account_row_source(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} WHERE identity_id = ?1{}",
        source.id_col,
        source.table,
        inference_predicate(conn, source.table)?
    ))?;
    let rows = stmt.query_map([identity_id], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[allow(dead_code)]
pub(crate) fn list_account_ids_on(conn: &Connection) -> Result<Vec<String>> {
    let source = account_row_source(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} WHERE 1=1{} ORDER BY {} ASC, created_at ASC, {} ASC",
        source.id_col,
        source.table,
        inference_predicate(conn, source.table)?,
        source.sort_col,
        source.id_col
    ))?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn count_accounts_for_provider_on(conn: &Connection, provider_id: &str) -> Result<i64> {
    let source = account_row_source(conn)?;
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM {} WHERE lower(provider_id) = lower(?1){}",
            source.table,
            inference_predicate(conn, source.table)?
        ),
        [provider_id],
        |row| row.get(0),
    )?)
}

#[allow(dead_code)]
pub(crate) fn list_account_ids_for_provider(
    conn: &Connection,
    provider_id: &str,
) -> Result<Vec<String>> {
    let source = account_row_source(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {} WHERE lower(provider_id) = lower(?1){}",
        source.id_col,
        source.table,
        inference_predicate(conn, source.table)?
    ))?;
    let rows = stmt.query_map([provider_id], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

#[allow(dead_code)]
pub(crate) fn delete_account_row(conn: &Connection, id: &str) -> Result<usize> {
    let source = account_row_source(conn)?;
    Ok(conn.execute(
        &format!("DELETE FROM {} WHERE {} = ?1", source.table, source.id_col),
        [id],
    )?)
}

pub(crate) fn destination_id_for_account(conn: &Connection, account: &Account) -> Result<String> {
    if let Some(parent_id) = custom_store::platform_parent_id(conn, &account.id)? {
        return Ok(destination_id_for_platform_account(&parent_id));
    }
    if account.provider_id == CUSTOM_PROVIDER_ID {
        return Ok(destination_id_for_custom_account(&account.id));
    }
    if builtin_provider(&account.provider_id).is_some() {
        return Ok(destination_id_for_builtin(&account.provider_id));
    }
    Ok(destination_id_for_dynamic(&account.provider_id))
}

/// Runtime insert: write the credential row (and usage-sync stub). After v52
/// this is the account-row store. `refresh_destination_shadow` rebuilds
/// destinations from `project()`.
pub(crate) fn insert_account_columns(
    conn: &Connection,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
) -> Result<()> {
    let destination_id = destination_id_for_account(conn, account)?;
    let credential_id = credential_id_for_legacy_account(&account.id).to_string();
    let routing_rank: i64 = conn.query_row(
        "SELECT COALESCE(MAX(routing_rank), -1) + 1 FROM credentials",
        [],
        |row| row.get(0),
    )?;
    let has_secret = !account.key_cipher.is_empty()
        || account
            .password_cipher
            .as_deref()
            .is_some_and(|value| !value.is_empty());
    let scope_json = serde_json::to_string(&ocg_domain::credential::ModelScope::All)?;
    conn.execute(
        "INSERT INTO credentials (
            id, legacy_account_id, destination_id, name, notes, has_secret,
            enabled, routing_rank, scope_json, auth_state, last_error,
            cooldown_generic_until, cooldown_5h_until, cooldown_week_until,
            cooldown_month_until, cooldown_free_until, quota_pool_id,
            onboarding_json, purchase_date, key_cipher, password_cipher,
            username, referral_code, cooldown_until, created_at, updated_at,
            auth_error, account_type, setup_step, provider_id, credential_kind,
            quota_scope, identity_id, verification_status, connection_verified_at,
            verification_error, usage_5h_window_started_at, usage_5h_window_cost_offset,
            usage_week_window_started_at, usage_week_window_cost_offset,
            usage_month_window_cost_offset
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'unknown', ?10,
            ?11, ?12, ?13, ?14, ?15, NULL, NULL, ?16, ?17, ?18,
            ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29,
            NULL, ?30, NULL, NULL, NULL, 0, NULL, 0, 0
         )",
        params![
            credential_id,
            account.id,
            destination_id,
            account.name,
            account.notes,
            i64::from(has_secret),
            i64::from(account.enabled),
            routing_rank,
            scope_json,
            account.last_error,
            account.cooldown_generic_until.map(|t| t.to_rfc3339()),
            account.cooldown_5h_until.map(|t| t.to_rfc3339()),
            account.cooldown_week_until.map(|t| t.to_rfc3339()),
            account.cooldown_month_until.map(|t| t.to_rfc3339()),
            account.cooldown_free_until.map(|t| t.to_rfc3339()),
            purchase_date,
            account.key_cipher,
            account.password_cipher,
            account.username,
            account.referral_code,
            account.cooldown_until.map(|t| t.to_rfc3339()),
            account.created_at.to_rfc3339(),
            account.updated_at.to_rfc3339(),
            account.auth_error,
            account.account_type.as_str(),
            account.setup_step.as_str(),
            account.provider_id,
            account.credential_kind.as_str(),
            account.quota_scope.as_str(),
            verification_status.as_str(),
        ],
    )?;
    conn.execute(
        "INSERT INTO provider_usage_sync_state (
            account_id, last_success_at, last_attempt_at, next_eligible_at,
            failure_streak, last_expedited_at
         ) VALUES (?1, NULL, NULL, NULL, 0, NULL)",
        [&account.id],
    )?;
    if table_exists(conn, "accounts")? {
        let sort_order: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM accounts",
            [],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT INTO accounts (
                id, name, username, password_cipher, key_cipher, enabled, referral_code,
                recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until,
                cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error,
                created_at, updated_at, auth_error, account_type, setup_step, notes,
                provider_id, credential_kind, quota_scope, sort_order, identity_id,
                verification_status
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, NULL, ?26
             )",
            params![
                account.id,
                account.name,
                account.username,
                account.password_cipher,
                account.key_cipher,
                i64::from(account.enabled),
                account.referral_code,
                purchase_date,
                account.cooldown_until.map(|t| t.to_rfc3339()),
                account.cooldown_generic_until.map(|t| t.to_rfc3339()),
                account.cooldown_5h_until.map(|t| t.to_rfc3339()),
                account.cooldown_week_until.map(|t| t.to_rfc3339()),
                account.cooldown_month_until.map(|t| t.to_rfc3339()),
                account.cooldown_free_until.map(|t| t.to_rfc3339()),
                account.last_error,
                account.created_at.to_rfc3339(),
                account.updated_at.to_rfc3339(),
                account.auth_error,
                account.account_type.as_str(),
                account.setup_step.as_str(),
                account.notes,
                account.provider_id,
                account.credential_kind.as_str(),
                account.quota_scope.as_str(),
                sort_order,
                verification_status.as_str(),
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn ensure_v52_credential_columns(conn: &Connection) -> Result<()> {
    for (column, definition) in V52_CREDENTIAL_COLUMNS {
        if !table_has_column(conn, "credentials", column)? {
            conn.execute(
                &format!("ALTER TABLE credentials ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn backfill_credential_account_columns(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "accounts")? {
        return Ok(());
    }
    conn.execute_batch(
        "UPDATE credentials
         SET username = COALESCE((SELECT username FROM accounts WHERE accounts.id = credentials.legacy_account_id), username),
             referral_code = COALESCE((SELECT referral_code FROM accounts WHERE accounts.id = credentials.legacy_account_id), referral_code),
             cooldown_until = COALESCE((SELECT cooldown_until FROM accounts WHERE accounts.id = credentials.legacy_account_id), cooldown_until),
             created_at = COALESCE((SELECT created_at FROM accounts WHERE accounts.id = credentials.legacy_account_id), created_at),
             updated_at = COALESCE((SELECT updated_at FROM accounts WHERE accounts.id = credentials.legacy_account_id), updated_at),
             auth_error = COALESCE((SELECT auth_error FROM accounts WHERE accounts.id = credentials.legacy_account_id), auth_error),
             account_type = COALESCE((SELECT account_type FROM accounts WHERE accounts.id = credentials.legacy_account_id), account_type, 'key'),
             setup_step = COALESCE((SELECT setup_step FROM accounts WHERE accounts.id = credentials.legacy_account_id), setup_step, 'ready'),
             provider_id = COALESCE((SELECT provider_id FROM accounts WHERE accounts.id = credentials.legacy_account_id), provider_id),
             credential_kind = COALESCE((SELECT credential_kind FROM accounts WHERE accounts.id = credentials.legacy_account_id), credential_kind),
             quota_scope = COALESCE((SELECT quota_scope FROM accounts WHERE accounts.id = credentials.legacy_account_id), quota_scope),
             identity_id = COALESCE((SELECT identity_id FROM accounts WHERE accounts.id = credentials.legacy_account_id), identity_id),
             verification_status = COALESCE((SELECT verification_status FROM accounts WHERE accounts.id = credentials.legacy_account_id), verification_status, 'not_required'),
             connection_verified_at = COALESCE((SELECT connection_verified_at FROM accounts WHERE accounts.id = credentials.legacy_account_id), connection_verified_at),
             verification_error = COALESCE((SELECT verification_error FROM accounts WHERE accounts.id = credentials.legacy_account_id), verification_error),
             usage_5h_window_started_at = COALESCE((SELECT usage_5h_window_started_at FROM accounts WHERE accounts.id = credentials.legacy_account_id), usage_5h_window_started_at),
             usage_5h_window_cost_offset = COALESCE((SELECT usage_5h_window_cost_offset FROM accounts WHERE accounts.id = credentials.legacy_account_id), usage_5h_window_cost_offset, 0),
             usage_week_window_started_at = COALESCE((SELECT usage_week_window_started_at FROM accounts WHERE accounts.id = credentials.legacy_account_id), usage_week_window_started_at),
             usage_week_window_cost_offset = COALESCE((SELECT usage_week_window_cost_offset FROM accounts WHERE accounts.id = credentials.legacy_account_id), usage_week_window_cost_offset, 0),
             usage_month_window_cost_offset = COALESCE((SELECT usage_month_window_cost_offset FROM accounts WHERE accounts.id = credentials.legacy_account_id), usage_month_window_cost_offset, 0),
             key_cipher = COALESCE((SELECT key_cipher FROM accounts WHERE accounts.id = credentials.legacy_account_id), key_cipher),
             password_cipher = COALESCE((SELECT password_cipher FROM accounts WHERE accounts.id = credentials.legacy_account_id), password_cipher),
             enabled = COALESCE((SELECT enabled FROM accounts WHERE accounts.id = credentials.legacy_account_id), enabled),
             routing_rank = COALESCE((SELECT sort_order FROM accounts WHERE accounts.id = credentials.legacy_account_id), routing_rank),
             name = COALESCE((SELECT name FROM accounts WHERE accounts.id = credentials.legacy_account_id), name),
             notes = COALESCE((SELECT notes FROM accounts WHERE accounts.id = credentials.legacy_account_id), notes),
             last_error = COALESCE((SELECT last_error FROM accounts WHERE accounts.id = credentials.legacy_account_id), last_error),
             purchase_date = COALESCE((SELECT recharge_date FROM accounts WHERE accounts.id = credentials.legacy_account_id), purchase_date)
         WHERE EXISTS (SELECT 1 FROM accounts WHERE accounts.id = credentials.legacy_account_id);",
    )?;
    Ok(())
}

pub(crate) fn assert_credential_totality(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "accounts")? {
        return Ok(());
    }
    let missing: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts a
         WHERE NOT EXISTS (
            SELECT 1 FROM credentials c WHERE c.legacy_account_id = a.id
         )",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        missing == 0,
        "v52 refuses to drop accounts: {missing} account row(s) have no credentials.legacy_account_id"
    );
    Ok(())
}

pub(crate) fn drop_accounts_foreign_keys(conn: &Connection) -> Result<()> {
    for table in CHILD_TABLES_WITH_ACCOUNTS_FK {
        rebuild_table_without_accounts_fk(conn, table)?;
    }
    Ok(())
}

fn rebuild_table_without_accounts_fk(conn: &Connection, table: &str) -> Result<()> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(());
    };
    if !sql.to_ascii_lowercase().contains("references accounts") {
        return Ok(());
    }
    let indexes: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT sql FROM sqlite_master
             WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL",
        )?;
        stmt.query_map([table], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let tmp = format!("{table}__v52");
    let created = rename_sqlite_create_table(&sql, table, &tmp);
    let created = strip_accounts_references(&created);
    conn.execute_batch(&created)?;
    conn.execute(&format!("INSERT INTO {tmp} SELECT * FROM {table}"), [])?;
    conn.execute(&format!("DROP TABLE {table}"), [])?;
    conn.execute(&format!("ALTER TABLE {tmp} RENAME TO {table}"), [])?;
    for index_sql in indexes {
        conn.execute_batch(&index_sql)?;
    }
    Ok(())
}

fn rename_sqlite_create_table(sql: &str, from: &str, to: &str) -> String {
    for (old, new) in [
        (
            format!("CREATE TABLE IF NOT EXISTS \"{from}\""),
            format!("CREATE TABLE IF NOT EXISTS \"{to}\""),
        ),
        (
            format!("CREATE TABLE IF NOT EXISTS {from}"),
            format!("CREATE TABLE IF NOT EXISTS {to}"),
        ),
        (
            format!("CREATE TABLE \"{from}\""),
            format!("CREATE TABLE \"{to}\""),
        ),
        (format!("CREATE TABLE {from}"), format!("CREATE TABLE {to}")),
    ] {
        if sql.contains(&old) {
            return sql.replacen(&old, &new, 1);
        }
    }
    sql.replace(from, to)
}

fn strip_accounts_references(sql: &str) -> String {
    let mut out = sql.to_string();
    for needle in [
        " REFERENCES accounts(id) ON DELETE CASCADE",
        " REFERENCES accounts(id) ON DELETE RESTRICT",
        " REFERENCES \"accounts\"(id) ON DELETE CASCADE",
        " REFERENCES \"accounts\"(id)",
        " REFERENCES accounts(id)",
        ", FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE",
        "FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE,",
        "FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE",
        ", FOREIGN KEY (account_id)",
        "FOREIGN KEY (account_id),",
        "FOREIGN KEY (account_id)",
    ] {
        out = out.replace(needle, "");
    }
    while let Some(idx) = out.rfind(',') {
        let after = out[idx + 1..].trim_start();
        if after.starts_with(')') {
            out.replace_range(idx..idx + 1, "");
            continue;
        }
        break;
    }
    out
}

pub(crate) fn drop_accounts_table(conn: &Connection) -> Result<()> {
    if table_exists(conn, "accounts")? {
        conn.execute_batch("DROP TABLE accounts;")?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct CredentialRowExtras {
    pub legacy_account_id: String,
    pub key_cipher: String,
    pub password_cipher: Option<String>,
    pub username: Option<String>,
    pub referral_code: Option<String>,
    pub cooldown_until: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub auth_error: Option<String>,
    pub account_type: Option<String>,
    pub setup_step: Option<String>,
    pub provider_id: Option<String>,
    pub credential_kind: Option<String>,
    pub quota_scope: Option<String>,
    pub identity_id: Option<String>,
    pub verification_status: Option<String>,
    pub connection_verified_at: Option<String>,
    pub verification_error: Option<String>,
    pub usage_5h_window_started_at: Option<String>,
    pub usage_5h_window_cost_offset: Option<f64>,
    pub usage_week_window_started_at: Option<String>,
    pub usage_week_window_cost_offset: Option<f64>,
    pub usage_month_window_cost_offset: Option<f64>,
    pub group_json: Option<String>,
    pub link_version: Option<i64>,
    pub link_snapshot: Option<String>,
    pub identity_confidence: Option<String>,
    pub authority_site: Option<String>,
    pub authority_subject: Option<String>,
    pub identity_enabled: Option<i32>,
    pub identity_label: Option<String>,
    pub identity_notes: Option<String>,
    pub credential_version: Option<i64>,
    pub auth_state_version: Option<i64>,
    pub rotated_at: Option<String>,
    pub binding_id: Option<String>,
    pub binding_enabled: Option<i32>,
    pub subscription_source: Option<String>,
    pub subscription_expires_on: Option<String>,
    pub grants_initialized: Option<i32>,
}

pub(crate) fn snapshot_credential_extras(
    conn: &Connection,
) -> Result<HashMap<String, CredentialRowExtras>> {
    if !table_exists(conn, "credentials")? {
        return Ok(HashMap::new());
    }
    if !table_has_column(conn, "credentials", "key_cipher")? {
        return Ok(HashMap::new());
    }
    let has_v52 = table_has_column(conn, "credentials", "provider_id")?;
    let has_link = table_has_column(conn, "credentials", "group_json")?;
    let has_v57 = table_has_column(conn, "credentials", "binding_id")?;
    let sql = if has_v52 && has_link && has_v57 {
        "SELECT legacy_account_id, key_cipher, password_cipher, username, referral_code,
                cooldown_until, created_at, updated_at, auth_error, account_type, setup_step,
                provider_id, credential_kind, quota_scope, identity_id, verification_status,
                connection_verified_at, verification_error, usage_5h_window_started_at,
                usage_5h_window_cost_offset, usage_week_window_started_at,
                usage_week_window_cost_offset, usage_month_window_cost_offset,
                group_json, link_version, link_snapshot,
                identity_confidence, authority_site, authority_subject, identity_enabled,
                identity_label, identity_notes, credential_version, auth_state_version,
                rotated_at, binding_id, binding_enabled, subscription_source,
                subscription_expires_on, grants_initialized
         FROM credentials"
    } else if has_v52 && has_link {
        "SELECT legacy_account_id, key_cipher, password_cipher, username, referral_code,
                cooldown_until, created_at, updated_at, auth_error, account_type, setup_step,
                provider_id, credential_kind, quota_scope, identity_id, verification_status,
                connection_verified_at, verification_error, usage_5h_window_started_at,
                usage_5h_window_cost_offset, usage_week_window_started_at,
                usage_week_window_cost_offset, usage_month_window_cost_offset,
                group_json, link_version, link_snapshot,
                NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL
         FROM credentials"
    } else if has_v52 {
        "SELECT legacy_account_id, key_cipher, password_cipher, username, referral_code,
                cooldown_until, created_at, updated_at, auth_error, account_type, setup_step,
                provider_id, credential_kind, quota_scope, identity_id, verification_status,
                connection_verified_at, verification_error, usage_5h_window_started_at,
                usage_5h_window_cost_offset, usage_week_window_started_at,
                usage_week_window_cost_offset, usage_month_window_cost_offset,
                NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL
         FROM credentials"
    } else {
        "SELECT legacy_account_id, key_cipher, password_cipher,
                NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, NULL, NULL, NULL, NULL
         FROM credentials"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(CredentialRowExtras {
            legacy_account_id: row.get(0)?,
            key_cipher: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            password_cipher: row.get(2)?,
            username: row.get(3)?,
            referral_code: row.get(4)?,
            cooldown_until: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            auth_error: row.get(8)?,
            account_type: row.get(9)?,
            setup_step: row.get(10)?,
            provider_id: row.get(11)?,
            credential_kind: row.get(12)?,
            quota_scope: row.get(13)?,
            identity_id: row.get(14)?,
            verification_status: row.get(15)?,
            connection_verified_at: row.get(16)?,
            verification_error: row.get(17)?,
            usage_5h_window_started_at: row.get(18)?,
            usage_5h_window_cost_offset: row.get(19)?,
            usage_week_window_started_at: row.get(20)?,
            usage_week_window_cost_offset: row.get(21)?,
            usage_month_window_cost_offset: row.get(22)?,
            group_json: row.get(23)?,
            link_version: row.get(24)?,
            link_snapshot: row.get(25)?,
            identity_confidence: row.get(26)?,
            authority_site: row.get(27)?,
            authority_subject: row.get(28)?,
            identity_enabled: row.get(29)?,
            identity_label: row.get(30)?,
            identity_notes: row.get(31)?,
            credential_version: row.get(32)?,
            auth_state_version: row.get(33)?,
            rotated_at: row.get(34)?,
            binding_id: row.get(35)?,
            binding_enabled: row.get(36)?,
            subscription_source: row.get(37)?,
            subscription_expires_on: row.get(38)?,
            grants_initialized: row.get(39)?,
        })
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let extra = row?;
        map.insert(extra.legacy_account_id.clone(), extra);
    }
    Ok(map)
}

pub(crate) fn restore_credential_extras(
    conn: &Connection,
    extras: &HashMap<String, CredentialRowExtras>,
) -> Result<()> {
    if extras.is_empty() || !table_has_column(conn, "credentials", "key_cipher")? {
        return Ok(());
    }
    let has_v52 = table_has_column(conn, "credentials", "provider_id")?;
    let has_link = table_has_column(conn, "credentials", "group_json")?;
    let has_v57 = table_has_column(conn, "credentials", "binding_id")?;
    for extra in extras.values() {
        if has_v52 {
            conn.execute(
                "UPDATE credentials SET
                    key_cipher = ?2,
                    password_cipher = ?3,
                    username = COALESCE(?4, username),
                    referral_code = COALESCE(?5, referral_code),
                    cooldown_until = COALESCE(?6, cooldown_until),
                    created_at = COALESCE(?7, created_at),
                    updated_at = COALESCE(?8, updated_at),
                    auth_error = COALESCE(?9, auth_error),
                    account_type = COALESCE(?10, account_type),
                    setup_step = COALESCE(?11, setup_step),
                    provider_id = COALESCE(?12, provider_id),
                    credential_kind = COALESCE(?13, credential_kind),
                    quota_scope = COALESCE(?14, quota_scope),
                    identity_id = COALESCE(?15, identity_id),
                    verification_status = COALESCE(?16, verification_status),
                    connection_verified_at = COALESCE(?17, connection_verified_at),
                    verification_error = COALESCE(?18, verification_error),
                    usage_5h_window_started_at = COALESCE(?19, usage_5h_window_started_at),
                    usage_5h_window_cost_offset = COALESCE(?20, usage_5h_window_cost_offset),
                    usage_week_window_started_at = COALESCE(?21, usage_week_window_started_at),
                    usage_week_window_cost_offset = COALESCE(?22, usage_week_window_cost_offset),
                    usage_month_window_cost_offset = COALESCE(?23, usage_month_window_cost_offset)
                 WHERE legacy_account_id = ?1",
                params![
                    extra.legacy_account_id,
                    extra.key_cipher,
                    extra.password_cipher,
                    extra.username,
                    extra.referral_code,
                    extra.cooldown_until,
                    extra.created_at,
                    extra.updated_at,
                    extra.auth_error,
                    extra.account_type,
                    extra.setup_step,
                    extra.provider_id,
                    extra.credential_kind,
                    extra.quota_scope,
                    extra.identity_id,
                    extra.verification_status,
                    extra.connection_verified_at,
                    extra.verification_error,
                    extra.usage_5h_window_started_at,
                    extra.usage_5h_window_cost_offset,
                    extra.usage_week_window_started_at,
                    extra.usage_week_window_cost_offset,
                    extra.usage_month_window_cost_offset,
                ],
            )?;
            if has_link {
                conn.execute(
                    "UPDATE credentials
                     SET group_json = ?2, link_version = ?3, link_snapshot = ?4
                     WHERE legacy_account_id = ?1",
                    params![
                        extra.legacy_account_id,
                        extra.group_json,
                        extra.link_version,
                        extra.link_snapshot,
                    ],
                )?;
            }
            if has_v57 {
                conn.execute(
                    "UPDATE credentials SET
                        identity_confidence = COALESCE(?2, identity_confidence),
                        authority_site = COALESCE(?3, authority_site),
                        authority_subject = COALESCE(?4, authority_subject),
                        identity_enabled = COALESCE(?5, identity_enabled),
                        identity_label = COALESCE(?6, identity_label),
                        identity_notes = COALESCE(?7, identity_notes),
                        credential_version = COALESCE(?8, credential_version),
                        auth_state_version = COALESCE(?9, auth_state_version),
                        rotated_at = COALESCE(?10, rotated_at),
                        binding_id = COALESCE(?11, binding_id),
                        binding_enabled = COALESCE(?12, binding_enabled),
                        subscription_source = COALESCE(?13, subscription_source),
                        subscription_expires_on = COALESCE(?14, subscription_expires_on),
                        grants_initialized = COALESCE(?15, grants_initialized)
                     WHERE legacy_account_id = ?1",
                    params![
                        extra.legacy_account_id,
                        extra.identity_confidence,
                        extra.authority_site,
                        extra.authority_subject,
                        extra.identity_enabled,
                        extra.identity_label,
                        extra.identity_notes,
                        extra.credential_version,
                        extra.auth_state_version,
                        extra.rotated_at,
                        extra.binding_id,
                        extra.binding_enabled,
                        extra.subscription_source,
                        extra.subscription_expires_on,
                        extra.grants_initialized,
                    ],
                )?;
            }
        } else {
            conn.execute(
                "UPDATE credentials SET key_cipher = ?2, password_cipher = ?3
                 WHERE legacy_account_id = ?1",
                params![
                    extra.legacy_account_id,
                    extra.key_cipher,
                    extra.password_cipher,
                ],
            )?;
        }
    }
    Ok(())
}

/// Recreate a late-schema `accounts` table from credentials so historical
/// rewind fixtures can still ALTER/UPDATE that table after v52.
#[allow(dead_code)]
pub(crate) fn materialize_legacy_accounts_for_rewind(conn: &Connection) -> Result<()> {
    if table_exists(conn, "accounts")? {
        return Ok(());
    }
    if !table_exists(conn, "credentials")? {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE accounts (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            username TEXT,
            password_cipher TEXT,
            key_cipher TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 1,
            referral_code TEXT,
            recharge_date TEXT,
            cooldown_until TEXT,
            cooldown_generic_until TEXT,
            cooldown_5h_until TEXT,
            cooldown_week_until TEXT,
            cooldown_month_until TEXT,
            cooldown_free_until TEXT,
            last_error TEXT,
            created_at TEXT,
            updated_at TEXT,
            auth_error TEXT,
            account_type TEXT,
            setup_step TEXT,
            notes TEXT,
            provider_id TEXT,
            credential_kind TEXT,
            quota_scope TEXT,
            sort_order INTEGER NOT NULL DEFAULT 0,
            identity_id TEXT,
            verification_status TEXT,
            connection_verified_at TEXT,
            verification_error TEXT,
            usage_5h_window_started_at TEXT,
            usage_5h_window_cost_offset REAL NOT NULL DEFAULT 0,
            usage_week_window_started_at TEXT,
            usage_week_window_cost_offset REAL NOT NULL DEFAULT 0,
            usage_month_window_cost_offset REAL NOT NULL DEFAULT 0
        );
        INSERT INTO accounts (
            id, name, username, password_cipher, key_cipher, enabled, referral_code,
            recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until,
            cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error,
            created_at, updated_at, auth_error, account_type, setup_step, notes,
            provider_id, credential_kind, quota_scope, sort_order, identity_id,
            verification_status, connection_verified_at, verification_error,
            usage_5h_window_started_at, usage_5h_window_cost_offset,
            usage_week_window_started_at, usage_week_window_cost_offset,
            usage_month_window_cost_offset
        )
        SELECT
            legacy_account_id, name, username, password_cipher, key_cipher, enabled,
            referral_code, purchase_date, cooldown_until, cooldown_generic_until,
            cooldown_5h_until, cooldown_week_until, cooldown_month_until,
            cooldown_free_until, last_error, created_at, updated_at, auth_error,
            account_type, setup_step, notes, provider_id, credential_kind, quota_scope,
            routing_rank, identity_id, verification_status, connection_verified_at,
            verification_error, usage_5h_window_started_at, usage_5h_window_cost_offset,
            usage_week_window_started_at, usage_week_window_cost_offset,
            usage_month_window_cost_offset
        FROM credentials;",
    )?;
    Ok(())
}
