use super::*;

#[test]
fn send_failure_kinds_keep_stage0_classification() {
    let timeout = TransportSendFailure::from_send_error(false, true, "operation timed out");
    assert_eq!(
        classify_transport(timeout.kind.into()),
        ProviderErrorClass::OutcomeUnknown
    );
    assert!(timeout.timed_out);
    assert!(
        timeout.message.starts_with("upstream request timed out:"),
        "{}",
        timeout.message
    );

    let connect = TransportSendFailure::from_send_error(true, false, "connection refused");
    assert_eq!(
        classify_transport(connect.kind.into()),
        ProviderErrorClass::Connect
    );
    assert!(!connect.timed_out);
    assert!(
        connect.message.starts_with("upstream request failed:"),
        "{}",
        connect.message
    );

    let other = TransportSendFailure::from_send_error(false, false, "connection reset");
    assert_eq!(
        classify_transport(other.kind.into()),
        ProviderErrorClass::OutcomeUnknown
    );
    assert!(!other.timed_out);

    let connect_timeout =
        TransportSendFailure::from_send_error(true, true, "error trying to connect");
    assert_eq!(
        classify_transport(connect_timeout.kind.into()),
        ProviderErrorClass::Connect
    );
    assert!(connect_timeout.timed_out);
    assert!(
        connect_timeout
            .message
            .starts_with("upstream request timed out:"),
        "{}",
        connect_timeout.message
    );
}
