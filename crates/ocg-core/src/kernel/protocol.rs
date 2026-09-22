//! Compatibility facade for [`ocg_domain::protocol`].
//!
//! Public items match the historical `ocg_core::kernel::protocol` surface.
//! `model_protocol` stays crate-private.

pub use ocg_domain::protocol::{
    ApiFormat, CommandCodeModelProtocol, OFFICIAL_PROTOCOL_BASELINE_DATE,
    OLLAMA_CLOUD_STATIC_PROTOCOL_SNAPSHOT_DATE, OllamaCloudModelProtocol,
    command_code_constructable_formats, command_code_model_protocol,
    command_code_supports_upstream, command_code_upstream_path, is_known_model,
    ollama_cloud_includes_model, ollama_cloud_model_protocol, ollama_cloud_protocol_seed_ids,
    ollama_cloud_shared_alias_stems, ollama_cloud_supported_formats,
    ollama_cloud_supports_upstream, opencode_supports_upstream, supported_model_ids,
    supported_model_protocol_profiles, supported_model_protocols,
};

pub(crate) use ocg_domain::protocol::model_protocol;
