//! Real-code checks for failed and interrupted AfterTool reports (PostToolUseFailure), the
//! failure side of the TLA+ model `formal/admission/Admission.tla` (see
//! `.agents/skills/formal-verify-admission/SKILL.md`).
//!
//! They drive the real `CoordinatorService` through its public `Coordinator` RPCs with the real
//! SQLite trace and blob store. No socket is bound, so they also run inside the Claude Code
//! sandbox. A failure report stores its raw error and its raw input as blobs before its one row;
//! a failed write consumes no id, stamp, binding or completion, and the corrected retry takes the
//! failed report's id and stamp and still pairs with the call's BeforeTool.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use clock::Clock;
use coordinator::adapter::CoordinatorService;
use coordinator::core::RunConfig;
use runtime_proto::v1::after_tool_report::Result as WireResult;
use runtime_proto::v1::coordinator_server::Coordinator;
use runtime_proto::v1::{AfterToolReport, BeforeToolRequest, ToolFailure, ToolSuccess};
use tonic::{Code, Request};
use trace_store::{
    read_events, read_input_payload, read_payload, BlobHash, Event, EventKind, Outcome,
    TraceWriter, BLOBS_DIR, TRACE_FILE,
};

fn service(dir: &Path) -> CoordinatorService {
    let config = RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases: false,
    };
    CoordinatorService::new(&config, TraceWriter::create(dir).unwrap()).unwrap()
}

fn input_of(call: &str) -> String {
    format!(r#"{{"command":"run {call}"}}"#)
}

fn before(slot: &str, session_id: &str, call: &str) -> Request<BeforeToolRequest> {
    Request::new(BeforeToolRequest {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Bash".into(),
        tool_input_json: input_of(call),
        intent: String::new(),
        tool_use_id: call.into(),
    })
}

fn report(slot: &str, session_id: &str, call: &str, result: WireResult) -> AfterToolReport {
    AfterToolReport {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Bash".into(),
        tool_input_json: input_of(call),
        tool_use_id: call.into(),
        result: Some(result),
    }
}

fn failure(error_json: &str, is_interrupt: Option<bool>) -> WireResult {
    WireResult::Failure(ToolFailure {
        error_json: error_json.into(),
        is_interrupt,
    })
}

fn success(response: &str) -> WireResult {
    WireResult::Success(ToolSuccess {
        tool_response_json: response.into(),
    })
}

fn trace(dir: &Path) -> Vec<Event> {
    read_events(&dir.join(TRACE_FILE)).unwrap()
}

fn stamp(e: &Event) -> Vec<u64> {
    Clock::from_bytes(&e.vclock).unwrap().entries().to_vec()
}

fn blob_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join(BLOBS_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn plant_row(dir: &Path, event_id: i64) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(dir.join(TRACE_FILE)).unwrap();
    db.execute(
        "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
         VALUES (?1, 'planted', 'x', x'', 'BeforeTool', 0)",
        [event_id],
    )
    .unwrap();
    db
}

fn unplant_row(db: &rusqlite::Connection, event_id: i64) {
    db.execute_batch("DROP TRIGGER IF EXISTS events_no_delete")
        .unwrap();
    db.execute("DELETE FROM events WHERE event_id = ?1", [event_id])
        .unwrap();
}

/// The `(outcome, is_interrupt)` columns as stored, read with plain SQL.
fn stored_outcomes(dir: &Path) -> Vec<(Option<String>, Option<i64>)> {
    let db = rusqlite::Connection::open(dir.join(TRACE_FILE)).unwrap();
    let mut stmt = db
        .prepare("SELECT outcome, is_interrupt FROM events ORDER BY event_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows
}

/// Model action `BlobFails` on a failure's error blob, then the corrected retry. The run's
/// `blobs/` is made read-only, so the real `BlobStore::put` cannot store the new error.
#[tokio::test]
async fn a_failed_error_blob_records_nothing_and_the_retry_pairs_with_the_same_id() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    let blobs = tmp.path().join(BLOBS_DIR);
    let files = blob_files(tmp.path());
    let failed_report = || report("slot-1", "s", "A", failure(r#""Exit code 2""#, Some(true)));

    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = svc.after_tool(Request::new(failed_report())).await;
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = failed.expect_err("a failed blob write is an error, never an Ack");
    assert_eq!(err.code(), Code::Internal);
    assert!(err.message().contains("blob"), "{}", err.message());
    assert_eq!(trace(tmp.path()).len(), 1, "no row for the failed report");
    assert_eq!(blob_files(tmp.path()), files, "no blob or temporary file");

    svc.after_tool(Request::new(failed_report())).await.unwrap();
    let events = trace(tmp.path());
    let retry = &events[1];
    assert_eq!(retry.kind, EventKind::AfterTool);
    assert_eq!(
        (retry.event_id, stamp(retry), retry.before_event_id),
        (2, vec![2, 0], Some(1)),
        "the retry takes the failed report's id and stamp and still pairs"
    );
    assert_eq!(
        retry.outcome,
        Some(Outcome::Failure {
            is_interrupt: Some(true)
        })
    );
    assert_eq!(
        read_payload(tmp.path(), retry).unwrap().as_deref(),
        Some(&br#""Exit code 2""#[..]),
        "the payload is the raw error, not an envelope"
    );
    assert_eq!(
        read_input_payload(tmp.path(), retry).unwrap(),
        Some(input_of("A").into_bytes())
    );
}

/// Model action `AppendFails` after both blobs of a failure: the INSERT collides with a planted
/// row. Both blobs stay as orphans, and the retries reuse them and pair with the same ids.
#[tokio::test]
async fn a_failed_failure_row_leaves_orphan_blobs_and_the_retry_pairs() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    svc.before_tool(before("slot-1", "s", "B")).await.unwrap();
    let db = plant_row(tmp.path(), 3);
    let b = || report("slot-1", "s", "B", failure(r#""""#, None));
    for _ in 0..2 {
        let err = svc.after_tool(Request::new(b())).await.unwrap_err();
        assert_eq!(err.code(), Code::Internal);
    }
    let orphan = BlobHash::of(br#""""#).to_string();
    assert!(
        blob_files(tmp.path()).contains(&orphan),
        "the orphan error blob"
    );
    unplant_row(&db, 3);

    svc.after_tool(Request::new(b())).await.unwrap();
    let a = report("slot-1", "s", "A", failure(r#""stopped""#, Some(false)));
    svc.after_tool(Request::new(a)).await.unwrap();
    let events = trace(tmp.path());
    let got: Vec<_> = events
        .iter()
        .map(|e| (e.event_id, e.kind, e.before_event_id, stamp(e), e.outcome))
        .collect();
    let failed = |is_interrupt| Some(Outcome::Failure { is_interrupt });
    assert_eq!(
        got,
        vec![
            (1, EventKind::BeforeTool, None, vec![1, 0], None),
            (2, EventKind::BeforeTool, None, vec![2, 0], None),
            (3, EventKind::AfterTool, Some(2), vec![3, 0], failed(None)),
            (
                4,
                EventKind::AfterTool,
                Some(1),
                vec![4, 0],
                failed(Some(false))
            ),
        ],
        "reverse completion pairs exactly; the failed writes used no id or stamp"
    );
    // An omitted flag is stored as NULL, an explicit false as 0.
    assert_eq!(
        stored_outcomes(tmp.path()),
        vec![
            (None, None),
            (None, None),
            (Some("failure".into()), None),
            (Some("failure".into()), Some(0)),
        ]
    );
    assert_eq!(
        read_payload(tmp.path(), &events[2]).unwrap().as_deref(),
        Some(&br#""""#[..]),
        "an empty error message is the JSON text \"\""
    );
}

/// A failed orphan failure on a new session binds nothing: the corrected retry is recorded on
/// the other slot as event 1 with no BeforeTool, keeping its input and error.
#[tokio::test]
async fn a_failed_orphan_failure_binds_no_session() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    let db = plant_row(tmp.path(), 1);
    let orphan = |slot: &str| report(slot, "fresh", "O", failure(r#""lost""#, None));
    let err = svc
        .after_tool(Request::new(orphan("slot-2")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);
    unplant_row(&db, 1);

    svc.after_tool(Request::new(orphan("slot-1")))
        .await
        .unwrap();
    let events = trace(tmp.path());
    assert_eq!(events.len(), 1);
    let o = &events[0];
    assert_eq!(
        (o.slot.as_str(), o.before_event_id, stamp(o)),
        ("slot-1", None, vec![1, 0])
    );
    assert_eq!(o.outcome, Some(Outcome::Failure { is_interrupt: None }));
    assert_eq!(
        read_input_payload(tmp.path(), o).unwrap(),
        Some(input_of("O").into_bytes())
    );
    assert_eq!(
        read_payload(tmp.path(), o).unwrap(),
        Some(br#""lost""#.to_vec())
    );
}

/// Mixed histories: a success after a failure and a failure after a success are both recorded
/// as rows of their own naming the same BeforeTool, and neither row is changed.
#[tokio::test]
async fn mixed_outcomes_are_all_recorded_and_pair_with_their_own_before() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-2", "s", "F")).await.unwrap();
    svc.before_tool(before("slot-2", "s", "S")).await.unwrap();
    for (call, result) in [
        ("F", failure(r#""first""#, Some(false))),
        ("S", success("{}")),
        ("F", success("{}")),
        ("S", failure(r#""late""#, Some(true))),
    ] {
        svc.after_tool(Request::new(report("slot-2", "s", call, result)))
            .await
            .unwrap();
    }
    let events = trace(tmp.path());
    let got: Vec<_> = events[2..]
        .iter()
        .map(|e| {
            (
                e.event_id,
                e.tool_use_id.clone().unwrap(),
                e.before_event_id,
                e.outcome,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (
                3,
                "F".into(),
                Some(1),
                Some(Outcome::Failure {
                    is_interrupt: Some(false)
                })
            ),
            (4, "S".into(), Some(2), Some(Outcome::Success)),
            (5, "F".into(), Some(1), Some(Outcome::Success)),
            (
                6,
                "S".into(),
                Some(2),
                Some(Outcome::Failure {
                    is_interrupt: Some(true)
                })
            ),
        ]
    );
    assert_eq!(stamp(&events[5]), vec![0, 6]);
}

/// Wire refusals of a failure: an empty error field and another tool are FailedPrecondition
/// with no row and no blob; an explicit empty message (`""`) is recorded.
#[tokio::test]
async fn a_failure_without_an_error_or_with_another_tool_is_refused_without_a_row() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    let files = blob_files(tmp.path());

    let no_error = report("slot-1", "s", "A", failure("", Some(true)));
    let err = svc.after_tool(Request::new(no_error)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert!(err.message().contains("no error"), "{}", err.message());
    let mut edit = report("slot-1", "s", "A", failure(r#""x""#, None));
    edit.tool = "Edit".into();
    let err = svc.after_tool(Request::new(edit)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert!(err.message().contains("Bash"), "{}", err.message());
    let default = report(
        "slot-1",
        "s",
        "A",
        WireResult::Failure(ToolFailure::default()),
    );
    let err = svc.after_tool(Request::new(default)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(trace(tmp.path()).len(), 1);
    assert_eq!(blob_files(tmp.path()), files);

    let empty_message = report("slot-1", "s", "A", failure(r#""""#, Some(false)));
    svc.after_tool(Request::new(empty_message)).await.unwrap();
    let events = trace(tmp.path());
    assert_eq!(
        (events[1].event_id, events[1].before_event_id),
        (2, Some(1))
    );
}
