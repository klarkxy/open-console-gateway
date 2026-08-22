//! HTTP-neutral, state-neutral control-plane query services.
//!
//! These functions are shared by Dashboard V2 compatibility adapters and the
//! V3 wire layer. They must not serialize HTTP envelopes, import or accept
//! `CoreState` / `gateway` / `dashboard` / `dashboard_v3`, or expose SQLite
//! rows / `AppConfig` / full `Account`. Adapters gather snapshots and locks
//! and pass narrow immutable inputs.

pub(crate) mod observability;
