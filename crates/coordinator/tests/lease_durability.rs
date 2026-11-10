//! Leases against the real trace: the lease table is part of the state the adapter commits only
//! after the event is appended, so a write whose row fails to persist takes no lease, and a Deny
//! whose row fails to persist is not remembered. Drives the real `CoordinatorService` with the
//! real SQLite trace and blob store; no socket is bound.

use std::path::Path;

use coordinator::adapter::CoordinatorService;
use coordinator::core::RunConfig;
use runtime_proto::v1::admission::Verdict;
use runtime_proto::v1::coordinator_server::Coordinator;
use runtime_proto::v1::{Admission, BeforeToolRequest};
use tonic::{Code, Request};
use trace_store::{
    read_events, read_input_payload, read_payload, EventKind, TraceWriter, TRACE_FILE,
};

const INPUT: &str = r#"{"file_path":"/work/src/lib.rs"}"#;

fn service(dir: &Path) -> CoordinatorService {
    let config = RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases: true,
    };
    CoordinatorService::new(&config, TraceWriter::create(dir).unwrap()).unwrap()
}

fn edit(slot: &str, call: &str) -> Request<BeforeToolRequest> {
    Request::new(BeforeToolRequest {
        slot: slot.into(),
        session_id: format!("sess-{slot}"),
        tool: "Edit".into(),
        tool_input_json: INPUT.into(),
        intent: String::new(),
        tool_use_id: call.into(),
    })
}

async fn admit(svc: &CoordinatorService, slot: &str, call: &str) -> Admission {
    svc.before_tool(edit(slot, call))
        .await
        .unwrap()
        .into_inner()
}

/// Plant a row with the next event id so the coordinator's next INSERT fails, run `failing`, then
/// remove the planted row.
async fn with_failed_append<F: std::future::Future>(
    dir: &Path,
    next_id: u64,
    failing: F,
) -> F::Output {
    let db = rusqlite::Connection::open(dir.join(TRACE_FILE)).unwrap();
    db.execute(
        &format!(
            "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
             VALUES ({next_id}, 'planted', 'x', x'', 'BeforeTool', 0)"
        ),
        [],
    )
    .unwrap();
    let out = failing.await;
    db.execute_batch(&format!(
        "DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = {next_id};
         CREATE TRIGGER events_no_delete BEFORE DELETE ON events
         BEGIN SELECT RAISE(ABORT, 'trace is append-only'); END;"
    ))
    .unwrap();
    out
}

#[tokio::test]
async fn a_write_whose_row_fails_takes_no_lease() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    let err = with_failed_append(tmp.path(), 1, svc.before_tool(edit("slot-1", "a")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);

    // slot-1 never got the file, so slot-2 takes it, and slot-1's retry is the one denied.
    assert_eq!(
        admit(&svc, "slot-2", "b").await.verdict,
        Verdict::Allow as i32
    );
    let denied = admit(&svc, "slot-1", "a").await;
    assert_eq!(
        (denied.verdict, denied.owner.as_str()),
        (Verdict::Deny as i32, "slot-2")
    );
}

#[tokio::test]
async fn a_deny_is_stored_with_its_reason_and_a_failed_deny_is_not_remembered() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    assert_eq!(
        admit(&svc, "slot-1", "a").await.verdict,
        Verdict::Allow as i32
    );

    let err = with_failed_append(tmp.path(), 2, svc.before_tool(edit("slot-2", "b")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);

    // The retry of the same call is a new call, not a repeat, and takes the failed one's id.
    let denied = admit(&svc, "slot-2", "b").await;
    assert_eq!(denied.verdict, Verdict::Deny as i32);
    let events = read_events(&tmp.path().join(TRACE_FILE)).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| (e.event_id, e.kind))
            .collect::<Vec<_>>(),
        [(1, EventKind::BeforeTool), (2, EventKind::Deny)]
    );
    let deny = &events[1];
    assert_eq!(deny.path.as_deref(), Some("/work/src/lib.rs"));
    let reason: String =
        serde_json::from_slice(&read_payload(tmp.path(), deny).unwrap().unwrap()).unwrap();
    assert_eq!(reason, denied.reason);
    assert!(reason.contains("leased by slot-1"), "{reason}");
    assert_eq!(
        read_input_payload(tmp.path(), deny).unwrap().unwrap(),
        INPUT.as_bytes()
    );
}
