//! Digest of the effective projection. The preimage includes material
//! fingerprints, not the plaintext key.

use super::native_targets::native_targets_preimage;
use super::types::{NormalizedRoute, ProductProjection, ProjectedAuth, assigned_priorities};
use sha2::{Digest, Sha256};

/// SHA-256 of the exact config.yaml bytes. This is not written into the YAML.
pub(crate) fn wire_digest(yaml: &str) -> String {
    super::types::fingerprint(yaml.as_bytes())
}

/// Recompute the logical digest after native route sets are attached.
///
/// The preimage includes route-set fingerprints, canonical native targets, and
/// the effective native facts. It is not the SHA-256 of the YAML bytes.
pub(crate) fn refresh_logical_digest(
    projection: &mut ProductProjection,
) -> Result<(), super::types::ProjectionError> {
    assigned_priorities(projection)?;
    projection.digest = projection_digest(projection);
    Ok(())
}

/// Content identity of the effective projection. Desired and applied revisions,
/// process generation, and runtime secrets are not part of this preimage.
pub(super) fn projection_digest(projection: &ProductProjection) -> String {
    let priorities = assigned_priorities(projection).unwrap_or(super::types::AssignedPriorities {
        api: Vec::new(),
        oauth: Vec::new(),
    });
    let body = serde_json::json!({
        "routing_mode": projection.routing_mode,
        "conversation_sticky": projection.conversation_sticky,
        "conversation_ttl_secs": projection.conversation_ttl_secs,
        "proxy_mode": projection.proxy_mode,
        "proxy_url": projection.proxy_url,
        "proxy_list_direction": projection.proxy_list_direction,
        "proxy_list_models": projection.proxy_list_models,
        "omissions": projection.omissions.iter().map(|item| serde_json::json!({
            "credential_id": item.credential_id,
            "reason": item.reason,
            "model": item.model,
        })).collect::<Vec<_>>(),
        "oauth_refs": projection.oauth_refs.iter().enumerate().map(|(index, item)| serde_json::json!({
            "provider": item.provider,
            "product_provider_id": item.product_provider_id,
            "relative_path": item.relative_path,
            "material_revision": item.material_revision,
            "auth_id": item.auth_id,
            "credential_id": item.credential_id,
            "credential_version": item.credential_version,
            "routing_rank": item.routing_rank,
            "priority": priority_at(&priorities.oauth, index),
            "models": item.models,
            "raw_provider_label": item.raw_provider_label,
            "native_mode": item.native_mode,
            "reported_base": item.reported_base,
        })).collect::<Vec<_>>(),
        "route_sets": projection.route_sets.iter().map(|set| serde_json::json!({
            "auth_id": set.auth_id,
            "binding_id": set.binding_id,
            "credential_id": set.credential_id,
            "credential_version": set.credential_version,
            "fingerprint": set.fingerprint,
            "material_fingerprint": set.material_fingerprint,
            "routing_rank": set.routing_rank,
            "routes": set.routes.iter().map(|route| serde_json::json!({
                "endpoint_fingerprint": route.endpoint_fingerprint,
                "endpoint_id": route.endpoint_id,
                "native_targets": native_targets_preimage(&route.native_targets),
                "origin": route.origin,
                "protocol": route.protocol,
                "public_model": route.public_model,
                "upstream_model": route.upstream_model,
                "validation_only": route.validation_only,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "auths": projection.auths.iter().enumerate().map(|(index, auth)| {
            let mut body = auth_body(auth);
            if let Some(object) = body.as_object_mut() {
                object.insert(
                    "priority".to_string(),
                    serde_json::json!(priority_at(&priorities.api, index)),
                );
            }
            body
        }).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    hex::encode(Sha256::digest(bytes))
}

fn priority_at(values: &[i64], index: usize) -> i64 {
    values.get(index).copied().unwrap_or(0)
}

/// SHA-256 hex of one normalized endpoint URL. The raw URL is not the result.
pub(crate) fn endpoint_fingerprint(url: &str) -> String {
    super::types::fingerprint(url.as_bytes())
}

/// SHA-256 hex of the credential identity and its sorted routes.
/// The preimage omits plaintext, the endpoint URL, the hop secret, and this
/// fingerprint itself. `endpoint_fingerprint` stands in for the URL.
/// `native_targets` is always present, including an empty list.
pub(crate) fn credential_route_fingerprint(
    auth_id: &str,
    credential_id: &str,
    credential_version: u64,
    binding_id: &str,
    material_fingerprint: &str,
    routing_rank: u32,
    routes: &[NormalizedRoute],
) -> String {
    let mut ordered: Vec<&NormalizedRoute> = routes.iter().collect();
    ordered.sort_by(|left, right| {
        left.public_model
            .cmp(&right.public_model)
            .then(left.protocol.cmp(&right.protocol))
            .then(left.endpoint_id.cmp(&right.endpoint_id))
            .then(left.endpoint_fingerprint.cmp(&right.endpoint_fingerprint))
            .then(left.upstream_model.cmp(&right.upstream_model))
            .then(left.origin.cmp(&right.origin))
            .then(left.validation_only.cmp(&right.validation_only))
    });
    let body = serde_json::json!({
        "auth_id": auth_id,
        "binding_id": binding_id,
        "credential_id": credential_id,
        "credential_version": credential_version,
        "material_fingerprint": material_fingerprint,
        "routing_rank": routing_rank,
        "routes": ordered.iter().map(|route| serde_json::json!({
            "endpoint_fingerprint": route.endpoint_fingerprint,
            "endpoint_id": route.endpoint_id,
            "origin": route.origin,
            "protocol": route.protocol,
            "native_targets": native_targets_preimage(&route.native_targets),
            "public_model": route.public_model,
            "upstream_model": route.upstream_model,
            "validation_only": route.validation_only,
        })).collect::<Vec<_>>(),
    });
    super::types::fingerprint(&serde_json::to_vec(&body).unwrap_or_default())
}

fn auth_body(auth: &ProjectedAuth) -> serde_json::Value {
    serde_json::json!({
        "auth_id": auth.auth_id,
        "credential_id": auth.credential_id,
        "legacy_account_id": auth.legacy_account_id,
        "credential_version": auth.credential_version,
        "binding_id": auth.binding_id,
        "routing_rank": auth.routing_rank,
        "sequence": auth.sequence,
        "provider_id": auth.provenance.provider_id,
        "adapter": auth.provenance.adapter,
        "legacy_kind": auth.provenance.legacy_kind,
        "legacy_id": auth.provenance.legacy_id,
        "preset_id": auth.provenance.preset_id,
        "destination_id": auth.provenance.destination_id,
        "quota_scope": auth.provenance.quota_scope,
        "scope": auth.scope,
        "material_sha256": auth.material.as_ref().map(|secret| secret.fingerprint()),
        "remote_inner": match auth.remote_inner {
            super::types::RemoteInner::NotRemote => "not_remote",
        },
        "request_identity": match auth.request_identity {
            super::types::RequestIdentityFact::None => "none",
            super::types::RequestIdentityFact::OpenCodeSession => "opencode_session",
        },
        "wire": match auth.wire {
            super::types::WireFact::None => "none",
            super::types::WireFact::OllamaReasoning => "ollama_reasoning",
        },
        "models": auth.models.iter().map(|model| serde_json::json!({
            "public_alias": model.public_alias,
            "upstream_name": model.upstream_name,
            "routes": model.routes.iter().map(|route| serde_json::json!({
                "protocol": route.protocol,
                "endpoint_url": route.endpoint_url,
                "auth": route.auth,
                "endpoint_id": route.endpoint_id,
                "validation_only": route.validation_only,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}
