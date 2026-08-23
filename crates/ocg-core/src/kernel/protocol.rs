//! Compatibility facade for [`ocg_domain::protocol`].
//!
//! Public items match the historical `ocg_core::kernel::protocol` surface.
//! `ModelProtocol` and `model_protocol` stay crate-private.

pub use ocg_domain::protocol::{
    ApiFormat, CommandCodeModelProtocol, command_code_model_protocol,
    command_code_protocol_profiles, command_code_supports_upstream, is_known_model,
    opencode_supports_upstream, supported_model_ids, supported_model_protocol_profiles,
    supported_model_protocols,
};

pub(crate) use ocg_domain::protocol::model_protocol;
// Keep the historical crate-private type path even though call sites name
// the value through `model_protocol` rather than `ModelProtocol`.
#[allow(unused_imports)]
pub(crate) use ocg_domain::protocol::ModelProtocol;
