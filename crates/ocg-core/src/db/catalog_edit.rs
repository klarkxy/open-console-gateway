//! Local additions use the same persisted snapshot as catalog deletion.
use super::*;

impl Database {
    pub(crate) fn add_contract_catalog_models(
        &self,
        scope: &ContractScope,
        model_ids: &[String],
        now: DateTime<Utc>,
    ) -> Result<()> {
        anyhow::ensure!(
            scope.kind_str() == SCOPE_KIND_PROVIDER
                && crate::provider_contracts::builtin_provider_scope_ids().contains(&scope.id()),
            "unknown built-in provider scope"
        );
        let tx = self
            .conn
            .is_autocommit()
            .then(|| self.conn.unchecked_transaction())
            .transpose()?;
        let current = load_scope_on(&self.conn, scope)?;
        let contracts = crate::provider_contracts::build_effective_contracts(
            &self.zen_free_model_catalog()?.unwrap_or_default(),
            &[],
            self.load_persisted_contracts()?,
        );
        let mut models = contracts
            .scope(scope)
            .map(|s| s.catalog.models.clone())
            .unwrap_or_default();
        let mut known: HashSet<String> = models.iter().map(|id| id.to_ascii_lowercase()).collect();
        anyhow::ensure!(
            !model_ids.is_empty() && models.len() + model_ids.len() <= 2000,
            "invalid catalog size"
        );
        for id in model_ids {
            anyhow::ensure!(
                !id.is_empty()
                    && id.len() <= 200
                    && !id.chars().any(|c| c.is_whitespace() || c.is_control()),
                "invalid model ID"
            );
            anyhow::ensure!(known.insert(id.to_ascii_lowercase()), "duplicate model ID");
            models.push(id.clone());
            // A local catalog declaration is not permission to send inference.
            for protocol in UpstreamProtocolKind::ALL {
                set_model_protocol_override_on(
                    &self.conn,
                    scope,
                    id,
                    protocol,
                    ProtocolOverrideState::ForceOff,
                    now,
                )?;
            }
        }
        upsert_contract_catalog_on(
            &self.conn,
            scope,
            &models,
            current.as_ref().and_then(|row| row.catalog_refreshed_at),
            current
                .as_ref()
                .map(|row| row.catalog_source.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("manual"),
            current
                .as_ref()
                .map(|row| row.catalog_source_url.as_str())
                .unwrap_or(""),
            now,
        )?;
        destination_store::refresh_builtin_catalog(self, scope)?;
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }
}

impl Database {
    /// One catalog row per upstream in sealed adapters; aliases rename that row.
    /// HTTP destinations retain their separate multi-mapping editing contract.
    pub(crate) fn edit_contract_catalog_model(
        &self,
        scope: &ContractScope,
        original_id: Option<&str>,
        mut model: ocg_domain::destination::CatalogModel,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let descriptor = crate::provider_contracts::provider_scope_descriptor(scope.id())
            .ok_or_else(|| anyhow::anyhow!("unknown built-in provider"))?;
        model.upstream_model = validate_custom_model_id(&model.upstream_model)?;
        model.public_model = if model.public_model.is_empty() {
            model.upstream_model.clone()
        } else {
            validate_custom_model_id(&model.public_model)?
        };
        let tx = self
            .conn
            .is_autocommit()
            .then(|| self.conn.unchecked_transaction())
            .transpose()?;
        let dest_id = destination_store::ensure_builtin_destination(&self.conn, scope.id())?;
        let mut catalog = destination_store::load_destination_catalog(&self.conn, &dest_id)?;
        let original = original_id
            .map(|id| {
                catalog
                    .iter()
                    .position(|m| m.upstream_model.eq_ignore_ascii_case(id))
                    .ok_or_else(|| anyhow::anyhow!("original catalog model no longer exists"))
            })
            .transpose()?;
        let go_ids = self
            .load_persisted_scope(&ContractScope::provider(OPENCODE_PROVIDER_ID))?
            .map(|s| s.catalog_models)
            .unwrap_or_default();
        let zen_ids = self
            .load_persisted_scope(&ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID))?
            .map(|s| s.catalog_models)
            .unwrap_or_default();
        for (index, other) in catalog.iter().enumerate() {
            if Some(index) == original {
                continue;
            }
            let effective_public = if other.public_model != other.upstream_model {
                other.public_model.clone()
            } else {
                crate::alias::canonical_alias_for_provider_model(
                    scope.id(),
                    &other.upstream_model,
                    &go_ids,
                    &zen_ids,
                )
            };
            anyhow::ensure!(
                effective_public.is_empty()
                    || !effective_public.eq_ignore_ascii_case(&model.public_model),
                "public model name already exists"
            );
            anyhow::ensure!(
                !other.public_model.eq_ignore_ascii_case(&model.public_model),
                "public model name already exists"
            );
            anyhow::ensure!(
                !other
                    .upstream_model
                    .eq_ignore_ascii_case(&model.upstream_model),
                "upstream model already exists; edit its existing mapping"
            );
        }
        anyhow::ensure!(
            catalog.len() < 2000 || original.is_some(),
            "catalog is full"
        );
        let persisted = self.load_persisted_contracts()?;
        let evidence = persisted
            .evidence
            .get(scope)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let allowed = crate::provider_contracts::admitted_protocols(
            descriptor.kind,
            descriptor.protocol_probe,
            &model.upstream_model,
            evidence,
        );
        let distinct: HashSet<_> = model.protocols.iter().collect();
        anyhow::ensure!(
            distinct.len() == model.protocols.len()
                && model.protocols.iter().all(|p| allowed.contains(p)),
            "selected protocol is outside the built-in adapter capability ceiling"
        );
        anyhow::ensure!(
            !model.enabled || !model.protocols.is_empty(),
            "enabled model requires at least one protocol"
        );
        anyhow::ensure!(
            model.preferred.is_none_or(|p| model.protocols.contains(&p)),
            "preferred protocol must be selected"
        );
        if model.preferred.is_none() {
            model.preferred = model.protocols.first().copied();
        }
        let current = load_scope_on(&self.conn, scope)?;
        let mut raw_ids = current
            .as_ref()
            .map(|s| s.catalog_models.clone())
            .unwrap_or_else(|| catalog.iter().map(|m| m.upstream_model.clone()).collect());
        if let Some(index) = original {
            let old = &catalog[index].upstream_model;
            if old != &model.upstream_model {
                raw_ids.retain(|id| !id.eq_ignore_ascii_case(old));
                purge_removed_catalog_model_on(&self.conn, scope, old)?;
            }
            catalog[index] = model.clone();
        } else {
            catalog.push(model.clone());
        }
        if !raw_ids.iter().any(|id| id == &model.upstream_model) {
            raw_ids.push(model.upstream_model.clone());
        }
        for protocol in UpstreamProtocolKind::ALL {
            let enabled = model.enabled && model.protocols.contains(&protocol);
            set_model_protocol_override_on(
                &self.conn,
                scope,
                &model.upstream_model,
                protocol,
                if enabled {
                    ProtocolOverrideState::ForceOn
                } else {
                    ProtocolOverrideState::ForceOff
                },
                now,
            )?;
        }
        if let Some(preferred) = model.preferred {
            set_model_protocol_preferences_on(
                &self.conn,
                scope,
                &[(model.upstream_model.clone(), preferred)],
            )?;
        }
        if model.preferred.is_none() {
            self.conn.execute("DELETE FROM provider_model_protocol_preferences WHERE provider_id = ?1 AND model_id = ?2 COLLATE NOCASE", params![scope.id(), model.upstream_model])?;
        }
        upsert_contract_catalog_on(
            &self.conn,
            scope,
            &raw_ids,
            current.as_ref().and_then(|s| s.catalog_refreshed_at),
            current
                .as_ref()
                .map(|s| s.catalog_source.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("manual"),
            current
                .as_ref()
                .map(|s| s.catalog_source_url.as_str())
                .unwrap_or(""),
            now,
        )?;
        destination_store::replace_destination_catalog(&self.conn, &dest_id, &catalog)?;
        if let Some(tx) = tx {
            tx.commit()?;
        }
        Ok(())
    }
}
