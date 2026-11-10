//! Real-code checks for the AfterTool path, the completion side of the TLA+ model
//! `formal/admission/Admission.tla` (see `.agents/skills/formal-verify-admission/SKILL.md`).
//!
//! They drive the real `CoordinatorService` through its public `Coordinator` RPCs with the real
//! SQLite trace and blob store. No socket is bound, so they also run inside the Claude Code
//! sandbox. Each failure is injected in the real file system or database, and each retry must
//! take the failed report's event id and stamp and still pair with the original BeforeTool:
//! a failed write consumes no id, no stamp, no session binding and no completion.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use clock::Clock;
use coordinator::adapter::CoordinatorService;
use coordinator::core::RunConfig;
use runtime_proto::v1::after_tool_report::Result as WireResult;
use runtime_proto::v1::coordinator_server::Coordinator;
use runtime_proto::v1::{AfterToolReport, BeforeToolRequest, ToolSuccess};
use tonic::{Code, Request};
use trace_store::{
    read_events, read_input_payload, read_payload, BlobHash, Event, EventKind, TraceWriter,
    BLOBS_DIR, TRACE_FILE,
};

fn service(dir: &Path) -> CoordinatorService {
    let config = RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases: false,
    };
    CoordinatorService::new(&config, TraceWriter::create(dir).unwrap()).unwrap()
}

fn input_of(call: &str) -> String {
    format!(r#"{{"call":"{call}"}}"#)
}

fn before(slot: &str, session_id: &str, call: &str) -> Request<BeforeToolRequest> {
    Request::new(BeforeToolRequest {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: input_of(call),
        intent: String::new(),
        tool_use_id: call.into(),
    })
}

fn report(slot: &str, session_id: &str, call: &str, response: &str) -> AfterToolReport {
    AfterToolReport {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: input_of(call),
        tool_use_id: call.into(),
        result: Some(WireResult::Success(ToolSuccess {
            tool_response_json: response.into(),
        })),
    }
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

/// Model actions `BlobFails` on an AfterTool's response blob, then a retry. The run's `blobs/`
/// is made read-only, so the real `BlobStore::put` cannot store the new response.
#[tokio::test]
async fn a_failed_response_blob_records_nothing_and_the_retry_still_pairs() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    let blobs = tmp.path().join(BLOBS_DIR);
    let files = blob_files(tmp.path());

    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = svc
        .after_tool(Request::new(report("slot-1", "s", "A", "1")))
        .await;
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = failed.expect_err("a failed blob write is an error, never an Ack");
    assert_eq!(err.code(), Code::Internal);
    assert!(err.message().contains("blob"), "{}", err.message());
    assert_eq!(trace(tmp.path()).len(), 1, "no row for the failed report");
    assert_eq!(blob_files(tmp.path()), files, "no blob or temporary file");

    svc.after_tool(Request::new(report("slot-1", "s", "A", "1")))
        .await
        .unwrap();
    let events = trace(tmp.path());
    let retry = &events[1];
    assert_eq!(retry.kind, EventKind::AfterTool);
    assert_eq!((retry.event_id, stamp(retry)), (2, vec![2, 0]));
    assert_eq!(retry.before_event_id, Some(1), "the retry still pairs");
    assert_eq!(
        read_payload(tmp.path(), retry).unwrap().as_deref(),
        Some(&b"1"[..])
    );
}

/// The response blob is already stored, the input blob is new and fails: both blobs are put
/// before the row, so the second failure still leaves no row.
#[tokio::test]
async fn a_failed_input_blob_after_the_response_blob_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    // An orphan report stores the response "same" first.
    svc.after_tool(Request::new(report("slot-1", "s", "O", "\"same\"")))
        .await
        .unwrap();
    let files = blob_files(tmp.path());
    // A's input was rewritten by a hook, so its input blob is new; its response is not.
    let mut rewritten = report("slot-1", "s", "A", "\"same\"");
    rewritten.tool_input_json = r#"{"call":"A","rewritten":1}"#.into();

    let blobs = tmp.path().join(BLOBS_DIR);
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = svc.after_tool(Request::new(rewritten.clone())).await;
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(failed.unwrap_err().code(), Code::Internal);
    assert_eq!(trace(tmp.path()).len(), 2);
    assert_eq!(blob_files(tmp.path()), files);

    svc.after_tool(Request::new(rewritten)).await.unwrap();
    let events = trace(tmp.path());
    let a = &events[2];
    assert_eq!(
        (a.event_id, a.before_event_id, stamp(a)),
        (3, Some(1), vec![3, 0])
    );
    assert_eq!(
        read_input_payload(tmp.path(), a).unwrap().as_deref(),
        Some(&br#"{"call":"A","rewritten":1}"#[..])
    );
    // Both versions of the input are kept: the BeforeTool's and the reported one.
    assert_eq!(
        read_payload(tmp.path(), &events[0]).unwrap(),
        Some(input_of("A").into_bytes())
    );
}

/// Model action `AppendFails` after both blobs: the INSERT collides with a planted row. Both
/// blobs stay as orphans, and the retry reuses them, its id and stamp, and still pairs.
#[tokio::test]
async fn a_failed_row_leaves_orphan_blobs_and_the_retry_pairs_with_the_same_id_and_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    svc.before_tool(before("slot-1", "s", "B")).await.unwrap();
    let db = plant_row(tmp.path(), 3);

    for _ in 0..2 {
        let err = svc
            .after_tool(Request::new(report("slot-1", "s", "B", "\"b\"")))
            .await
            .expect_err("the INSERT collides");
        assert_eq!(err.code(), Code::Internal);
    }
    let orphan = BlobHash::of(b"\"b\"").to_string();
    assert!(blob_files(tmp.path()).contains(&orphan), "the orphan blob");
    assert_eq!(
        trace(tmp.path()).len(),
        3,
        "two BeforeTools and the planted row"
    );
    unplant_row(&db, 3);

    let files = blob_files(tmp.path());
    svc.after_tool(Request::new(report("slot-1", "s", "B", "\"b\"")))
        .await
        .unwrap();
    svc.after_tool(Request::new(report("slot-1", "s", "A", "\"a\"")))
        .await
        .unwrap();
    let events = trace(tmp.path());
    let got: Vec<_> = events
        .iter()
        .map(|e| (e.event_id, e.kind, e.before_event_id, stamp(e)))
        .collect();
    assert_eq!(
        got,
        vec![
            (1, EventKind::BeforeTool, None, vec![1, 0]),
            (2, EventKind::BeforeTool, None, vec![2, 0]),
            (3, EventKind::AfterTool, Some(2), vec![3, 0]),
            (4, EventKind::AfterTool, Some(1), vec![4, 0]),
        ],
        "reverse completion pairs exactly; the failures used no id or stamp"
    );
    // The retry reused the orphan blobs and added only A's response.
    let mut want = files;
    want.push(BlobHash::of(b"\"a\"").to_string());
    want.sort();
    assert_eq!(blob_files(tmp.path()), want);
}

/// A failed orphan report on a new session binds nothing: the session can still start on the
/// other slot, and the orphan is then recorded there with no BeforeTool.
#[tokio::test]
async fn a_failed_orphan_report_binds_no_session() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    let db = plant_row(tmp.path(), 1);
    let err = svc
        .after_tool(Request::new(report("slot-2", "fresh", "O", "null")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);
    unplant_row(&db, 1);

    svc.after_tool(Request::new(report("slot-1", "fresh", "O", "null")))
        .await
        .unwrap();
    let events = trace(tmp.path());
    assert_eq!(events.len(), 1);
    let o = &events[0];
    assert_eq!(
        (o.slot.as_str(), o.before_event_id, stamp(o)),
        ("slot-1", None, vec![1, 0])
    );
    assert_eq!(
        read_input_payload(tmp.path(), o).unwrap(),
        Some(input_of("O").into_bytes())
    );
    assert_eq!(read_payload(tmp.path(), o).unwrap(), Some(b"null".to_vec()));
}

/// A report without a result is refused at the wire, and a refused report (another tool) is a
/// FailedPrecondition: neither records anything nor uses an id.
#[tokio::test]
async fn reports_without_a_result_or_with_another_tool_are_refused_without_a_row() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();

    let mut no_result = report("slot-1", "s", "A", "1");
    no_result.result = None;
    let err = svc.after_tool(Request::new(no_result)).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    let mut empty = report("slot-1", "s", "A", "1");
    empty.result = Some(WireResult::Success(ToolSuccess::default()));
    let err = svc.after_tool(Request::new(empty)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    let mut bash = report("slot-1", "s", "A", "1");
    bash.tool = "Bash".into();
    let err = svc.after_tool(Request::new(bash)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert!(err.message().contains("Edit"), "{}", err.message());
    assert_eq!(trace(tmp.path()).len(), 1);

    svc.after_tool(Request::new(report("slot-1", "s", "A", "1")))
        .await
        .unwrap();
    let events = trace(tmp.path());
    assert_eq!(
        (events[1].event_id, events[1].before_event_id),
        (2, Some(1))
    );
}

/// An orphan report with an empty tool is refused before it claims the call or binds the
/// session, so the corrected retry is recorded as the call's first event on either slot.
#[tokio::test]
async fn an_orphan_with_no_tool_claims_nothing_and_the_corrected_retry_is_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    let mut no_tool = report("slot-2", "fresh", "O", "null");
    no_tool.tool = String::new();
    let err = svc.after_tool(Request::new(no_tool)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert!(err.message().contains("no tool"), "{}", err.message());
    assert!(trace(tmp.path()).is_empty());
    let blobs = tmp.path().join(BLOBS_DIR);
    assert!(
        !blobs.exists() || blob_files(tmp.path()).is_empty(),
        "no blob written"
    );

    // Not bound to slot-2, and the call is not claimed under "".
    svc.after_tool(Request::new(report("slot-1", "fresh", "O", "null")))
        .await
        .unwrap();
    let events = trace(tmp.path());
    assert_eq!(events.len(), 1);
    let o = &events[0];
    assert_eq!(
        (o.event_id, o.slot.as_str(), o.tool.as_deref()),
        (1, "slot-1", Some("Edit"))
    );
    assert_eq!((o.before_event_id, stamp(o)), (None, vec![1, 0]));
}
