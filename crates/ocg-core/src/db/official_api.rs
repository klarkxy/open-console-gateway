//! Typed financial evidence in the existing price and balance tables.
use super::*;
use crate::official_api::{
    self, OfficialApiKind, OfficialBalance, OfficialPriceSheet, OfficialSpend,
};

impl Database {
    pub(crate) fn official_api_prices(
        &self,
        provider: &str,
        kind: OfficialApiKind,
    ) -> Result<OfficialPriceSheet> {
        match self.latest_provider_pricing_snapshot(provider)? {
            None => Ok(official_api::pricing::seed(kind)),
            Some(row) => {
                let sheet: OfficialPriceSheet = serde_json::from_str(&row.snapshot_json)?;
                anyhow::ensure!(
                    sheet.kind == kind
                        && row.source_url == sheet.source_url
                        && row.revision == sheet.revision,
                    "official price identity mismatch"
                );
                official_api::pricing::validate(&sheet)?;
                Ok(sheet)
            }
        }
    }

    pub(crate) fn store_official_api_prices(
        &self,
        runtime: &DynamicProviderRuntime,
        sheet: &OfficialPriceSheet,
    ) -> Result<()> {
        anyhow::ensure!(
            official_api::kind_for_runtime(runtime) == Some(sheet.kind),
            "official price provider mismatch"
        );
        official_api::pricing::validate(sheet)?;
        let tx = self.conn.unchecked_transaction()?;
        let current = get_dynamic_provider_on(&tx, &runtime.id)?
            .ok_or_else(|| anyhow::anyhow!("provider removed"))?;
        anyhow::ensure!(current == *runtime, "provider changed");
        tx.execute("INSERT OR IGNORE INTO provider_pricing_snapshots
            (provider_id,revision,activated_at,document_updated_at,source_url,content_hash,snapshot_json)
            VALUES (?1,?2,?3,?3,?4,?5,?6)", params![runtime.id, sheet.revision, sheet.observed_at.to_rfc3339(), sheet.source_url,
                sheet.revision.rsplit(':').next().unwrap_or_default(), serde_json::to_string(sheet)?])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn official_api_balances(
        &self,
        account: &Account,
        runtime: &DynamicProviderRuntime,
    ) -> Result<Vec<OfficialBalance>> {
        let source = official_api::balance_source(account, runtime);
        let rows = self.list_credit_balances(&account.id)?;
        let mut result = Vec::new();
        for currency in ["CNY", "USD"] {
            let part = |kind: &str| {
                rows.iter().find(|row| {
                    row.balance_kind == format!("official_{currency}_{kind}")
                        && row.source == source
                        && row.unit == currency
                })
            };
            if let (Some(total), Some(granted), Some(topped_up)) =
                (part("total"), part("granted"), part("topped_up"))
                && let Some(observed_at) = total.observed_at
                && granted.observed_at == Some(observed_at)
                && topped_up.observed_at == Some(observed_at)
                && [total.amount, granted.amount, topped_up.amount]
                    .iter()
                    .all(|v| v.is_finite())
            {
                result.push(OfficialBalance {
                    currency: currency.into(),
                    total: total.amount,
                    granted: granted.amount,
                    topped_up: topped_up.amount,
                    observed_at,
                });
            }
        }
        Ok(result)
    }

    pub(crate) fn store_official_api_balances(
        &self,
        account: &Account,
        runtime: &DynamicProviderRuntime,
        balances: &[OfficialBalance],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let cipher: String = tx.query_row(
            "SELECT key_cipher FROM accounts WHERE id=?1 AND provider_id=?2",
            params![account.id, runtime.id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(cipher == account.key_cipher, "credential changed");
        let source = official_api::balance_source(account, runtime);
        tx.execute(
            "DELETE FROM credit_balances WHERE account_id=?1 AND balance_kind LIKE 'official_%'",
            [&account.id],
        )?;
        for balance in balances {
            anyhow::ensure!(
                matches!(balance.currency.as_str(), "CNY" | "USD"),
                "unsupported balance currency"
            );
            for (kind, amount) in [
                ("total", balance.total),
                ("granted", balance.granted),
                ("topped_up", balance.topped_up),
            ] {
                anyhow::ensure!(amount.is_finite(), "invalid balance amount");
                tx.execute("INSERT INTO credit_balances(account_id,balance_kind,amount,unit,source,observed_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![account.id,format!("official_{}_{kind}",balance.currency),amount,balance.currency,source,balance.observed_at.to_rfc3339(),now.to_rfc3339()])?;
            }
        }
        tx.execute("UPDATE provider_usage_sync_state SET last_attempt_at=?1,last_success_at=?1 WHERE account_id=?2",params![now.to_rfc3339(),account.id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn official_api_spend(
        &self,
        account: &Account,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<(Vec<OfficialSpend>, u64)> {
        let mut stmt=self.conn.prepare("SELECT native_cost_currency,SUM(native_cost_value),COUNT(*) FROM forward_logs
            WHERE account_id=?1 AND provider_id=?2 AND julianday(timestamp)>=julianday(?3) AND julianday(timestamp)<=julianday(?4)
              AND status IN ('success','success_unpriced','success_no_usage') AND pricing_revision_id LIKE 'official-api:%'
              AND native_cost_value IS NOT NULL AND native_cost_currency IN ('CNY','USD')
            GROUP BY native_cost_currency ORDER BY native_cost_currency")?;
        let rows = stmt.query_map(
            params![
                account.id,
                account.provider_id,
                since.to_rfc3339(),
                until.to_rfc3339()
            ],
            |r| {
                Ok(OfficialSpend {
                    currency: r.get(0)?,
                    amount: r.get(1)?,
                    priced_requests: r.get(2)?,
                })
            },
        )?;
        let spend = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let unknown:u64=self.conn.query_row("SELECT COUNT(*) FROM forward_logs WHERE account_id=?1 AND provider_id=?2
            AND julianday(timestamp)>=julianday(?3) AND julianday(timestamp)<=julianday(?4)
            AND status IN ('success','success_unpriced','success_no_usage')
            AND (pricing_revision_id IS NULL OR pricing_revision_id NOT LIKE 'official-api:%' OR native_cost_value IS NULL)",params![account.id,account.provider_id,since.to_rfc3339(),until.to_rfc3339()],|r|r.get(0))?;
        Ok((spend, unknown))
    }
}
