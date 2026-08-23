//! External-crate smoke for the Stage 1 kernel facade.
//!
//! `PricingSnapshot::estimate` must remain an inherent method so dependents
//! can call `embedded_seed().estimate(...)` without importing a helper trait.

use ocg_core::kernel::catalog::OPENCODE_GO_USAGE_URL;
use ocg_core::kernel::ids::{OPENCODE_PROVIDER_ID, custom_model_id_matches};
use ocg_core::kernel::protocol::{ApiFormat, is_known_model};
use ocg_core::kernel::zen::stripped_free_alias;
use ocg_core::pricing::embedded_seed;

#[test]
fn embedded_seed_estimate_is_an_inherent_method() {
    let estimate = embedded_seed().estimate("glm-5.3", 1, 1, 0, 0, None);
    assert_eq!(estimate.cost_state, "priced");
    assert!(estimate.cost.is_some());
}

#[test]
fn kernel_facade_keeps_public_module_paths() {
    assert_eq!(OPENCODE_GO_USAGE_URL, "https://opencode.ai/zen/go/v1/usage");
    assert_eq!(OPENCODE_PROVIDER_ID, "opencode");
    assert!(custom_model_id_matches("glm-5.2", "GLM-5.2"));
    assert!(is_known_model("glm-5.2"));
    assert_eq!(
        ApiFormat::ChatCompletions.upstream_path(),
        Some("/v1/chat/completions")
    );
    assert_eq!(stripped_free_alias("mimo-v2.5-free"), Some("mimo-v2.5"));
}
