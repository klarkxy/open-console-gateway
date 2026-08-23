//! Compatibility facade for [`ocg_domain::ids`].
//!
//! Public items match the historical `ocg_core::kernel::ids` surface.
//! `looks_raw_shaped` stays crate-private.

pub use ocg_domain::ids::{
    ANONYMOUS_FREE_OFFERING_ID, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, COMMAND_CODE_PROVIDER_ID, CUSTOM_API_OFFERING_ID,
    CUSTOM_PROVIDER_ID, DEFAULT_ACCOUNT_TEST_MODEL, GO_OFFERING_ID, GOAT_OFFERING_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, PRIMARY_KEY_ID, PRIMARY_KEY_NAME,
    SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID, SCNET_TOKEN_PLAN_OFFERING_IDS,
    SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID, SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID,
    ZEN_FREE_ACCOUNT_ID, ZEN_FREE_ACCOUNT_NAME, custom_model_id_matches, is_free_model,
    normalize_model_name,
};

pub(crate) use ocg_domain::ids::looks_raw_shaped;
