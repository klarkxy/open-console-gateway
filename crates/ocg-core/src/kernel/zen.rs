//! Compatibility facade for [`ocg_domain::zen`].
//!
//! Public items match the historical `ocg_core::kernel::zen` surface.

pub use ocg_domain::zen::{
    ZEN_MODELS_SOURCE_URL, ZenFreeModelCatalog, ZenFreeModelView, model_views, parse_catalog,
    stripped_free_alias,
};
