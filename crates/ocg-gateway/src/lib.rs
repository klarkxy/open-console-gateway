//! Inference Gateway protocol, policy, and execution boundaries.

/// Hardcoded I/O-free client alias registry and raw-ID parser.
///
/// Public only as the cross-crate bridge; the host crate's `alias`
/// compatibility facade keeps the historical public paths.
#[doc(hidden)]
pub mod alias;

/// Data-only single-attempt transport boundary.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps these items crate-private.
#[doc(hidden)]
pub mod attempt;

/// Strict per-Key quota exhaustion recognition. Side effects stay in the host.
///
/// Public only as the cross-crate bridge; the host crate's quota-recovery
/// facade keeps these items crate-private.
#[doc(hidden)]
pub mod quota;

/// Pure attempt-adjacent provider/transport error classification policy.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps these items crate-private.
#[doc(hidden)]
pub mod classify;

/// Pure secret-free in-memory selection state machine.
///
/// Public only as the cross-crate bridge; a later host facade should keep
/// historical routing-runtime paths crate-private. Do not glob-reexport.
#[doc(hidden)]
pub mod selector;

/// Whole-document JSON protocol conversion.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::protocol`
/// facade keeps parse, usage, stream, and route-identity paths crate-owned.
/// Do not glob-reexport.
#[doc(hidden)]
pub mod protocol;

/// Pure per-attempt wire normalization (Ollama Cloud request/response rewrite).
///
/// Public only as the cross-crate bridge; the host crate's `gateway::wire`
/// facade keeps these items crate-private.
#[doc(hidden)]
pub mod wire;
