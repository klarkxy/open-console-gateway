//! Hardcoded unified model alias registry.
//!
//! Outbound clients should send stable lowercase kebab-case aliases. Existing
//! OpenCode Go model IDs are the preferred aliases in this slice. Case-folded
//! kebab spellings such as `GLM-5.2` are accepted. Names containing `/`, `_`,
//! or whitespace are treated as raw IDs and never folded onto a kebab alias
//! (`glm/5.2` is not `glm-5.2`). A raw upstream model ID is accepted only
//! when it uniquely selects one provider mapping; ambiguity returns
//! [`ResolveError::Ambiguous`] with code [`AMBIGUOUS_MODEL_ID`].
//!
//! Command Code GOAT has one official non-routeable mapping:
//! Alias `deepseek-v4-flash` → raw `deepseek/deepseek-v4-flash`. The kebab
//! alias stays Go-owned and published; the unique slash raw ID pins to GOAT
//! and is not production-selectable. SCNet and Custom stay fail-closed.
//! Later provider adapters consume [`ProviderMapping`] from
//! [`crate::gateway::materialize`]: parse the client protocol once, then
//! materialize model / protocol / endpoint / auth per candidate. Adapters must
//! not probe a billable inference path to discover protocol support. The
//! OpenCode `MODEL_PROTOCOLS` table stays Go-specific.

use crate::gateway::free_models::{is_free_model, mapped_free_for};
use crate::gateway::protocol::supported_model_ids;
use crate::provider::{
    ANONYMOUS_FREE_OFFERING_ID, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, COMMAND_CODE_PROVIDER_ID, GO_OFFERING_ID,
    GOAT_OFFERING_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Machine-readable error code for a raw ID that matches more than one mapping.
pub const AMBIGUOUS_MODEL_ID: &str = "ambiguous_model_id";

/// One provider's upstream identity for a client-facing name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMapping {
    pub provider_id: &'static str,
    pub offering_id: &'static str,
    pub upstream_model: &'static str,
    /// Production-routeable mappings only. Reserved offerings stay false.
    pub routeable: bool,
}

impl ProviderMapping {
    pub fn is_opencode_go(&self) -> bool {
        self.provider_id == OPENCODE_PROVIDER_ID && self.offering_id == GO_OFFERING_ID
    }

    pub fn is_zen_free(&self) -> bool {
        self.provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID
            && self.offering_id == ANONYMOUS_FREE_OFFERING_ID
    }

    pub fn is_command_code_goat(&self) -> bool {
        crate::provider::is_command_code_goat(self.provider_id, self.offering_id)
    }
}

/// A preferred client-facing alias and its provider mappings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub alias: &'static str,
    pub mappings: Vec<ProviderMapping>,
    /// OpenCode prefer-mode free twin, when the docs mapping exists.
    pub prefer_twin: Option<&'static str>,
}

/// Result of looking up a client-supplied model name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedModel {
    /// Preferred alias. May follow account order, sticky, and fallback across
    /// routeable mappings (including Zen prefer overlay).
    Alias {
        requested: String,
        alias: &'static str,
        mappings: Vec<ProviderMapping>,
        prefer_twin: Option<&'static str>,
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
                    .map(|mapping| {
                        format!(
                            "{}/{}:{}",
                            mapping.provider_id, mapping.offering_id, mapping.upstream_model
                        )
                    })
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

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(build_builtin_registry)
}

fn build_builtin_registry() -> Registry {
    let mut specs = Vec::new();
    for id in supported_model_ids() {
        if is_free_model(id) {
            specs.push(AliasEntry {
                alias: id,
                mappings: vec![zen_mapping(id)],
                prefer_twin: None,
            });
        } else {
            specs.push(AliasEntry {
                alias: id,
                mappings: go_alias_mappings(id),
                prefer_twin: mapped_free_for(id),
            });
        }
    }
    registry_from_entries(specs)
}

fn go_mapping(upstream_model: &'static str) -> ProviderMapping {
    ProviderMapping {
        provider_id: OPENCODE_PROVIDER_ID,
        offering_id: GO_OFFERING_ID,
        upstream_model,
        routeable: true,
    }
}

fn goat_deepseek_v4_flash_mapping() -> ProviderMapping {
    ProviderMapping {
        provider_id: COMMAND_CODE_PROVIDER_ID,
        offering_id: GOAT_OFFERING_ID,
        upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        routeable: false,
    }
}

fn go_alias_mappings(upstream_model: &'static str) -> Vec<ProviderMapping> {
    let mut mappings = vec![go_mapping(upstream_model)];
    if upstream_model == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS {
        mappings.push(goat_deepseek_v4_flash_mapping());
    }
    mappings
}

fn zen_mapping(upstream_model: &'static str) -> ProviderMapping {
    ProviderMapping {
        provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID,
        offering_id: ANONYMOUS_FREE_OFFERING_ID,
        upstream_model,
        routeable: true,
    }
}

struct Registry {
    aliases: BTreeMap<String, AliasEntry>,
    /// Exact upstream model ID → every mapping that uses it.
    raw_exact: BTreeMap<String, Vec<ProviderMapping>>,
    /// Lowercased exact upstream model ID → every mapping that uses it.
    /// Separator characters are preserved; `glm/5.2` never joins `glm-5.2`.
    raw_folded: BTreeMap<String, Vec<ProviderMapping>>,
}

fn registry_from_entries(entries: Vec<AliasEntry>) -> Registry {
    let mut aliases = BTreeMap::new();
    let mut raw_exact: BTreeMap<String, Vec<ProviderMapping>> = BTreeMap::new();
    let mut raw_folded: BTreeMap<String, Vec<ProviderMapping>> = BTreeMap::new();
    for entry in entries {
        debug_assert!(
            !looks_raw_shaped(entry.alias),
            "published aliases must be kebab-case without slash, space, or underscore"
        );
        for mapping in &entry.mappings {
            raw_exact
                .entry(mapping.upstream_model.to_string())
                .or_default()
                .push(mapping.clone());
            raw_folded
                .entry(mapping.upstream_model.to_lowercase())
                .or_default()
                .push(mapping.clone());
        }
        aliases.insert(entry.alias.to_lowercase(), entry);
    }
    Registry {
        aliases,
        raw_exact,
        raw_folded,
    }
}

/// Slash, underscore, or whitespace means "treat as a raw ID": never fold those
/// characters into `-` and then hit a kebab alias (`glm/5.2` ≠ `glm-5.2`).
fn looks_raw_shaped(name: &str) -> bool {
    name.chars()
        .any(|ch| ch == '/' || ch == '_' || ch.is_whitespace())
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

fn resolve_in(registry: &Registry, requested: &str) -> Result<ResolvedModel, ResolveError> {
    let original = requested.to_string();
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err(ResolveError::Unknown {
            requested: original,
        });
    }

    // Raw-looking IDs are resolved against exact (then case-folded exact) raw
    // keys before any alias spelling. Separator folding is never applied.
    if looks_raw_shaped(trimmed) {
        if let Some(mappings) = registry.raw_exact.get(trimmed) {
            return pin_or_ambiguous(original, mappings);
        }
        if let Some(mappings) = registry.raw_folded.get(&trimmed.to_lowercase()) {
            return pin_or_ambiguous(original, mappings);
        }
        return Err(ResolveError::Unknown {
            requested: original,
        });
    }

    let folded = trimmed.to_lowercase();
    if let Some(entry) = registry.aliases.get(&folded) {
        return Ok(ResolvedModel::Alias {
            requested: original,
            alias: entry.alias,
            mappings: entry.mappings.clone(),
            prefer_twin: entry.prefer_twin,
        });
    }
    if let Some(mappings) = registry.raw_exact.get(trimmed) {
        return pin_or_ambiguous(original, mappings);
    }
    if let Some(mappings) = registry.raw_folded.get(&folded) {
        return pin_or_ambiguous(original, mappings);
    }
    Err(ResolveError::Unknown {
        requested: original,
    })
}

/// Preferred aliases that live `GET /v1/models` exposes.
pub fn published_aliases() -> Vec<&'static str> {
    registry()
        .aliases
        .values()
        .map(|entry| entry.alias)
        .collect()
}

pub fn is_published_alias(name: &str) -> bool {
    matches!(resolve(name), Ok(ResolvedModel::Alias { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::free_models::free_model_ids;

    #[test]
    fn go_model_ids_are_preferred_aliases() {
        let resolved = resolve("glm-5.2").expect("known Go model");
        match resolved {
            ResolvedModel::Alias {
                alias, mappings, ..
            } => {
                assert_eq!(alias, "glm-5.2");
                assert_eq!(mappings.len(), 1);
                assert!(mappings[0].is_opencode_go());
                assert!(mappings[0].routeable);
                assert_eq!(mappings[0].upstream_model, "glm-5.2");
            }
            other => panic!("expected alias, got {other:?}"),
        }
    }

    #[test]
    fn alias_lookup_is_case_insensitive_kebab() {
        let resolved = resolve("GLM-5.2").expect("case-folded alias");
        assert!(matches!(
            resolved,
            ResolvedModel::Alias {
                alias: "glm-5.2",
                ..
            }
        ));
        assert!(is_published_alias("Grok-4.5"));
        assert!(is_published_alias(" glm-5.2 "));
        for alias in published_aliases() {
            assert_eq!(alias, alias.to_lowercase());
            assert!(!alias.is_empty());
            assert!(!looks_raw_shaped(alias));
        }
    }

    #[test]
    fn raw_looking_names_do_not_collapse_onto_kebab_aliases() {
        for name in ["glm/5.2", "GLM_5.2", "Grok 4.5", "glm 5.2"] {
            match resolve(name) {
                Err(ResolveError::Unknown { requested }) => assert_eq!(requested, name),
                other => panic!("`{name}` must not collapse onto a kebab alias, got {other:?}"),
            }
            assert!(!is_published_alias(name));
        }
        assert!(matches!(
            resolve("glm-5.2").unwrap(),
            ResolvedModel::Alias {
                alias: "glm-5.2",
                ..
            }
        ));
    }

    #[test]
    fn zen_free_ids_are_aliases_not_go() {
        let resolved = resolve("deepseek-v4-flash-free").expect("free alias");
        match resolved {
            ResolvedModel::Alias {
                alias, mappings, ..
            } => {
                assert_eq!(alias, "deepseek-v4-flash-free");
                assert_eq!(mappings.len(), 1);
                assert!(mappings[0].is_zen_free());
                assert_eq!(mappings[0].upstream_model, "deepseek-v4-flash-free");
            }
            other => panic!("expected alias, got {other:?}"),
        }
    }

    #[test]
    fn prefer_twins_are_recorded_on_go_aliases() {
        match resolve("deepseek-v4-flash").unwrap() {
            ResolvedModel::Alias { prefer_twin, .. } => {
                assert_eq!(prefer_twin, Some("deepseek-v4-flash-free"));
            }
            other => panic!("expected alias, got {other:?}"),
        }
        match resolve("mimo-v2.5").unwrap() {
            ResolvedModel::Alias { prefer_twin, .. } => {
                assert_eq!(prefer_twin, Some("mimo-v2.5-free"));
            }
            other => panic!("expected alias, got {other:?}"),
        }
        match resolve("glm-5.2").unwrap() {
            ResolvedModel::Alias { prefer_twin, .. } => assert_eq!(prefer_twin, None),
            other => panic!("expected alias, got {other:?}"),
        }
    }

    #[test]
    fn registry_covers_every_opencode_protocol_id() {
        let aliases = published_aliases();
        for id in supported_model_ids() {
            assert!(
                aliases.contains(&id),
                "MODEL_PROTOCOLS id `{id}` must have an alias"
            );
        }
        for id in free_model_ids() {
            assert!(
                aliases.contains(&id),
                "free model `{id}` must have an alias"
            );
            assert!(resolve(id).unwrap().routeable_mappings()[0].is_zen_free());
        }
        assert!(!aliases.iter().any(|alias| alias.contains("goat")));
        assert!(!aliases.iter().any(|alias| alias.contains("scnet")));
        assert!(!aliases.iter().any(|alias| alias.contains("custom")));
    }

    #[test]
    fn slash_prefixed_goat_raw_pins_to_command_code_and_does_not_steal_go() {
        match resolve(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM) {
            Ok(ResolvedModel::PinnedRaw { mapping, .. }) => {
                assert!(mapping.is_command_code_goat());
                assert!(!mapping.routeable);
                assert_eq!(
                    mapping.upstream_model,
                    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
                );
                assert!(
                    ResolvedModel::PinnedRaw {
                        requested: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                        mapping: mapping.clone(),
                    }
                    .routeable_mappings()
                    .is_empty()
                );
            }
            other => panic!("GOAT raw id must uniquely pin to command-code/goat, got {other:?}"),
        }
        match resolve(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS).unwrap() {
            ResolvedModel::Alias {
                alias, mappings, ..
            } => {
                assert_eq!(alias, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS);
                assert!(mappings.iter().any(|mapping| mapping.is_opencode_go()));
                assert!(
                    mappings
                        .iter()
                        .any(|mapping| mapping.is_command_code_goat() && !mapping.routeable)
                );
                let routeable = mappings
                    .iter()
                    .filter(|mapping| mapping.routeable)
                    .collect::<Vec<_>>();
                assert_eq!(routeable.len(), 1);
                assert!(routeable[0].is_opencode_go());
                assert_eq!(
                    routeable[0].upstream_model,
                    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS
                );
            }
            other => panic!("expected published Go alias, got {other:?}"),
        }
        assert!(is_published_alias(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS
        ));
        assert!(!is_published_alias(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
        ));
        assert!(
            !published_aliases().iter().any(|alias| alias.contains('/')
                || *alias == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
        );
    }

    #[test]
    fn unknown_names_are_not_aliases() {
        match resolve("definitely-not-a-model") {
            Err(ResolveError::Unknown { requested }) => {
                assert_eq!(requested, "definitely-not-a-model");
                assert!(
                    ResolveError::Unknown {
                        requested: requested.clone()
                    }
                    .code()
                    .is_none()
                );
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn unique_raw_id_pins_to_one_mapping() {
        let registry = registry_from_entries(vec![
            AliasEntry {
                alias: "widget",
                mappings: vec![go_mapping("widget")],
                prefer_twin: None,
            },
            AliasEntry {
                alias: "gadget",
                mappings: vec![ProviderMapping {
                    provider_id: OPENCODE_PROVIDER_ID,
                    offering_id: GO_OFFERING_ID,
                    upstream_model: "vendor.gadget-v1",
                    routeable: true,
                }],
                prefer_twin: None,
            },
        ]);
        match resolve_in(&registry, "vendor.gadget-v1").unwrap() {
            ResolvedModel::PinnedRaw { mapping, .. } => {
                assert_eq!(mapping.upstream_model, "vendor.gadget-v1");
                assert!(mapping.is_opencode_go());
            }
            other => panic!("expected pinned raw, got {other:?}"),
        }
        // Alias still wins when a kebab string is both an alias and a raw ID.
        assert!(matches!(
            resolve_in(&registry, "widget").unwrap(),
            ResolvedModel::Alias {
                alias: "widget",
                ..
            }
        ));
        // Exact slash-form raw IDs pin without collapsing onto a kebab alias.
        let slash_registry = registry_from_entries(vec![AliasEntry {
            alias: "widget",
            mappings: vec![ProviderMapping {
                provider_id: OPENCODE_PROVIDER_ID,
                offering_id: GO_OFFERING_ID,
                upstream_model: "vendor/widget-v1",
                routeable: true,
            }],
            prefer_twin: None,
        }]);
        match resolve_in(&slash_registry, "vendor/widget-v1").unwrap() {
            ResolvedModel::PinnedRaw { mapping, .. } => {
                assert_eq!(mapping.upstream_model, "vendor/widget-v1");
            }
            other => panic!("exact slash raw must pin, got {other:?}"),
        }
        assert!(matches!(
            resolve_in(&slash_registry, "vendor-widget-v1"),
            Err(ResolveError::Unknown { .. })
        ));
    }

    #[test]
    fn overlapping_raw_ids_return_ambiguous_model_id() {
        let registry = registry_from_entries(vec![
            AliasEntry {
                alias: "alpha",
                mappings: vec![go_mapping("shared-raw")],
                prefer_twin: None,
            },
            AliasEntry {
                alias: "beta",
                mappings: vec![zen_mapping("shared-raw")],
                prefer_twin: None,
            },
        ]);
        match resolve_in(&registry, "shared-raw") {
            Err(error) => {
                assert_eq!(error.code(), Some(AMBIGUOUS_MODEL_ID));
                let message = error.message();
                assert!(message.contains(AMBIGUOUS_MODEL_ID));
                assert!(message.contains("shared-raw"));
                assert!(message.contains("opencode/go"));
                assert!(message.contains("opencode-zen-free"));
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
        // Preferred aliases still resolve even when their upstream IDs overlap.
        assert!(matches!(
            resolve_in(&registry, "alpha").unwrap(),
            ResolvedModel::Alias { alias: "alpha", .. }
        ));
    }

    #[test]
    fn fail_closed_raw_mapping_is_not_routeable() {
        let registry = registry_from_entries(vec![AliasEntry {
            alias: "visible",
            mappings: vec![ProviderMapping {
                provider_id: "command-code",
                offering_id: "goat",
                upstream_model: "goat-only-raw",
                routeable: false,
            }],
            prefer_twin: None,
        }]);
        match resolve_in(&registry, "goat-only-raw") {
            Ok(ResolvedModel::PinnedRaw { mapping, .. }) => {
                assert!(!mapping.routeable);
                assert_eq!(mapping.provider_id, "command-code");
                assert!(
                    ResolvedModel::PinnedRaw {
                        requested: "goat-only-raw".into(),
                        mapping: mapping.clone(),
                    }
                    .routeable_mappings()
                    .is_empty()
                );
            }
            other => {
                panic!("fail-closed unique raw must pin without being routeable, got {other:?}")
            }
        }
        match resolve_in(&registry, "visible").unwrap() {
            ResolvedModel::Alias { mappings, .. } => {
                assert!(!mappings[0].routeable);
                assert!(
                    ResolvedModel::Alias {
                        requested: "visible".into(),
                        alias: "visible",
                        mappings: mappings.clone(),
                        prefer_twin: None,
                    }
                    .routeable_mappings()
                    .is_empty()
                );
            }
            other => panic!("expected alias, got {other:?}"),
        }
    }
}
