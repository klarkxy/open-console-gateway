//! Compatibility facade for [`ocg_gateway::alias`].
//!
//! Public items match the historical `ocg_core::alias` surface. Do not
//! glob-reexport or reexport the module itself. The owning I/O-free
//! registry lives in `ocg_gateway::alias`.
//!
//! Custom capability matching stays on `kernel::ids::custom_model_id_matches`;
//! the gateway implementation uses the equivalent domain matcher.

#[doc(inline)]
pub use ocg_gateway::alias::{
    AMBIGUOUS_MODEL_ID, AliasEntry, CUSTOM_DYNAMIC_UPSTREAM, ExtraProviderCatalog, ProviderMapping,
    PublishedAlias, ResolveError, ResolvedModel, RuntimeCatalogs,
    canonical_alias_for_provider_model, published_aliases, published_routeable_aliases,
    published_routeable_aliases_with_runtime_catalogs,
    published_routeable_models_with_runtime_catalogs, resolve, resolve_with_runtime_catalogs,
    routeable_aliases_for, routeable_aliases_for_with_runtime_catalogs,
    routeable_models_for_with_runtime_catalogs,
};
