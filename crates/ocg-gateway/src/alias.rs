//! Catalog-driven unified model alias registry.
//!
//! Outbound clients should send stable lowercase kebab-case aliases. The
//! original OpenCode Go protocol table defines its shared names. Other sealed
//! adapters contribute unique names from their saved catalogs. Saved Zen Free rows use
//! the official `-free` suffix: the original ID stays an exact raw pin, and
//! the suffix-stripped name is published as an Alias
//! (`muse-spark-1.3-contributor-free` → `muse-spark-1.3-contributor`).
//! Case-folded kebab spellings such as `GLM-5.2` are accepted. Names containing `/`, `_`,
//! or whitespace are treated as raw IDs and never folded onto a kebab alias
//! (`glm/5.2` is not `glm-5.2`). A raw upstream model ID is accepted only
//! when it uniquely selects one provider mapping or is also a declared shared
//! public name; other ambiguity returns
//! [`ResolveError::Ambiguous`] with code [`AMBIGUOUS_MODEL_ID`].
//!
//! Command Code GOAT rows generate unique catalog aliases from their leaf names
//! and join existing shared aliases where possible.
//! Known plan suffixes are stripped only when the shorter Alias is authorized;
//! explicit saved names override generated names. The unique
//! slash raw ID still pins to GOAT. Eligible Custom capabilities
//! overlay published aliases and resolve otherwise unknown IDs without
//! stealing Go/Zen mappings.
//! Later host adapters consume [`ProviderMapping`]: parse the client protocol
//! once, then materialize model / protocol / endpoint / auth per candidate.
//! Adapters must not probe a billable inference path to discover protocol
//! support. The OpenCode protocol table stays Go-specific.
//!
//! Items are rust-public only as the cross-crate bridge; the host crate's
//! `alias` compatibility facade keeps the historical public paths.

use ocg_domain::ids::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
    custom_model_id_matches, is_free_model, looks_raw_shaped,
};
use ocg_domain::protocol::supported_model_ids;
use ocg_domain::provider::is_custom_api;
use ocg_domain::zen::{ZenFreeModelCatalog, stripped_free_alias};
#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Machine-readable error code for a raw ID that matches more than one mapping.
pub const AMBIGUOUS_MODEL_ID: &str = "ambiguous_model_id";

/// One provider's upstream identity for a client-facing name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMapping {
    pub provider_id: String,
    pub upstream_model: String,
    /// Production-routeable mappings only. Reserved providers stay false.
    pub routeable: bool,
}

/// One provider's explicit public-to-upstream mappings.
///
/// Used by dynamic Providers and by the separate builtin alias overrides in
/// [`RuntimeCatalogs`]. Provider identity does not change the sealed adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraProviderCatalog {
    pub provider_id: String,
    pub mappings: Vec<(String, String)>,
}

/// Borrowed runtime catalog inputs used to overlay the sealed Alias registry.
///
/// Callers pass one value instead of extending resolver signatures whenever a
/// new static adapter contributes a catalog. The registry remains code-owned;
/// extra catalogs are a generic owned-string collection, not a plugin slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct RuntimeCatalogs<'a> {
    pub go: &'a [String],
    pub zen_free: &'a [String],
    pub custom: &'a [String],
    pub command_code: &'a [String],
    pub minimax: &'a [String],
    pub kimi: &'a [String],
    pub cpa: &'a [String],
    pub ollama: &'a [String],
    pub ollama_pinned: &'a [String],
    /// Explicit aliases for saved builtin rows, with exact upstream spelling.
    /// An override replaces only that provider/upstream's generated aliases.
    pub builtin_aliases: &'a [ExtraProviderCatalog],
    pub extra: &'a [ExtraProviderCatalog],
}

impl ProviderMapping {
    pub fn is_opencode_go(&self) -> bool {
        self.provider_id == OPENCODE_PROVIDER_ID
    }

    pub fn is_zen_free(&self) -> bool {
        self.provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID
    }

    pub fn is_command_code_goat(&self) -> bool {
        ocg_domain::provider::is_command_code_goat(&self.provider_id)
    }

    pub fn is_custom_api(&self) -> bool {
        is_custom_api(&self.provider_id)
    }

    pub fn is_minimax_cn(&self) -> bool {
        self.provider_id == MINIMAX_PROVIDER_ID
    }

    pub fn is_kimi_cn(&self) -> bool {
        self.provider_id == KIMI_PROVIDER_ID
    }

    pub fn is_cpa(&self) -> bool {
        self.provider_id == CPA_PROVIDER_ID
    }

    pub fn is_ollama_cloud(&self) -> bool {
        self.provider_id == OLLAMA_PROVIDER_ID
    }
}

/// A preferred client-facing alias and its provider mappings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub alias: String,
    pub mappings: Vec<ProviderMapping>,
}

/// Result of looking up a client-supplied model name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedModel {
    /// Preferred alias. May follow account order, sticky, and fallback across
    /// routeable mappings (including Zen prefer overlay).
    Alias {
        requested: String,
        alias: String,
        mappings: Vec<ProviderMapping>,
    },
    /// Raw upstream ID that uniquely selected one routeable mapping. Pinned to
    /// that provider; no cross-provider fallback or prefer overlay.
    PinnedRaw {
        requested: String,
        mapping: ProviderMapping,
    },
}

impl ResolvedModel {
    pub fn requested(&self) -> &str {
        match self {
            Self::Alias { requested, .. } | Self::PinnedRaw { requested, .. } => requested,
        }
    }

    pub fn is_pinned(&self) -> bool {
        matches!(self, Self::PinnedRaw { .. })
    }

    pub fn routeable_mappings(&self) -> Vec<&ProviderMapping> {
        match self {
            Self::Alias { mappings, .. } => mappings
                .iter()
                .filter(|mapping| mapping.routeable)
                .collect(),
            Self::PinnedRaw { mapping, .. } if mapping.routeable => vec![mapping],
            Self::PinnedRaw { .. } => Vec::new(),
        }
    }

    /// Alias requests may follow account order, sticky, and fallback.
    /// Unique raw IDs stay pinned to one provider mapping.
    pub fn allows_cross_account_fallback(&self) -> bool {
        matches!(self, Self::Alias { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    Unknown {
        requested: String,
    },
    Ambiguous {
        requested: String,
        mappings: Vec<ProviderMapping>,
    },
}

impl ResolveError {
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Ambiguous { .. } => Some(AMBIGUOUS_MODEL_ID),
            Self::Unknown { .. } => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Unknown { requested } => format!("unknown model `{requested}`"),
            Self::Ambiguous {
                requested,
                mappings,
            } => {
                let providers = mappings
                    .iter()
                    .map(|mapping| format!("{}:{}", mapping.provider_id, mapping.upstream_model))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{AMBIGUOUS_MODEL_ID}: requested model `{requested}` matches multiple provider mappings ({providers}); send a preferred alias instead of this raw id"
                )
            }
        }
    }
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();
#[cfg(test)]
thread_local! {
    static RUNTIME_REGISTRY_BUILDS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub fn take_runtime_registry_build_count() -> usize {
    RUNTIME_REGISTRY_BUILDS.with(|count| count.replace(0))
}

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(build_builtin_registry)
}

fn build_builtin_registry() -> Registry {
    build_registry(&ZenFreeModelCatalog::default().models)
}

fn build_registry(zen_free_models: &[String]) -> Registry {
    let mut specs = Vec::new();
    for id in supported_model_ids() {
        if id == "big-pickle" || is_free_model(id) {
            continue;
        } else {
            specs.push(AliasEntry {
                alias: id.to_string(),
                mappings: go_alias_mappings(id),
            });
        }
    }
    let mut registry = registry_from_entries(specs);
    for model in zen_free_models {
        if !is_free_model(model) {
            continue;
        }
        let mapping = zen_mapping(model);
        insert_raw_mapping(&mut registry, mapping.clone());
        if let Some(alias) = stripped_free_alias(model) {
            insert_mapping(&mut registry, alias, mapping);
        }
    }
    registry
}

fn validated_builtin_aliases(catalogs: RuntimeCatalogs<'_>) -> Vec<ExtraProviderCatalog> {
    catalogs
        .builtin_aliases
        .iter()
        .filter_map(|catalog| {
            let models = match catalog.provider_id.as_str() {
                OPENCODE_PROVIDER_ID => catalogs.go,
                OPENCODE_ZEN_FREE_PROVIDER_ID => catalogs.zen_free,
                COMMAND_CODE_PROVIDER_ID => catalogs.command_code,
                MINIMAX_PROVIDER_ID => catalogs.minimax,
                KIMI_PROVIDER_ID => catalogs.kimi,
                OLLAMA_PROVIDER_ID => catalogs.ollama,
                _ => return None,
            };
            let mappings = catalog
                .mappings
                .iter()
                .filter(|(public, upstream)| {
                    public != upstream && !public.trim().is_empty() && models.contains(upstream)
                })
                .cloned()
                .collect::<Vec<_>>();
            (!mappings.is_empty()).then(|| ExtraProviderCatalog {
                provider_id: catalog.provider_id.clone(),
                mappings,
            })
        })
        .collect()
}

fn build_runtime_registry(catalogs: RuntimeCatalogs<'_>) -> Registry {
    #[cfg(test)]
    RUNTIME_REGISTRY_BUILDS.with(|count| count.set(count.get() + 1));
    let overrides = validated_builtin_aliases(catalogs);
    let catalogs = RuntimeCatalogs {
        builtin_aliases: &overrides,
        ..catalogs
    };
    let mut registry = build_registry(catalogs.zen_free);
    insert_named_catalog(
        &mut registry,
        catalogs.minimax,
        minimax_mapping,
        catalogs.go,
        catalogs.builtin_aliases,
    );
    insert_named_catalog(
        &mut registry,
        catalogs.kimi,
        kimi_mapping,
        catalogs.go,
        catalogs.builtin_aliases,
    );
    insert_goat_catalog(
        &mut registry,
        catalogs.command_code,
        catalogs.go,
        catalogs.builtin_aliases,
    );
    insert_cpa_catalog(&mut registry, catalogs.cpa);
    insert_ollama_catalog(
        &mut registry,
        catalogs.ollama,
        catalogs.ollama_pinned,
        catalogs.go,
        catalogs.builtin_aliases,
    );
    insert_extra_catalogs(&mut registry, catalogs.extra);
    if !catalogs.builtin_aliases.is_empty() {
        for id in catalogs.go {
            if !is_free_model(id) {
                insert_raw_mapping(&mut registry, go_mapping(id));
            }
        }
    }
    replace_builtin_aliases(&mut registry, catalogs.builtin_aliases);
    join_cpa_shared_public_names(&mut registry, catalogs);
    registry
}

/// One runtime alias table plus the overlay lists `resolve` needs.
///
/// Building this table walks every saved catalog row. Callers that resolve or
/// advertise many names must reuse one index: [`resolve_with_runtime_catalogs`]
/// rebuilds it on every call.
pub struct RuntimeCatalogIndex {
    registry: Registry,
    go: Vec<String>,
    go_all: Vec<String>,
    command_code: Vec<String>,
    command_code_all: Vec<String>,
    custom: Vec<String>,
    extra: Vec<ExtraProviderCatalog>,
    builtin_aliases: Vec<ExtraProviderCatalog>,
}

impl RuntimeCatalogIndex {
    pub fn from_catalogs(catalogs: RuntimeCatalogs<'_>) -> Self {
        let builtin_aliases = validated_builtin_aliases(catalogs);
        let catalogs = RuntimeCatalogs {
            builtin_aliases: &builtin_aliases,
            ..catalogs
        };
        let registry = build_runtime_registry(catalogs);
        let go = catalogs
            .go
            .iter()
            .filter(|id| !has_builtin_override(catalogs, OPENCODE_PROVIDER_ID, id))
            .cloned()
            .collect();
        let command_code = catalogs
            .command_code
            .iter()
            .filter(|id| !has_builtin_override(catalogs, COMMAND_CODE_PROVIDER_ID, id))
            .cloned()
            .collect();
        Self {
            registry,
            go,
            go_all: catalogs.go.to_vec(),
            command_code,
            command_code_all: catalogs.command_code.to_vec(),
            custom: catalogs.custom.to_vec(),
            extra: catalogs.extra.to_vec(),
            builtin_aliases,
        }
    }

    fn catalogs(&self) -> RuntimeCatalogs<'_> {
        RuntimeCatalogs {
            go: &self.go_all,
            zen_free: &[],
            custom: &self.custom,
            command_code: &self.command_code_all,
            minimax: &[],
            kimi: &[],
            cpa: &[],
            ollama: &[],
            ollama_pinned: &[],
            extra: &self.extra,
            builtin_aliases: &self.builtin_aliases,
        }
    }

    pub fn resolve(&self, requested: &str) -> Result<ResolvedModel, ResolveError> {
        resolve_in_runtime(
            &self.registry,
            self.catalogs(),
            &self.go,
            &self.command_code,
            requested,
        )
    }

    pub fn published_models(&self) -> Vec<PublishedAlias> {
        published_routeable_from_index(self)
    }
}

fn resolve_in_runtime(
    registry: &Registry,
    catalogs: RuntimeCatalogs<'_>,
    go: &[String],
    command_code: &[String],
    requested: &str,
) -> Result<ResolvedModel, ResolveError> {
    let explicit_public = |provider: &str| {
        catalogs.builtin_aliases.iter().any(|catalog| {
            catalog.provider_id == provider && extra_public_hit(catalog, requested).is_some()
        })
    };
    let initial = resolve_in(registry, requested);
    if catalogs
        .builtin_aliases
        .iter()
        .any(|catalog| extra_public_hit(catalog, requested).is_some())
        && let Ok(ResolvedModel::Alias { mappings, .. }) = &initial
        && let Some(raw) = registry.raw_exact.get(requested.trim())
    {
        let conflicts = raw
            .iter()
            .filter(|mapping| !mappings.contains(mapping))
            .cloned()
            .collect::<Vec<_>>();
        let shadows_same_provider_raw = raw.iter().any(|raw| {
            mappings.iter().any(|mapping| {
                mapping.provider_id == raw.provider_id
                    && mapping.upstream_model != raw.upstream_model
            })
        });
        if shadows_same_provider_raw || !conflicts.is_empty() {
            let mut mappings = mappings.clone();
            mappings.extend(conflicts);
            return Err(ResolveError::Ambiguous {
                requested: requested.to_string(),
                mappings,
            });
        }
    }
    let go_resolved = match initial {
        Ok(resolved) if explicit_public(OPENCODE_PROVIDER_ID) => Ok(resolved),
        Ok(resolved) => overlay_go_catalog(resolved, go),
        Err(ResolveError::Unknown { requested }) => overlay_unknown_go(requested, go),
        other => other,
    };
    let custom_resolved = match go_resolved {
        Ok(resolved) => overlay_custom_catalog(resolved, catalogs.custom),
        Err(ResolveError::Unknown { requested }) => match catalogs
            .custom
            .iter()
            .find(|id| custom_model_id_matches(id, &requested))
        {
            Some(alias) => Ok(ResolvedModel::Alias {
                requested,
                alias: alias.clone(),
                mappings: vec![custom_mapping(CUSTOM_DYNAMIC_UPSTREAM)],
            }),
            None => Err(ResolveError::Unknown { requested }),
        },
        other => other,
    };
    let goat_resolved = match custom_resolved {
        Ok(resolved) if explicit_public(COMMAND_CODE_PROVIDER_ID) => Ok(resolved),
        Ok(resolved) => {
            let models = if resolved.is_pinned() {
                catalogs.command_code
            } else {
                command_code
            };
            overlay_goat_catalog(resolved, models, registry)
        }
        Err(ResolveError::Unknown { requested }) => {
            overlay_unknown_goat(requested, command_code, registry)
        }
        other => other,
    };
    let builtin_resolved = match goat_resolved {
        Ok(resolved) => overlay_extra_catalogs(resolved, catalogs.builtin_aliases, false),
        Err(ResolveError::Unknown { requested }) => {
            overlay_unknown_extra(requested, catalogs.builtin_aliases)
        }
        other => other,
    };
    match builtin_resolved {
        Ok(resolved) => overlay_extra_catalogs(resolved, catalogs.extra, true),
        Err(ResolveError::Unknown { requested }) => {
            overlay_unknown_extra(requested, catalogs.extra)
        }
        other => other,
    }
}

fn published_routeable_from_index(index: &RuntimeCatalogIndex) -> Vec<PublishedAlias> {
    let catalogs = index.catalogs();
    let mut published = published_routeable_in(&index.registry);
    for catalog in catalogs.builtin_aliases {
        for (public, _) in catalog
            .mappings
            .iter()
            .filter(|(public, _)| looks_raw_shaped(public))
        {
            if !published.iter().any(|item| item.alias == *public)
                && let Ok(resolved) = index.resolve(public)
                && let Some(mapping) = resolved.routeable_mappings().first()
            {
                published.push(PublishedAlias {
                    alias: public.clone(),
                    owned_by: mapping.provider_id.clone(),
                });
            }
        }
    }
    if !catalogs.builtin_aliases.is_empty() {
        published.retain(|item| {
            index
                .resolve(&item.alias)
                .is_ok_and(|resolved| !resolved.routeable_mappings().is_empty())
        });
    }
    for id in catalogs.go {
        if is_free_model(id)
            || has_builtin_override(catalogs, OPENCODE_PROVIDER_ID, id)
            || published.iter().any(|item| item.alias == *id)
        {
            continue;
        }
        if matches!(
            index.resolve(id),
            Ok(ResolvedModel::PinnedRaw { mapping, .. })
                if mapping.routeable && mapping.is_opencode_go() && mapping.upstream_model == *id
        ) {
            published.push(PublishedAlias {
                alias: id.clone(),
                owned_by: OPENCODE_PROVIDER_ID.to_string(),
            });
        }
    }
    published.sort_by(|left, right| left.alias.cmp(&right.alias));
    published
}

fn has_builtin_override(catalogs: RuntimeCatalogs<'_>, provider_id: &str, upstream: &str) -> bool {
    catalogs.builtin_aliases.iter().any(|catalog| {
        catalog.provider_id == provider_id
            && catalog.mappings.iter().any(|(_, model)| model == upstream)
    })
}

fn replace_builtin_aliases(registry: &mut Registry, overrides: &[ExtraProviderCatalog]) {
    for catalog in overrides {
        for (_, upstream) in &catalog.mappings {
            for entry in registry.aliases.values_mut() {
                entry.mappings.retain(|mapping| {
                    mapping.provider_id != catalog.provider_id
                        || mapping.upstream_model != *upstream
                });
            }
        }
    }
    registry
        .aliases
        .retain(|_, entry| !entry.mappings.is_empty());
    insert_extra_catalogs(registry, overrides);
}

/// A CPA ID that another saved catalog declares under the same public name
/// is a shared Alias. Raw-shaped IDs and differently named raw pins retain
/// their ambiguity checks.
fn join_cpa_shared_public_names(registry: &mut Registry, catalogs: RuntimeCatalogs<'_>) {
    for id in catalogs.cpa {
        if looks_raw_shaped(id) {
            continue;
        }
        let exact = |models: &[String]| models.iter().any(|model| model == id);
        let same_public_extra =
            catalogs
                .extra
                .iter()
                .chain(catalogs.builtin_aliases)
                .any(|catalog| {
                    catalog
                        .mappings
                        .iter()
                        .any(|(public, _)| public.eq_ignore_ascii_case(id))
                });
        let builtin = registry
            .raw_exact
            .get(id)
            .into_iter()
            .flatten()
            .filter(|mapping| !has_builtin_override(catalogs, &mapping.provider_id, id))
            .filter(|mapping| match mapping.provider_id.as_str() {
                COMMAND_CODE_PROVIDER_ID => exact(catalogs.command_code),
                MINIMAX_PROVIDER_ID => exact(catalogs.minimax),
                KIMI_PROVIDER_ID => exact(catalogs.kimi),
                OLLAMA_PROVIDER_ID => exact(catalogs.ollama),
                OPENCODE_ZEN_FREE_PROVIDER_ID => exact(catalogs.zen_free),
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        let same_public_go =
            exact(catalogs.go) && !has_builtin_override(catalogs, OPENCODE_PROVIDER_ID, id);
        if !same_public_extra && !same_public_go && builtin.is_empty() {
            continue;
        }
        if same_public_go {
            insert_mapping(registry, id, go_mapping(id));
        }
        for mapping in builtin {
            insert_mapping(registry, id, mapping);
        }
        insert_mapping(registry, id, cpa_mapping(id));
    }
}

fn insert_extra_catalogs(registry: &mut Registry, extras: &[ExtraProviderCatalog]) {
    for extra in extras {
        for (public_model, upstream_model) in &extra.mappings {
            let provider_mapping = mapping(&extra.provider_id, upstream_model, true);
            insert_raw_mapping(registry, provider_mapping.clone());
            if looks_raw_shaped(public_model) {
                // Raw-shaped public names stay exact request pins. A differing
                // upstream is preserved on the mapping and never published as a
                // second catalog id; overlay resolves the public name.
                continue;
            }
            insert_mapping(registry, public_model, provider_mapping);
        }
    }
}

fn insert_goat_catalog(
    registry: &mut Registry,
    model_ids: &[String],
    go_model_ids: &[String],
    overrides: &[ExtraProviderCatalog],
) {
    for model_id in model_ids {
        upsert_mapping(registry, None, goat_mapping(model_id, true));
    }
    for model_id in model_ids {
        let alias = command_alias_for_catalog(model_id, registry);
        let unique = model_ids
            .iter()
            .filter(|candidate| {
                command_alias_for_catalog(candidate, registry).eq_ignore_ascii_case(&alias)
            })
            .count()
            == 1;
        if goat_leaf_alias_is_publishable(&alias, unique, go_model_ids, registry)
            && !saved_name_belongs_to_another_model(
                overrides,
                COMMAND_CODE_PROVIDER_ID,
                model_id,
                &alias,
            )
        {
            upsert_mapping(registry, Some(&alias), goat_mapping(model_id, true));
        }
    }
}

// Discovery must not add a second target beneath an operator's saved name.
fn saved_name_belongs_to_another_model(
    overrides: &[ExtraProviderCatalog],
    provider_id: &str,
    upstream: &str,
    alias: &str,
) -> bool {
    overrides
        .iter()
        .filter(|catalog| catalog.provider_id == provider_id)
        .any(|catalog| {
            catalog
                .mappings
                .iter()
                .any(|(public, target)| public.eq_ignore_ascii_case(alias) && target != upstream)
        })
}

fn catalog_leaf(model_id: &str) -> String {
    model_id
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-")
}

fn insert_named_catalog(
    registry: &mut Registry,
    model_ids: &[String],
    mapping: fn(&str) -> ProviderMapping,
    go_model_ids: &[String],
    overrides: &[ExtraProviderCatalog],
) {
    let names = model_ids
        .iter()
        .map(|id| (id, catalog_leaf(id)))
        .collect::<Vec<_>>();
    for (model_id, alias) in &names {
        insert_raw_mapping(registry, mapping(model_id));
        let unique = names.iter().filter(|(_, other)| other == alias).count() == 1;
        if goat_leaf_alias_is_publishable(alias, unique, go_model_ids, registry)
            && !saved_name_belongs_to_another_model(
                overrides,
                &mapping(model_id).provider_id,
                model_id,
                alias,
            )
        {
            insert_mapping(registry, alias, mapping(model_id));
        }
    }
}

/// CPA catalog rows first join code-owned Aliases. Every row keeps its exact
/// upstream raw pin; later, exact shared public names may also become Aliases.
fn insert_cpa_catalog(registry: &mut Registry, model_ids: &[String]) {
    for model_id in model_ids {
        let mapping = cpa_mapping(model_id);
        let alias = code_owned_alias(registry, model_id);
        insert_raw_mapping(registry, mapping.clone());
        if let Some(alias) = alias {
            insert_mapping(registry, &alias, mapping);
        }
    }
}

fn mapping(provider_id: &str, upstream_model: &str, routeable: bool) -> ProviderMapping {
    ProviderMapping {
        provider_id: provider_id.to_string(),
        upstream_model: upstream_model.to_string(),
        routeable,
    }
}

fn go_mapping(upstream_model: &str) -> ProviderMapping {
    mapping(OPENCODE_PROVIDER_ID, upstream_model, true)
}

fn goat_deepseek_v4_flash_mapping() -> ProviderMapping {
    goat_mapping(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, false)
}

fn goat_mapping(upstream_model: &str, routeable: bool) -> ProviderMapping {
    mapping(COMMAND_CODE_PROVIDER_ID, upstream_model, routeable)
}

fn goat_catalog_hit(goat_model_ids: &[String], requested: &str) -> Option<String> {
    goat_model_ids
        .iter()
        .find(|id| id.trim() == requested.trim())
        .cloned()
}

fn goat_catalog_hit_for_resolved(
    goat_model_ids: &[String],
    resolved: &ResolvedModel,
    registry: &Registry,
) -> Option<String> {
    goat_catalog_hit(goat_model_ids, resolved.requested()).or_else(|| match resolved {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => mappings
            .iter()
            .filter(|mapping| mapping.is_command_code_goat())
            .find_map(|mapping| goat_catalog_hit(goat_model_ids, &mapping.upstream_model))
            .or_else(|| command_catalog_hit_for_alias(goat_model_ids, alias, registry)),
        ResolvedModel::PinnedRaw { mapping, .. } if mapping.is_command_code_goat() => {
            goat_catalog_hit(goat_model_ids, &mapping.upstream_model)
        }
        _ => None,
    })
}

const COMMAND_ALIAS_SUFFIX_EXCEPTIONS: &[&str] = &["-paid", "-free"];

fn command_catalog_hit_for_alias(
    goat_model_ids: &[String],
    alias: &str,
    registry: &Registry,
) -> Option<String> {
    let matches = goat_model_ids
        .iter()
        .filter(|id| command_alias_for_catalog(id, registry).eq_ignore_ascii_case(alias))
        .collect::<Vec<_>>();
    (matches.len() == 1).then(|| matches[0].clone())
}

fn go_alias_mappings(upstream_model: &'static str) -> Vec<ProviderMapping> {
    let mut mappings = vec![go_mapping(upstream_model)];
    if upstream_model == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS {
        mappings.push(goat_deepseek_v4_flash_mapping());
    }
    mappings
}

fn zen_mapping(upstream_model: &str) -> ProviderMapping {
    mapping(OPENCODE_ZEN_FREE_PROVIDER_ID, upstream_model, true)
}

fn custom_mapping(upstream_model: &str) -> ProviderMapping {
    mapping(CUSTOM_PROVIDER_ID, upstream_model, true)
}

fn minimax_mapping(upstream_model: &str) -> ProviderMapping {
    mapping(MINIMAX_PROVIDER_ID, upstream_model, true)
}

fn kimi_mapping(upstream_model: &str) -> ProviderMapping {
    mapping(KIMI_PROVIDER_ID, upstream_model, true)
}

fn cpa_mapping(upstream_model: &str) -> ProviderMapping {
    mapping(CPA_PROVIDER_ID, upstream_model, true)
}

fn ollama_mapping(upstream_model: &str, routeable: bool) -> ProviderMapping {
    mapping(OLLAMA_PROVIDER_ID, upstream_model, routeable)
}

/// Strip the trailing `:` tag from an Ollama Cloud catalog id
/// (`model:tag` → `model`). Ids without a tag keep their exact spelling;
/// the stem is only ever compared against code-owned alias stems, never
/// published on its own. Tag values are upstream runtime data: this function
/// must stay free of hardcoded snapshot ids.
fn ollama_catalog_stem(model_id: &str) -> &str {
    let trimmed = model_id.trim();
    match trimmed.rsplit_once(':') {
        Some((stem, tag)) if !stem.is_empty() && !tag.is_empty() => stem,
        _ => trimmed,
    }
}

/// Unique catalog stems become aliases. Coexisting tags keep distinct names;
/// one administrator-pinned tag may own the short stem. Exact IDs stay pins.
fn insert_ollama_catalog(
    registry: &mut Registry,
    model_ids: &[String],
    pinned_model_ids: &[String],
    go_model_ids: &[String],
    overrides: &[ExtraProviderCatalog],
) {
    let names = model_ids
        .iter()
        .map(|id| {
            let stem = ollama_catalog_stem(id);
            let matches = model_ids
                .iter()
                .filter(|other| ollama_catalog_stem(other).eq_ignore_ascii_case(stem))
                .collect::<Vec<_>>();
            let pinned = matches
                .iter()
                .filter(|other| pinned_model_ids.contains(**other))
                .collect::<Vec<_>>();
            let short = matches.len() == 1 || (pinned.len() == 1 && pinned[0].as_str() == id);
            let alias = if short {
                catalog_leaf(stem)
            } else {
                catalog_leaf(id).replace(':', "-")
            };
            (id, alias)
        })
        .collect::<Vec<_>>();
    for (id, alias) in &names {
        upsert_mapping(registry, None, ollama_mapping(id, true));
        let unique = names.iter().filter(|(_, other)| other == alias).count() == 1;
        if goat_leaf_alias_is_publishable(alias, unique, go_model_ids, registry)
            && !saved_name_belongs_to_another_model(overrides, OLLAMA_PROVIDER_ID, id, alias)
        {
            insert_mapping(registry, alias, ollama_mapping(id, true));
        }
    }
}

/// Sentinel upstream id for Custom-only resolutions. Per-candidate materialization
/// uses the account's declared capability ID instead of this value.
pub const CUSTOM_DYNAMIC_UPSTREAM: &str = "";

struct Registry {
    aliases: BTreeMap<String, AliasEntry>,
    /// Exact upstream model ID → every mapping that uses it.
    raw_exact: BTreeMap<String, Vec<ProviderMapping>>,
}

fn registry_from_entries(entries: Vec<AliasEntry>) -> Registry {
    let mut aliases = BTreeMap::new();
    let mut raw_exact: BTreeMap<String, Vec<ProviderMapping>> = BTreeMap::new();
    for entry in entries {
        debug_assert!(
            !looks_raw_shaped(&entry.alias),
            "published aliases must be kebab-case without slash, space, or underscore"
        );
        for mapping in &entry.mappings {
            raw_exact
                .entry(mapping.upstream_model.to_string())
                .or_default()
                .push(mapping.clone());
        }
        aliases.insert(entry.alias.to_lowercase(), entry);
    }
    Registry { aliases, raw_exact }
}

fn insert_mapping(registry: &mut Registry, alias: &str, mapping: ProviderMapping) {
    let key = alias.to_ascii_lowercase();
    let entry = registry.aliases.entry(key).or_insert_with(|| AliasEntry {
        alias: alias.to_string(),
        mappings: Vec::new(),
    });
    if !entry.mappings.contains(&mapping) {
        entry.mappings.push(mapping.clone());
    }
    insert_raw_mapping(registry, mapping);
}

fn upsert_mapping(registry: &mut Registry, alias: Option<&str>, mapping: ProviderMapping) {
    let same_identity = |existing: &ProviderMapping| {
        existing.provider_id == mapping.provider_id
            && existing.upstream_model == mapping.upstream_model
    };
    if let Some(alias) = alias {
        let key = alias.to_ascii_lowercase();
        let entry = registry.aliases.entry(key).or_insert_with(|| AliasEntry {
            alias: alias.to_string(),
            mappings: Vec::new(),
        });
        if let Some(existing) = entry.mappings.iter_mut().find(|item| same_identity(item)) {
            *existing = mapping.clone();
        } else {
            entry.mappings.push(mapping.clone());
        }
    }
    let mappings = registry
        .raw_exact
        .entry(mapping.upstream_model.clone())
        .or_default();
    if let Some(existing) = mappings.iter_mut().find(|item| same_identity(item)) {
        *existing = mapping;
    } else {
        mappings.push(mapping);
    }
}

fn insert_raw_mapping(registry: &mut Registry, mapping: ProviderMapping) {
    let mappings = registry
        .raw_exact
        .entry(mapping.upstream_model.clone())
        .or_default();
    if !mappings.contains(&mapping) {
        mappings.push(mapping);
    }
}

fn pin_or_ambiguous(
    requested: String,
    mappings: &[ProviderMapping],
) -> Result<ResolvedModel, ResolveError> {
    match mappings {
        [mapping] => Ok(ResolvedModel::PinnedRaw {
            requested,
            mapping: mapping.clone(),
        }),
        [] => Err(ResolveError::Unknown { requested }),
        _ => Err(ResolveError::Ambiguous {
            requested,
            mappings: mappings.to_vec(),
        }),
    }
}

/// Resolve a client-supplied model name against the builtin registry.
pub fn resolve(requested: &str) -> Result<ResolvedModel, ResolveError> {
    resolve_in(registry(), requested)
}

/// Resolve one client model against all runtime catalog inputs.
///
/// Catalog rows activate generated aliases or exact raw pins. Explicit builtin
/// aliases replace only their own generated mappings and retain raw ambiguity checks.
/// Prefer [`RuntimeCatalogIndex`] when resolving more than one name against the
/// same catalogs.
pub fn resolve_with_runtime_catalogs(
    requested: &str,
    catalogs: RuntimeCatalogs<'_>,
) -> Result<ResolvedModel, ResolveError> {
    RuntimeCatalogIndex::from_catalogs(catalogs).resolve(requested)
}

fn extra_mapping(extra: &ExtraProviderCatalog, upstream_model: &str) -> ProviderMapping {
    mapping(&extra.provider_id, upstream_model, true)
}

fn extra_public_hit<'a>(
    extra: &'a ExtraProviderCatalog,
    requested: &str,
) -> Option<&'a (String, String)> {
    extra
        .mappings
        .iter()
        .find(|(public_model, _upstream_model)| custom_model_id_matches(public_model, requested))
}

fn extra_raw_only_hit<'a>(
    extra: &'a ExtraProviderCatalog,
    requested: &str,
) -> Option<&'a (String, String)> {
    extra
        .mappings
        .iter()
        .find(|(public_model, upstream_model)| {
            upstream_model.trim() == requested.trim()
                && !custom_model_id_matches(public_model, requested)
        })
}

fn overlay_extra_catalogs(
    resolved: ResolvedModel,
    extras: &[ExtraProviderCatalog],
    replace_provider_mapping: bool,
) -> Result<ResolvedModel, ResolveError> {
    let mut current = resolved;
    for extra in extras {
        current = overlay_one_extra(current, extra, replace_provider_mapping)?;
    }
    Ok(current)
}

fn overlay_one_extra(
    resolved: ResolvedModel,
    extra: &ExtraProviderCatalog,
    replace_provider_mapping: bool,
) -> Result<ResolvedModel, ResolveError> {
    let requested_name = resolved.requested();
    let public_hit = extra_public_hit(extra, requested_name);
    let raw_only_hit = extra_raw_only_hit(extra, requested_name);
    if let Some((_, upstream_model)) = raw_only_hit {
        let raw_mapping = extra_mapping(extra, upstream_model);
        // A renamed builtin may have used its exact upstream as a generated
        // shared alias. Keep the other providers on that existing name; the
        // rename removes this mapping, not the shared alias's authority.
        let remaining_generated_alias = !replace_provider_mapping
            && matches!(&resolved, ResolvedModel::Alias { alias, .. }
                if *alias == canonical_alias_for_provider_model(
                    &extra.provider_id, upstream_model, &[], &[]));
        let conflicts = match &resolved {
            ResolvedModel::Alias { mappings, .. } => {
                !remaining_generated_alias && !mappings.contains(&raw_mapping)
            }
            ResolvedModel::PinnedRaw { mapping, .. } => mapping != &raw_mapping,
        };
        if conflicts {
            let (requested, mut mappings) = match resolved {
                ResolvedModel::Alias {
                    requested,
                    mappings,
                    ..
                } => (requested, mappings),
                ResolvedModel::PinnedRaw { requested, mapping } => (requested, vec![mapping]),
            };
            mappings.push(raw_mapping);
            return Err(ResolveError::Ambiguous {
                requested,
                mappings,
            });
        }
    }
    let Some((_, upstream_model)) = public_hit else {
        return Ok(resolved);
    };
    let replacement = extra_mapping(extra, upstream_model);
    match resolved {
        ResolvedModel::Alias {
            requested,
            alias,
            mut mappings,
        } => {
            if replace_provider_mapping
                && let Some(existing) = mappings
                    .iter_mut()
                    .find(|mapping| mapping.provider_id.eq_ignore_ascii_case(&extra.provider_id))
            {
                *existing = replacement;
            } else if !mappings.contains(&replacement) {
                mappings.push(replacement);
            }
            Ok(ResolvedModel::Alias {
                requested,
                alias,
                mappings,
            })
        }
        ResolvedModel::PinnedRaw { requested, mapping } if mapping == replacement => {
            Ok(ResolvedModel::PinnedRaw {
                requested,
                mapping: replacement,
            })
        }
        ResolvedModel::PinnedRaw { requested, mapping } => Err(ResolveError::Ambiguous {
            requested,
            mappings: vec![mapping, replacement],
        }),
    }
}

fn overlay_unknown_extra(
    requested: String,
    extras: &[ExtraProviderCatalog],
) -> Result<ResolvedModel, ResolveError> {
    let mut public_hits = Vec::new();
    let mut raw_hits = Vec::new();
    for extra in extras {
        for (public_model, upstream_model) in &extra.mappings {
            if custom_model_id_matches(public_model, &requested) {
                public_hits.push((public_model.clone(), extra_mapping(extra, upstream_model)));
            }
            if upstream_model.trim() == requested.trim() {
                raw_hits.push(extra_mapping(extra, upstream_model));
            }
        }
    }
    let exact_raw = raw_hits
        .iter()
        .any(|mapping| mapping.upstream_model.trim() == requested.trim());
    if exact_raw && (looks_raw_shaped(&requested) || public_hits.is_empty()) {
        return pin_or_ambiguous(requested, &raw_hits);
    }
    if !public_hits.is_empty() {
        let alias = public_hits[0].0.clone();
        return Ok(ResolvedModel::Alias {
            requested,
            alias,
            mappings: public_hits
                .into_iter()
                .map(|(_, mapping)| mapping)
                .collect(),
        });
    }
    Err(ResolveError::Unknown { requested })
}

fn overlay_go_catalog(
    resolved: ResolvedModel,
    go_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    let Some(canonical) = provider_catalog_hit_for_resolved(
        go_model_ids,
        &resolved,
        |mapping| mapping.is_opencode_go(),
        go_catalog_alias,
    ) else {
        return Ok(resolved);
    };
    overlay_known_provider(resolved, canonical, go_mapping)
}

fn overlay_unknown_go(
    requested: String,
    go_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    overlay_unknown_provider(requested, go_model_ids, go_mapping)
}

fn overlay_custom_catalog(
    resolved: ResolvedModel,
    custom_model_ids: &[String],
) -> Result<ResolvedModel, ResolveError> {
    let custom_hit = custom_model_ids
        .iter()
        .any(|id| custom_model_id_matches(id, resolved.requested()));
    if !custom_hit {
        return Ok(resolved);
    }
    match resolved {
        ResolvedModel::Alias {
            requested,
            alias,
            mut mappings,
        } => {
            if !mappings.iter().any(|mapping| mapping.is_custom_api()) {
                mappings.push(custom_mapping(&alias));
            }
            Ok(ResolvedModel::Alias {
                requested,
                alias,
                mappings,
            })
        }
        ResolvedModel::PinnedRaw { requested, mapping } if !mapping.is_custom_api() => {
            Err(ResolveError::Ambiguous {
                requested,
                mappings: vec![mapping, custom_mapping(CUSTOM_DYNAMIC_UPSTREAM)],
            })
        }
        other => Ok(other),
    }
}

fn provider_catalog_hit_for_resolved(
    model_ids: &[String],
    resolved: &ResolvedModel,
    owns_mapping: impl Fn(&ProviderMapping) -> bool,
    alias_for_model: fn(&str) -> String,
) -> Option<String> {
    provider_catalog_hit(model_ids, resolved.requested()).or_else(|| match resolved {
        ResolvedModel::Alias {
            alias, mappings, ..
        } => provider_catalog_hit_by_alias(model_ids, alias, alias_for_model).or_else(|| {
            mappings
                .iter()
                .filter(|mapping| owns_mapping(mapping))
                .find_map(|mapping| provider_catalog_hit(model_ids, &mapping.upstream_model))
        }),
        ResolvedModel::PinnedRaw { mapping, .. } if owns_mapping(mapping) => {
            provider_catalog_hit(model_ids, &mapping.upstream_model)
        }
        _ => None,
    })
}

fn provider_catalog_hit_by_alias(
    model_ids: &[String],
    alias: &str,
    alias_for_model: fn(&str) -> String,
) -> Option<String> {
    model_ids
        .iter()
        .find(|id| !looks_raw_shaped(id) && alias_for_model(id).eq_ignore_ascii_case(alias))
        .cloned()
}

fn provider_catalog_hit(model_ids: &[String], requested: &str) -> Option<String> {
    model_ids
        .iter()
        .find(|id| id.trim() == requested.trim())
        .cloned()
}

fn overlay_unknown_provider(
    requested: String,
    model_ids: &[String],
    mapping: impl Fn(&str) -> ProviderMapping,
) -> Result<ResolvedModel, ResolveError> {
    let Some(canonical) = provider_catalog_hit(model_ids, &requested) else {
        return Err(ResolveError::Unknown { requested });
    };
    Ok(ResolvedModel::PinnedRaw {
        requested,
        mapping: mapping(&canonical),
    })
}

fn overlay_known_provider(
    resolved: ResolvedModel,
    canonical: String,
    mapping: impl Fn(&str) -> ProviderMapping,
) -> Result<ResolvedModel, ResolveError> {
    match resolved {
        ResolvedModel::Alias {
            requested,
            alias,
            mut mappings,
        } => {
            if let Some(existing) = mappings.iter_mut().find(|existing| {
                let replacement = mapping(&canonical);
                existing.provider_id == replacement.provider_id
            }) {
                *existing = mapping(&canonical);
            } else {
                mappings.push(mapping(&canonical));
            }
            Ok(ResolvedModel::Alias {
                requested,
                alias,
                mappings,
            })
        }
        ResolvedModel::PinnedRaw {
            requested,
            mapping: existing,
        } => {
            let replacement = mapping(&canonical);
            if existing.provider_id == replacement.provider_id {
                Ok(ResolvedModel::PinnedRaw {
                    requested,
                    mapping: replacement,
                })
            } else {
                Err(ResolveError::Ambiguous {
                    requested,
                    mappings: vec![existing, replacement],
                })
            }
        }
    }
}

fn overlay_goat_catalog(
    resolved: ResolvedModel,
    goat_model_ids: &[String],
    registry: &Registry,
) -> Result<ResolvedModel, ResolveError> {
    let Some(canonical) = goat_catalog_hit_for_resolved(goat_model_ids, &resolved, registry) else {
        return Ok(match resolved {
            ResolvedModel::PinnedRaw { requested, mapping }
                if mapping.is_command_code_goat() && mapping.routeable =>
            {
                ResolvedModel::PinnedRaw {
                    requested,
                    mapping: goat_mapping(&mapping.upstream_model, false),
                }
            }
            other => other,
        });
    };
    overlay_known_goat(resolved, canonical)
}

fn overlay_unknown_goat(
    requested: String,
    goat_model_ids: &[String],
    registry: &Registry,
) -> Result<ResolvedModel, ResolveError> {
    let Some(canonical) = goat_catalog_hit(goat_model_ids, &requested).or_else(|| {
        command_catalog_hit_for_alias(goat_model_ids, &requested.to_ascii_lowercase(), registry)
    }) else {
        return Err(ResolveError::Unknown { requested });
    };
    let alias = command_alias_for_catalog(&canonical, registry);
    if looks_raw_shaped(&requested)
        || (canonical.trim() == requested.trim() && !alias.eq_ignore_ascii_case(&requested))
    {
        return Ok(ResolvedModel::PinnedRaw {
            requested,
            mapping: goat_mapping(&canonical, true),
        });
    }
    if let Some(entry) = registry.aliases.get(&alias) {
        let mut mappings = entry.mappings.clone();
        if !mappings.iter().any(ProviderMapping::is_command_code_goat) {
            mappings.push(goat_mapping(&canonical, true));
        }
        return Ok(ResolvedModel::Alias {
            requested,
            alias: entry.alias.clone(),
            mappings,
        });
    }
    Ok(ResolvedModel::PinnedRaw {
        requested,
        mapping: goat_mapping(&canonical, true),
    })
}

fn overlay_known_goat(
    resolved: ResolvedModel,
    canonical: String,
) -> Result<ResolvedModel, ResolveError> {
    match resolved {
        ResolvedModel::Alias {
            requested,
            alias,
            mut mappings,
        } => {
            if let Some(existing) = mappings
                .iter_mut()
                .find(|mapping| mapping.is_command_code_goat())
            {
                existing.routeable = true;
                existing.upstream_model = canonical;
            } else {
                mappings.push(goat_mapping(&canonical, true));
            }
            Ok(ResolvedModel::Alias {
                requested,
                alias,
                mappings,
            })
        }
        ResolvedModel::PinnedRaw { requested, mapping } => {
            if mapping.is_command_code_goat() {
                return Ok(ResolvedModel::PinnedRaw {
                    requested,
                    mapping: goat_mapping(&canonical, true),
                });
            }
            Err(ResolveError::Ambiguous {
                requested,
                mappings: vec![mapping, goat_mapping(&canonical, true)],
            })
        }
    }
}

fn resolve_in(registry: &Registry, requested: &str) -> Result<ResolvedModel, ResolveError> {
    let original = requested.to_string();
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err(ResolveError::Unknown {
            requested: original,
        });
    }

    // Raw-looking IDs are exact provider identifiers. Case folding belongs to
    // published aliases (and the separate Custom matcher), never built-in raw
    // pins.
    if looks_raw_shaped(trimmed) {
        if let Some(mappings) = registry.raw_exact.get(trimmed) {
            return pin_or_ambiguous(original, mappings);
        }
        return Err(ResolveError::Unknown {
            requested: original,
        });
    }

    let folded = trimmed.to_lowercase();
    if registry
        .aliases
        .get(&folded)
        .is_some_and(|entry| entry.alias != trimmed)
        && let Some(mappings) = registry.raw_exact.get(trimmed)
    {
        return pin_or_ambiguous(original, mappings);
    }
    if let Some(entry) = registry.aliases.get(&folded) {
        return Ok(ResolvedModel::Alias {
            requested: original,
            alias: entry.alias.clone(),
            mappings: entry.mappings.clone(),
        });
    }
    if let Some(mappings) = registry.raw_exact.get(trimmed) {
        return pin_or_ambiguous(original, mappings);
    }
    Err(ResolveError::Unknown {
        requested: original,
    })
}

/// Preferred aliases present in the registry, including fail-closed-only names.
/// Client `GET /v1/models` uses [`published_routeable_aliases`] instead.
pub fn published_aliases() -> Vec<String> {
    registry()
        .aliases
        .values()
        .map(|entry| entry.alias.clone())
        .collect()
}

/// A routeable preferred alias advertised by `GET /v1/models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedAlias {
    pub alias: String,
    pub owned_by: String,
}

/// Routeable preferred aliases that `GET /v1/models` exposes, in deterministic
/// registry order. `owned_by` is the first routeable mapping's `provider_id`.
/// Non-routeable GOAT / Custom mappings stay unpublished.
///
/// First-wins `owned_by` is only the client list advertisement. Catalog and
/// application-model discovery use [`routeable_aliases_for`], which keeps an
/// alias under every provider that currently has a routeable mapping.
pub fn published_routeable_aliases() -> Vec<PublishedAlias> {
    published_routeable_in(registry())
}

/// Published aliases after applying all runtime catalogs. Exact
/// raw-only rows remain outside this Alias-only list.
pub fn published_routeable_aliases_with_runtime_catalogs(
    catalogs: RuntimeCatalogs<'_>,
) -> Vec<PublishedAlias> {
    published_routeable_in(&build_runtime_registry(catalogs))
}

/// Client-visible names: generated and explicit aliases plus uniquely resolved
/// exact Go IDs. Discovery alone does not create shared aliases. Hosts
/// still apply protocol enablement and the operator publication switch.
pub fn published_routeable_models_with_runtime_catalogs(
    catalogs: RuntimeCatalogs<'_>,
) -> Vec<PublishedAlias> {
    RuntimeCatalogIndex::from_catalogs(catalogs).published_models()
}

/// Public names that resolve to this provider, independent of first-wins ownership.
pub fn routeable_models_for_with_runtime_catalogs(
    provider_id: &str,
    catalogs: RuntimeCatalogs<'_>,
) -> Vec<String> {
    let index = RuntimeCatalogIndex::from_catalogs(catalogs);
    let go = catalogs.go;
    index
        .published_models()
        .into_iter()
        .filter(|item| {
            index.resolve(&item.alias).is_ok_and(|resolved| {
                resolved.routeable_mappings().iter().any(|mapping| {
                    mapping.provider_id == provider_id
                        && (provider_id != OPENCODE_PROVIDER_ID
                            || go.contains(&mapping.upstream_model))
                })
            })
        })
        .map(|item| item.alias)
        .collect()
}

fn go_catalog_alias(model_id: &str) -> String {
    model_id
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-")
}

/// Unique last-segment kebab names from every saved Command catalog row become
/// aliases. New Go raw-only names remain reserved to Go; existing shared aliases
/// can gain a Command mapping without changing their identity.
fn goat_leaf_alias_is_publishable(
    alias: &str,
    unique: bool,
    go_model_ids: &[String],
    registry: &Registry,
) -> bool {
    if !unique || alias.is_empty() || looks_raw_shaped(alias) {
        return false;
    }
    if is_command_alias_authorized(registry, alias) {
        return true;
    }
    !go_model_ids.iter().any(|id| id.eq_ignore_ascii_case(alias))
}

/// Canonical spelling for a code-owned client alias. This remains an alias
/// authority check, not catalog-name normalization: CPA rows such as
/// `MiniMax-M3` stay exact raw pins unless CPA itself exposes `minimax-m3`.
fn code_owned_alias(registry: &Registry, candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    if looks_raw_shaped(candidate) {
        return None;
    }
    registry
        .aliases
        .get(&candidate.to_ascii_lowercase())
        .map(|entry| entry.alias.clone())
}

// Generated Command aliases cannot authorize suffix stripping on other rows.
// Keep naming independent of catalog iteration order and later overlays.
fn is_command_alias_authorized(registry: &Registry, candidate: &str) -> bool {
    registry.aliases.get(candidate).is_some_and(|entry| {
        entry.mappings.iter().any(|mapping| {
            mapping.is_opencode_go()
                || mapping.is_zen_free()
                || mapping.is_minimax_cn()
                || mapping.is_kimi_cn()
        })
    })
}

/// Public names for a complete provider catalog, using the runtime collision,
/// reservation, tag selection and saved override rules.
pub fn catalog_aliases(
    provider_id: &str,
    catalogs: RuntimeCatalogs<'_>,
) -> BTreeMap<String, String> {
    let registry = build_runtime_registry(catalogs);
    let mut result = BTreeMap::<String, String>::new();
    for entry in registry.aliases.values() {
        for mapping in &entry.mappings {
            if mapping.provider_id == provider_id && mapping.routeable {
                result
                    .entry(mapping.upstream_model.clone())
                    .or_insert_with(|| entry.alias.clone());
            }
        }
    }
    result
}

/// Generated Command aliases for the complete saved catalog. Management views
/// use the same collision and Go-name reservation rules as runtime resolution.
pub fn command_catalog_aliases(
    model_ids: &[String],
    go_model_ids: &[String],
    zen_free_models: &[String],
) -> BTreeMap<String, String> {
    catalog_aliases(
        COMMAND_CODE_PROVIDER_ID,
        RuntimeCatalogs {
            command_code: model_ids,
            go: go_model_ids,
            zen_free: zen_free_models,
            ..RuntimeCatalogs::default()
        },
    )
}

fn command_alias_for_catalog(upstream_model: &str, registry: &Registry) -> String {
    let leaf = upstream_model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-");
    let is_authorized_alias = |candidate: &str| is_command_alias_authorized(registry, candidate);
    if is_authorized_alias(&leaf) {
        return leaf;
    }
    for suffix in COMMAND_ALIAS_SUFFIX_EXCEPTIONS {
        if let Some(candidate) = leaf.strip_suffix(suffix)
            && is_authorized_alias(candidate)
        {
            return candidate.to_string();
        }
    }
    leaf
}

/// Single-row naming convenience. Catalog-wide consumers use `catalog_aliases`
/// so collisions and tagged selection share the runtime rules.
/// Go keeps its shared names; Zen Free strips `-free`.
/// An empty result means the row is raw-only.
pub fn canonical_alias_for_provider_model(
    provider_id: &str,
    upstream_model: &str,
    go_model_ids: &[String],
    zen_free_models: &[String],
) -> String {
    let registry = build_registry(zen_free_models);
    if provider_id == OPENCODE_PROVIDER_ID {
        let candidate = upstream_model.trim().to_ascii_lowercase();
        return registry
            .aliases
            .get(&candidate)
            .map(|entry| entry.alias.clone())
            .unwrap_or_default();
    }
    if provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID {
        let candidate = stripped_free_alias(upstream_model)
            .unwrap_or(upstream_model)
            .trim()
            .to_ascii_lowercase();
        return registry
            .aliases
            .get(&candidate)
            .map(|entry| entry.alias.clone())
            .unwrap_or_default();
    }
    if provider_id == COMMAND_CODE_PROVIDER_ID {
        let candidate = command_alias_for_catalog(upstream_model, &registry);
        return if goat_leaf_alias_is_publishable(&candidate, true, go_model_ids, &registry) {
            candidate
        } else {
            String::new()
        };
    }
    if matches!(
        provider_id,
        MINIMAX_PROVIDER_ID | KIMI_PROVIDER_ID | OLLAMA_PROVIDER_ID
    ) {
        let models = [upstream_model.to_string()];
        let catalogs = RuntimeCatalogs {
            go: go_model_ids,
            zen_free: zen_free_models,
            minimax: if provider_id == MINIMAX_PROVIDER_ID {
                &models
            } else {
                &[]
            },
            kimi: if provider_id == KIMI_PROVIDER_ID {
                &models
            } else {
                &[]
            },
            ollama: if provider_id == OLLAMA_PROVIDER_ID {
                &models
            } else {
                &[]
            },
            ..RuntimeCatalogs::default()
        };
        return catalog_aliases(provider_id, catalogs)
            .remove(upstream_model)
            .unwrap_or_default();
    }
    if provider_id == CUSTOM_PROVIDER_ID {
        return upstream_model.trim().to_string();
    }
    String::new()
}

fn published_routeable_in(registry: &Registry) -> Vec<PublishedAlias> {
    registry
        .aliases
        .values()
        .filter_map(|entry| {
            entry
                .mappings
                .iter()
                .find(|mapping| mapping.routeable)
                .map(|mapping| PublishedAlias {
                    alias: entry.alias.clone(),
                    owned_by: mapping.provider_id.clone(),
                })
        })
        .collect()
}

/// Preferred aliases that currently have a routeable mapping for this
/// provider, in deterministic registry order. Raw upstream IDs are
/// never returned. Unroutable mappings yield an empty list without a
/// hardcoded per-plan alias set.
pub fn routeable_aliases_for(provider_id: &str) -> Vec<String> {
    routeable_aliases_for_in(registry(), provider_id)
}

fn routeable_aliases_for_in(registry: &Registry, provider_id: &str) -> Vec<String> {
    registry
        .aliases
        .values()
        .filter(|entry| {
            entry
                .mappings
                .iter()
                .any(|mapping| mapping.routeable && mapping.provider_id == provider_id)
        })
        .map(|entry| entry.alias.clone())
        .collect()
}

/// Routeable aliases for one sealed provider after applying all runtime
/// catalogs.
pub fn routeable_aliases_for_with_runtime_catalogs(
    provider_id: &str,
    catalogs: RuntimeCatalogs<'_>,
) -> Vec<String> {
    routeable_aliases_for_in(&build_runtime_registry(catalogs), provider_id)
}

#[cfg(test)]
mod tests;
