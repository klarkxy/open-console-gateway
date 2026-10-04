use super::*;

fn sample<'a>(model: &'a str, url: &'a str, version: u64) -> ReplayRouteIdentity<'a> {
    ReplayRouteIdentity {
        adapter: AdapterKind::Minimax,
        upstream: ApiFormat::Messages,
        upstream_model: model,
        request_url: url,
        credential_id: "cred-1",
        credential_version: version,
        destination_id: "destination-1",
        authorization_connection_id: "connection-1",
        binding_id: "binding-1",
        auth: UpstreamAuth::Bearer,
    }
}

#[test]
fn replay_domain_is_stable_and_changes_with_route_identity() {
    let first = replay_domain_for(&sample(
        "MiniMax-M3",
        "https://api.minimaxi.com/anthropic/v1/messages",
        3,
    ))
    .unwrap();
    let again = replay_domain_for(&sample(
        "MiniMax-M3",
        "https://api.minimaxi.com/anthropic/v1/messages",
        3,
    ))
    .unwrap();
    assert_eq!(
        first.hex(),
        again.hex(),
        "the digest has no clock or random"
    );
    assert_eq!(first.hex().len(), 64);
    assert!(first.hex().bytes().all(|byte| byte.is_ascii_hexdigit()));

    let other_model = replay_domain_for(&sample(
        "MiniMax-M2",
        "https://api.minimaxi.com/anthropic/v1/messages",
        3,
    ))
    .unwrap();
    let other_url = replay_domain_for(&sample(
        "MiniMax-M3",
        "https://api.minimaxi.com/v1/messages",
        3,
    ))
    .unwrap();
    let rotated = replay_domain_for(&sample(
        "MiniMax-M3",
        "https://api.minimaxi.com/anthropic/v1/messages",
        4,
    ))
    .unwrap();
    assert_ne!(first.hex(), other_model.hex());
    assert_ne!(first.hex(), other_url.hex());
    assert_ne!(first.hex(), rotated.hex());
}

#[test]
fn replay_domain_ignores_material_outside_the_route_tuple() {
    let mut left = sample(
        "MiniMax-M3",
        "https://api.minimaxi.com/anthropic/v1/messages",
        3,
    );
    let right = sample(
        "MiniMax-M3",
        "https://api.minimaxi.com/anthropic/v1/messages",
        3,
    );
    left.credential_id = "cred-1";
    let secret = "sk-live-should-never-enter-the-domain";
    let domain = replay_domain_for(&left).unwrap();
    assert_eq!(domain.hex(), replay_domain_for(&right).unwrap().hex());
    assert!(
        !domain.hex().contains(secret),
        "the digest is not a copy of credential material"
    );
    assert!(!super::canonical_identity(&left).contains(secret));
}
