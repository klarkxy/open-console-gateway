use super::*;
use ocg_domain::destination::{
    LegacyDestinationFacts, Protocol as DomainProtocol, destination_from_legacy,
};

#[test]
fn http_receipt_projects_saved_override_and_disabled_actions() {
    let destination = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: "custom".into(),
        name: "Custom".into(),
        endpoint_url: "https://example.com/v1".into(),
        protocol: DomainProtocol::ChatCompletions,
        model_capabilities: vec![("public".into(), "upstream".into())],
    })
    .unwrap();
    let mut dto = DestinationDto::from(&destination);
    dto.catalog[0].enabled = false;
    dto.catalog[0].protocols = vec![ProtocolDto::Messages];
    dto.catalog[0].preferred = Some(ProtocolDto::Messages);
    dto.catalog[0].upstream_override = Some(
        crate::dashboard_v4::types::DestinationUpstreamOverridePatch {
            protocol: ProtocolDto::Messages,
            endpoint_url: "https://other.example/v1/messages".into(),
        },
    );
    let presentation = http_presentation(&dto).unwrap();
    assert_eq!(presentation.total, 1);
    assert!(presentation.all_disabled);
    let row = &presentation.models[0];
    assert_eq!(row.public_model, "public");
    assert_eq!(row.upstream_model, "upstream");
    assert_eq!(row.target_protocol, Some(Protocol::Messages));
    assert_eq!(row.test_protocol, None);
    assert_eq!(row.writable_protocols, vec![Protocol::Messages]);
    assert!(!row.effective_on);
    assert!(row.actions.iter().any(|a| a.key == "toggle" && a.allowed));
    assert!(row.actions.iter().any(|a| a.key == "test" && !a.allowed));
    dto.catalog[0].enabled = true;
    let enabled = http_presentation(&dto).unwrap();
    assert!(!enabled.all_disabled);
    assert_eq!(enabled.models[0].test_protocol, Some(Protocol::Messages));
    assert!(
        enabled.models[0]
            .actions
            .iter()
            .any(|a| a.key == "test" && a.allowed)
    );
}
