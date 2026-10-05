//! Product projection of persisted routing facts into one CPA auth per credential.
//!
//! This module records what the owned CPA process must be told. It does not
//! select a credential, retry, send, or persist config. The private file shape
//! is the canonical contract in `projection-yaml-v1.md`.

mod auth_id;
mod build;
mod digest;
mod endpoints;
mod native_targets;
mod types;
mod yaml;

pub(crate) use auth_id::{auth_id, material_fingerprint};
pub(crate) use build::{
    CpaPlacement, classify_validation, classify_validation_with_auth, cpa_placement_from_origins,
    native_inclusion, note_native_route_unmapped, project, registration_is_complete,
};
pub(crate) use digest::{
    credential_route_fingerprint, endpoint_fingerprint, refresh_logical_digest, wire_digest,
};
pub(crate) use native_targets::{
    EffectiveNativeWire, GEMINI_CALLABLE_PROTOCOL, MAX_NATIVE_TARGETS,
    NATIVE_ENDPOINT_PIN_CAPABILITY, NativeAuthorityFacts, NativeSourceOperation,
    NativeTargetOutcome, accept_stored_native_targets, canonical_native_url, default_grant_ids,
    endpoint_pin_capability_listed, facts_from_effective, native_mode_ok, native_route_targets,
    native_targets_for, native_targets_preimage, normalize_native_label, targets_for_applied_route,
};
pub(crate) use types::{
    CredentialRouteSet, NativeDispatchTarget, NativeEndpointPin, NativeInclusion, NormalizedRoute,
    OAuthFileRef, PendingApprovedRoute, ProductProjection, ProjectedAuth, ProjectionError,
    ProjectionInput, ProjectionRevisions, RemoteInner, RequestIdentityFact, RuntimeEnvelope,
    ValidationCandidate, ValidationCandidateKind, ValidationRow, ValidationSidecar, WireFact,
};
pub(crate) use yaml::{RenderedYaml, render_canonical_yaml, render_standard_yaml};

#[cfg(test)]
mod tests;
