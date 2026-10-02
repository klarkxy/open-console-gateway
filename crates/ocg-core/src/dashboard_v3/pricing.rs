//! Pricing routes are not mounted.
//!
//! Debug tests can still install a process-generation fetch guard. Nothing in
//! this module reads that guard or refreshes a price snapshot.

#[cfg(debug_assertions)]
mod official_pricing_fetch {
    use crate::kernel::pricing::PricingSnapshot;
    use crate::state::CoreState;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};

    type OfficialFetch = Arc<dyn Fn(&CoreState) -> crate::Result<PricingSnapshot> + Send + Sync>;

    static OFFICIAL_FETCH_OVERRIDES: OnceLock<Mutex<HashMap<u64, OfficialFetch>>> = OnceLock::new();

    fn official_fetch_overrides() -> &'static Mutex<HashMap<u64, OfficialFetch>> {
        OFFICIAL_FETCH_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Test-only guard that drops one installed fetch closure.
    pub struct OfficialPricingFetchGuard {
        process_generation: u64,
    }

    impl Drop for OfficialPricingFetchGuard {
        fn drop(&mut self) {
            official_fetch_overrides()
                .lock()
                .remove(&self.process_generation);
        }
    }

    /// Bind an unused fetch closure to one CoreState process generation.
    #[must_use]
    pub fn install_official_pricing_fetch_for_tests(
        process_generation: u64,
        fetch: impl Fn(&CoreState) -> crate::Result<PricingSnapshot> + Send + Sync + 'static,
    ) -> OfficialPricingFetchGuard {
        official_fetch_overrides()
            .lock()
            .insert(process_generation, Arc::new(fetch));
        OfficialPricingFetchGuard { process_generation }
    }

    /// Bind an unused failure closure to one CoreState process generation.
    #[must_use]
    pub fn install_official_pricing_fetch_error_for_tests(
        process_generation: u64,
        message: impl Into<String>,
    ) -> OfficialPricingFetchGuard {
        let message = message.into();
        install_official_pricing_fetch_for_tests(process_generation, move |_| {
            Err(anyhow::anyhow!(message.clone()))
        })
    }
}

#[cfg(debug_assertions)]
pub use official_pricing_fetch::{
    OfficialPricingFetchGuard, install_official_pricing_fetch_error_for_tests,
    install_official_pricing_fetch_for_tests,
};
