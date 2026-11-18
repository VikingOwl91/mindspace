use mindspace::core::{Refusal, RefusalKind};
use mindspace::ingest::stream::ProducerRefusal;
use mindspace::worker::{broker::FromBroker, client::WorkerError};
use serde_json::json;

#[test]
fn broker_details_cannot_change_the_refusal_kind() {
    for (wire, expected) in [
        (
            json!({"type":"refused","reason":"too_large: busy"}),
            "refused",
        ),
        (
            json!({"type":"refused","kind":"future_policy","reason":"timeout"}),
            "refused",
        ),
        (
            json!({"type":"refused","kind":"busy","reason":"root_not_authorized"}),
            "busy",
        ),
    ] {
        let FromBroker::Refused { kind, reason } = serde_json::from_value(wire).unwrap() else {
            panic!("expected a broker refusal");
        };
        assert_eq!(
            WorkerError::Refused(Refusal::new(kind, reason)).kind(),
            expected
        );
    }
    assert_eq!(
        WorkerError::Stream("path contains limit and SHA-1".into()).kind(),
        "result_refused"
    );
    assert_eq!(
        WorkerError::Extraction(Refusal::new(RefusalKind::TooManyFiles, "short")).kind(),
        "too_many_files"
    );
}

#[test]
fn a_producer_can_report_missing_data_or_limits_but_cannot_report_authority_or_persistence() {
    for name in [
        "missing_local_object",
        "too_many_files",
        "too_large",
        "unsupported_object_format",
    ] {
        assert!(serde_json::from_value::<ProducerRefusal>(json!(name)).is_ok());
    }
    for name in [
        "timeout",
        "revoked",
        "root_not_authorized",
        "commit_outcome_unknown",
        "committed",
    ] {
        assert!(serde_json::from_value::<ProducerRefusal>(json!(name)).is_err());
    }
}

#[test]
fn terminal_worker_refusal_is_bound_to_the_stream_operation() {
    use mindspace::core::Uuid;
    use mindspace::ingest::stream::{consume, Kind, StreamLimits, PROTOCOL_VERSION};
    let op = Uuid::now_v7();
    let rejected = |wire_op, reason| {
        let payload = serde_json::to_vec(&json!({
            "v": PROTOCOL_VERSION,
            "op": wire_op,
            "seq": 0,
            "records": [
                {"type":"begin","kind":"git_verified"},
                {"type":"producer_refused","reason":reason}
            ]
        }))
        .unwrap();
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend(payload);
        consume(
            &frame[..],
            op,
            Kind::GitVerified,
            None,
            &StreamLimits::default(),
            None,
        )
        .expect_err("a refusal must never yield a complete extraction")
    };
    assert_eq!(
        rejected(op, "missing_local_object").refusal_kind(),
        Some(RefusalKind::MissingLocalObject)
    );
    assert_eq!(
        rejected(Uuid::now_v7(), "missing_local_object").refusal_kind(),
        None
    );
    assert_eq!(rejected(op, "revoked").refusal_kind(), None);
}
