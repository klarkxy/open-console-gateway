//! Compatibility facade for [`ocg_domain::catalog`].
//!
//! Public items match the historical `ocg_core::kernel::catalog` surface.

pub use ocg_domain::catalog::{
    CatalogParseError, CredentialKind, OPENCODE_GO_USAGE_URL, QuotaScope, UpstreamAuthScheme,
    UpstreamProtocolKind,
};
