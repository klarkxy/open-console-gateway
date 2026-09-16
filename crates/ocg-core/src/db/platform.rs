//! Parent ownership and observations for account-owned platform Keys.
use super::*;
use crate::platform::{
    PlatformAccount, PlatformGroup, PlatformKind, PlatformLink, PlatformSnapshot,
};

// A new association must not reuse a deleted row's refresh identity (ABA).
fn fresh_version() -> u64 {
    (uuid::Uuid::new_v4().as_u128() as u64) & 0x0000_FFFF_FFFF_FFFF
}

pub(super) fn migrate_to_v38(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
    if version >= 38 {
        return Ok(());
    }
    anyhow::ensure!(version == 37, "v38 requires schema v37");
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS platform_accounts (
        id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('new_api','sub2api')),
        name TEXT NOT NULL, base_url TEXT NOT NULL, credential_cipher TEXT,
        version INTEGER NOT NULL DEFAULT 1, snapshot TEXT);
        CREATE TABLE IF NOT EXISTS platform_links (
        account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
        platform_account_id TEXT NOT NULL REFERENCES platform_accounts(id) ON DELETE RESTRICT,
        group_json TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, snapshot TEXT);
        INSERT OR REPLACE INTO schema_version(version) VALUES(38);",
    )?;
    tx.commit()?;
    Ok(())
}

fn parent_row(row: &Row<'_>) -> rusqlite::Result<PlatformAccount> {
    let kind: String = row.get(1)?;
    let snapshot: Option<String> = row.get(6)?;
    Ok(PlatformAccount {
        id: row.get(0)?,
        kind: if kind == "new_api" {
            PlatformKind::NewApi
        } else {
            PlatformKind::Sub2api
        },
        name: row.get(2)?,
        base_url: row.get(3)?,
        has_user_credential: row.get(4)?,
        version: row.get(5)?,
        snapshot: snapshot
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(6, Type::Text, Box::new(e)))?,
    })
}

pub(super) fn merge_platforms_on(
    conn: &Connection,
    parents: &[crate::platform::PortablePlatformAccount],
    links: &[crate::platform::PortablePlatformLink],
    imported_account_ids: &HashSet<String>,
) -> Result<()> {
    for parent in parents {
        let kind = if parent.kind == PlatformKind::NewApi {
            "new_api"
        } else {
            "sub2api"
        };
        let existing: Option<(String, String)> = conn
            .query_row(
                "SELECT kind,base_url FROM platform_accounts WHERE id=?1",
                [&parent.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        anyhow::ensure!(
            existing
                .as_ref()
                .is_none_or(|(k, b)| k == kind && b == &parent.base_url),
            "imported platform identity conflicts with immutable origin"
        );
        conn.execute("INSERT INTO platform_accounts(id,kind,name,base_url,version) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET name=excluded.name,version=excluded.version,snapshot=NULL",params![parent.id,kind,parent.name,parent.base_url,fresh_version()])?;
        identity::persist_platform_identity(
            conn,
            &parent.id,
            &parent.name,
            &parent.base_url,
            Utc::now(),
        )?;
    }
    for link in links {
        let mut group = link.group.clone();
        group.verified = false;
        conn.execute("INSERT INTO platform_links(account_id,platform_account_id,group_json,version) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id) DO UPDATE SET platform_account_id=excluded.platform_account_id,group_json=excluded.group_json,version=excluded.version,snapshot=NULL",params![link.account_id,link.platform_account_id,serde_json::to_string(&group)?,fresh_version()])?;
        let parent_base: Option<String> = conn
            .query_row(
                "SELECT base_url FROM platform_accounts WHERE id=?1",
                [&link.platform_account_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(base_url) = parent_base {
            identity::update_account_identity_declaration(
                conn,
                &link.account_id,
                Some(&ocg_domain::credential::DeclaredPlatformRelation {
                    platform_account_id: link.platform_account_id.clone(),
                    group: identity::platform_group_label(&group),
                    parent_base_url: base_url,
                }),
                Utc::now(),
            )?;
        }
    }
    // Also restore materialization for destination links whose Key was imported from V4.
    let mut stmt=conn.prepare("SELECT l.account_id,p.base_url,COALESCE(c.upstream_protocol,''),a.provider_id FROM platform_links l JOIN platform_accounts p ON p.id=l.platform_account_id JOIN accounts a ON a.id=l.account_id LEFT JOIN account_custom_configs c ON c.account_id=l.account_id")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, base, protocol, provider) in rows {
        if !imported_account_ids.contains(&id.to_ascii_lowercase()) {
            continue;
        }
        anyhow::ensure!(
            is_custom_api(&provider),
            "linked account must remain Custom API"
        );
        let _protocol = UpstreamProtocolKind::try_from(protocol.as_str())?;
        let endpoint = crate::platform::hosted_endpoint(&base)?;
        conn.execute(
            "UPDATE account_custom_configs SET endpoint_url=?2 WHERE account_id=?1",
            params![id, endpoint],
        )?;
        conn.execute(
            "UPDATE platform_links SET snapshot=NULL,version=version+1 WHERE account_id=?1",
            [id],
        )?;
    }
    Ok(())
}

impl Database {
    pub(crate) fn platform_hosted_endpoint(&self, id: &str) -> Result<Option<String>> {
        let base: Option<String> = self.conn.query_row("SELECT p.base_url FROM platform_accounts p JOIN platform_links l ON l.platform_account_id=p.id WHERE l.account_id=?1",[id],|r|r.get(0)).optional()?;
        base.map(|b| crate::platform::hosted_endpoint(&b))
            .transpose()
    }
    pub fn list_platform_accounts(&self) -> Result<Vec<PlatformAccount>> {
        let mut stmt = self.conn.prepare("SELECT id,kind,name,base_url,credential_cipher IS NOT NULL,version,snapshot FROM platform_accounts ORDER BY rowid")?;
        Ok(stmt
            .query_map([], parent_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn platform_account(&self, id: &str) -> Result<Option<PlatformAccount>> {
        Ok(self.conn.query_row("SELECT id,kind,name,base_url,credential_cipher IS NOT NULL,version,snapshot FROM platform_accounts WHERE id=?1", [id], parent_row).optional()?)
    }

    pub(crate) fn platform_credential_cipher(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT credential_cipher FROM platform_accounts WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    pub(crate) fn create_platform_account(
        &self,
        id: &str,
        kind: PlatformKind,
        name: &str,
        base_url: &str,
        credential_cipher: Option<&str>,
    ) -> Result<()> {
        let base_url = crate::platform::validate_platform_base_url(base_url)?;
        anyhow::ensure!(
            !name.trim().is_empty() && name.len() <= 200,
            "invalid platform name"
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("INSERT INTO platform_accounts(id,kind,name,base_url,credential_cipher,version) VALUES(?1,?2,?3,?4,?5,?6)", params![id, if kind == PlatformKind::NewApi { "new_api" } else { "sub2api" }, name.trim(), base_url, credential_cipher, fresh_version()])?;
        identity::persist_platform_identity(&tx, id, name.trim(), &base_url, Utc::now())?;
        tx.commit()?;
        Ok(())
    }

    /// None preserves a credential; Some(None) explicitly clears it.
    pub(crate) fn update_platform_account(
        &self,
        id: &str,
        name: &str,
        credential: Option<Option<&str>>,
    ) -> Result<()> {
        anyhow::ensure!(
            !name.trim().is_empty() && name.len() <= 200,
            "invalid platform name"
        );
        let tx = self.conn.unchecked_transaction()?;
        let count = tx.execute(
            "UPDATE platform_accounts SET name=?2,version=version+1 WHERE id=?1",
            params![id, name.trim()],
        )?;
        anyhow::ensure!(count == 1, "platform account not found");
        let identity_id = ocg_domain::credential::identity_id_for_platform_account(id);
        tx.execute(
            "UPDATE upstream_identities SET label=?2, updated_at=?3 WHERE id=?1",
            params![identity_id.as_str(), name.trim(), Utc::now().to_rfc3339()],
        )?;
        if let Some(value) = credential {
            tx.execute(
                "UPDATE platform_accounts SET credential_cipher=?2,snapshot=NULL WHERE id=?1",
                params![id, value],
            )?;
            tx.execute("UPDATE platform_links SET snapshot=NULL,version=version+1 WHERE platform_account_id=?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn delete_platform_account(&self, id: &str) -> Result<()> {
        anyhow::ensure!(
            self.conn.query_row(
                "SELECT COUNT(*) FROM platform_links WHERE platform_account_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )? == 0,
            "unlink Keys before deleting the platform account"
        );
        let tx = self.conn.unchecked_transaction()?;
        anyhow::ensure!(
            tx.execute("DELETE FROM platform_accounts WHERE id=?1", [id])? == 1,
            "platform account not found"
        );
        identity::delete_platform_identity(&tx, id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_platform_links(&self) -> Result<Vec<PlatformLink>> {
        let mut stmt = self.conn.prepare("SELECT account_id,platform_account_id,group_json,snapshot FROM platform_links ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (account_id, platform_account_id, group, snapshot) = row?;
            Ok(PlatformLink {
                account_id,
                platform_account_id,
                group: serde_json::from_str(&group)?,
                snapshot: snapshot.map(|s| serde_json::from_str(&s)).transpose()?,
            })
        })
        .collect()
    }

    pub(crate) fn link_platform_account(
        &self,
        account_id: &str,
        parent_id: &str,
        group: &PlatformGroup,
    ) -> Result<()> {
        let account = self.get_account(account_id)?.context("account not found")?;
        anyhow::ensure!(
            is_custom_api(&account.provider_id),
            "only Custom API Keys can be linked"
        );
        let parent = self
            .platform_account(parent_id)?
            .context("platform account not found")?;
        let custom = self
            .account_custom_config(account_id)?
            .context("Custom configuration missing")?;
        let endpoint_url = crate::platform::hosted_endpoint(&parent.base_url)?;
        let mut group = group.clone();
        group.verified = false;
        group.subscription_type = None;
        anyhow::ensure!(
            group
                .id
                .as_ref()
                .is_none_or(|v| v.len() <= 200 && !v.chars().any(char::is_control))
                && group
                    .platform
                    .as_ref()
                    .is_none_or(|v| v.len() <= 64 && !v.chars().any(char::is_control))
                && group.auto_groups.len() <= 50
                && group
                    .auto_groups
                    .iter()
                    .all(|v| v.len() <= 200 && !v.chars().any(char::is_control)),
            "invalid group identity"
        );
        let tx = self.conn.unchecked_transaction()?;
        persist_account_custom_config_on(
            &tx,
            account_id,
            &AccountCustomConfigInput {
                endpoint_url,
                upstream_protocol: custom.upstream_protocol,
            },
        )?;
        tx.execute("INSERT INTO platform_links(account_id,platform_account_id,group_json,version) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id) DO UPDATE SET platform_account_id=excluded.platform_account_id,group_json=excluded.group_json,version=excluded.version,snapshot=NULL",params![account_id,parent_id,serde_json::to_string(&group)?,fresh_version()])?;
        identity::update_account_identity_declaration(
            &tx,
            account_id,
            Some(&ocg_domain::credential::DeclaredPlatformRelation {
                platform_account_id: parent_id.to_string(),
                group: identity::platform_group_label(&group),
                parent_base_url: parent.base_url,
            }),
            Utc::now(),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn unlink_platform_account(&self, account_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM platform_links WHERE account_id=?1",
            [account_id],
        )?;
        identity::update_account_identity_declaration(&tx, account_id, None, Utc::now())?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn platform_refresh_token(
        &self,
        parent_id: &str,
        account_id: Option<&str>,
    ) -> Result<String> {
        let parent = self
            .platform_account(parent_id)?
            .context("platform account not found")?;
        let mut token = format!("{}:{}", parent.id, parent.version);
        if let Some(id) = account_id {
            let version: i64 = self.conn.query_row(
                "SELECT version FROM platform_links WHERE account_id=?1 AND platform_account_id=?2",
                params![id, parent_id],
                |r| r.get(0),
            )?;
            let account = self.get_account(id)?.context("account not found")?;
            // Private comparison token; never returned or logged.
            token.push_str(&format!(":{id}:{version}:{}", account.key_cipher));
        }
        Ok(token)
    }

    pub(crate) fn save_platform_refresh(
        &self,
        parent_id: &str,
        account_id: Option<&str>,
        token: &str,
        snapshot: &PlatformSnapshot,
    ) -> Result<bool> {
        if self
            .platform_refresh_token(parent_id, account_id)
            .ok()
            .as_deref()
            != Some(token)
        {
            return Ok(false);
        }
        let previous = if let Some(id) = account_id {
            self.list_platform_links()?
                .into_iter()
                .find(|l| l.account_id == id)
                .and_then(|l| l.snapshot)
        } else {
            self.platform_account(parent_id)?.and_then(|p| p.snapshot)
        };
        let mut saved = snapshot.clone();
        if snapshot.stale
            && let Some(mut old) = previous
        {
            old.stale = true;
            old.errors = snapshot.errors.clone();
            saved = old;
        }
        let json = serde_json::to_string(&saved)?;
        if let Some(id) = account_id {
            self.conn.execute(
                "UPDATE platform_links SET snapshot=?2,version=version+1 WHERE account_id=?1",
                params![id, json],
            )?;
        } else {
            self.conn.execute(
                "UPDATE platform_accounts SET snapshot=?2,version=version+1 WHERE id=?1",
                params![parent_id, json],
            )?;
        }
        Ok(true)
    }
}
