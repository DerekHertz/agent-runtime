//! Real-code checks for Stop and SessionEnd reports, the lifecycle side of the TLA+ model
//! `formal/admission/Admission.tla` (kind "L"; see `.agents/skills/formal-verify-admission/
//! SKILL.md`).
//!
//! They drive the real `CoordinatorService` through its public `Coordinator` RPCs with the real
//! SQLite trace and blob store. No socket is bound, so they also run inside the Claude Code
//! sandbox. A lifecycle report stores its fields blob before its one row; a failed write consumes
//! no id, stamp or binding, and the retry takes the failed report's id and stamp. A Stop or
//! SessionEnd leaves every recorded call, completion and binding in place.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use clock::Clock;
use coordinator::adapter::CoordinatorService;
use coordinator::core::RunConfig;
use runtime_proto::v1::after_tool_report::Result as WireResult;
use runtime_proto::v1::coordinator_server::Coordinator;
use runtime_proto::v1::{
    AfterToolReport, BeforeToolRequest, LifecycleEvent, LifecycleReport, ToolSuccess,
};
use tonic::{Code, Request};
use trace_store::{
    read_events, read_payload, BlobHash, Event, EventKind, TraceWriter, BLOBS_DIR, TRACE_FILE,
};

const STOP_FIELDS: &str = r#"{"stop_hook_active":true,"last_assistant_message":"done"}"#;
const END_FIELDS: &str = r#"{"reason":"prompt_input_exit"}"#;

fn service(dir: &Path) -> CoordinatorService {
    let config = RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases: false,
    };
    CoordinatorService::new(&config, TraceWriter::create(dir).unwrap()).unwrap()
}

fn before(slot: &str, session_id: &str, call: &str) -> Request<BeforeToolRequest> {
    Request::new(BeforeToolRequest {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: format!(r#"{{"file_path":"/work/{call}"}}"#),
        intent: String::new(),
        tool_use_id: call.into(),
    })
}

fn after(slot: &str, session_id: &str, call: &str) -> Request<AfterToolReport> {
    Request::new(AfterToolReport {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: format!(r#"{{"file_path":"/work/{call}"}}"#),
        tool_use_id: call.into(),
        result: Some(WireResult::Success(ToolSuccess {
            tool_response_json: "null".into(),
        })),
    })
}

fn lifecycle(slot: &str, session_id: &str, event: i32, fields: &str) -> Request<LifecycleReport> {
    Request::new(LifecycleReport {
        slot: slot.into(),
        session_id: session_id.into(),
        event,
        fields_json: fields.into(),
    })
}

fn stop(slot: &str, session_id: &str) -> Request<LifecycleReport> {
    lifecycle(slot, session_id, LifecycleEvent::Stop as i32, STOP_FIELDS)
}

fn end(slot: &str, session_id: &str) -> Request<LifecycleReport> {
    lifecycle(
        slot,
        session_id,
        LifecycleEvent::SessionEnd as i32,
        END_FIELDS,
    )
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

/// The wire maps each event to its own kind, stores the fields byte for byte under their blake3
/// hash, and ticks the slot once per report.
#[tokio::test]
async fn stop_and_session_end_map_to_their_own_kinds_with_hashed_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.lifecycle(stop("slot-2", "s")).await.unwrap();
    svc.lifecycle(end("slot-2", "s")).await.unwrap();
    let events = trace(tmp.path());
    let got: Vec<_> = events
        .iter()
        .map(|e| (e.event_id, e.kind, stamp(e), e.session_id.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (1, EventKind::Stop, vec![0, 1], "s"),
            (2, EventKind::SessionEnd, vec![0, 2], "s")
        ]
    );
    for (e, fields) in events.iter().zip([STOP_FIELDS, END_FIELDS]) {
        assert_eq!(
            read_payload(tmp.path(), e).unwrap().as_deref(),
            Some(fields.as_bytes())
        );
        assert_eq!(
            e.payload_hash.as_deref(),
            Some(&BlobHash::of(fields.as_bytes()).as_bytes()[..])
        );
        assert_eq!((&e.tool, &e.tool_use_id, e.outcome), (&None, &None, None));
    }
}

/// Model action `BlobFails` on a lifecycle report's fields blob, then the retry: a new session's
/// first event fails, so it binds nothing; the retry takes the same id and stamp.
#[tokio::test]
async fn a_failed_fields_blob_records_nothing_binds_nothing_and_the_retry_reuses_id_and_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    let blobs = tmp.path().join(BLOBS_DIR);
    let files = blob_files(tmp.path());

    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = svc.lifecycle(end("slot-2", "fresh")).await;
    let other = svc.lifecycle(stop("slot-2", "other")).await;
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();

    for err in [failed.unwrap_err(), other.unwrap_err()] {
        assert_eq!(err.code(), Code::Internal);
        assert!(err.message().contains("blob"), "{}", err.message());
    }
    assert_eq!(trace(tmp.path()).len(), 1, "no row for the failed reports");
    assert_eq!(blob_files(tmp.path()), files, "no blob or temporary file");

    // The retry is the event the failed report would have been: id 2, slot-2's first stamp.
    svc.lifecycle(end("slot-2", "fresh")).await.unwrap();
    let retry = trace(tmp.path()).pop().unwrap();
    assert_eq!(
        (retry.event_id, retry.kind, stamp(&retry)),
        (2, EventKind::SessionEnd, vec![0, 1])
    );
    assert_eq!(
        read_payload(tmp.path(), &retry).unwrap().as_deref(),
        Some(END_FIELDS.as_bytes())
    );
    // The failed Stop bound nothing: "other" may still start on slot-1.
    svc.before_tool(before("slot-1", "other", "B"))
        .await
        .unwrap();
    let last = trace(tmp.path()).pop().unwrap();
    assert_eq!((last.event_id, stamp(&last)), (3, vec![2, 0]));
}

/// Model action `AppendFails` for a lifecycle report: the INSERT fails after the fields blob is
/// stored. The orphan blob is harmless and the retry reuses it, its id and its stamp.
#[tokio::test]
async fn a_failed_lifecycle_row_leaves_an_orphan_blob_and_the_retry_reuses_id_and_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    let db = rusqlite::Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    db.execute(
        "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
         VALUES (2, 'planted', 'x', x'', 'BeforeTool', 0)",
        [],
    )
    .unwrap();
    let err = svc.lifecycle(stop("slot-1", "s")).await.unwrap_err();
    assert_eq!(err.code(), Code::Internal);
    let orphan = hex(BlobHash::of(STOP_FIELDS.as_bytes()).as_bytes());
    assert!(blob_files(tmp.path()).contains(&orphan));
    db.execute_batch("DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = 2;")
        .unwrap();

    svc.lifecycle(stop("slot-1", "s")).await.unwrap();
    let events = trace(tmp.path());
    assert_eq!(
        (events[1].event_id, events[1].kind, stamp(&events[1])),
        (2, EventKind::Stop, vec![2, 0])
    );
    assert_eq!(
        blob_files(tmp.path()).len(),
        2,
        "the input blob and the reused fields blob"
    );
    // The call opened before the failed Stop still pairs.
    svc.after_tool(after("slot-1", "s", "A")).await.unwrap();
    assert_eq!(trace(tmp.path())[2].before_event_id, Some(1));
}

/// After Stop and SessionEnd a late completion pairs, a new session on the same slot continues
/// its clock, and the ended session cannot move to another slot.
#[tokio::test]
async fn lifecycle_reports_keep_calls_bindings_and_the_slot_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    svc.before_tool(before("slot-1", "s", "A")).await.unwrap();
    svc.before_tool(before("slot-1", "s", "B")).await.unwrap();
    svc.lifecycle(stop("slot-1", "s")).await.unwrap();
    svc.lifecycle(end("slot-1", "s")).await.unwrap();
    svc.after_tool(after("slot-1", "s", "B")).await.unwrap();
    svc.after_tool(after("slot-1", "s", "A")).await.unwrap();
    svc.before_tool(before("slot-1", "restarted", "A"))
        .await
        .unwrap();
    for refused in [
        svc.lifecycle(end("slot-2", "s")).await.unwrap_err(),
        svc.after_tool(after("slot-2", "s", "A")).await.unwrap_err(),
    ] {
        assert_eq!(refused.code(), Code::FailedPrecondition);
        assert!(
            refused.message().contains("bound to slot"),
            "{}",
            refused.message()
        );
    }
    let denied = svc.before_tool(before("slot-2", "s", "C")).await.unwrap();
    assert!(denied.into_inner().reason.contains("bound to slot"));

    let got: Vec<_> = trace(tmp.path())
        .iter()
        .map(|e| {
            (
                e.event_id,
                e.kind,
                e.session_id.clone(),
                e.before_event_id,
                stamp(e),
            )
        })
        .collect();
    let s = || "s".to_owned();
    assert_eq!(
        got,
        [
            (1, EventKind::BeforeTool, s(), None, vec![1, 0]),
            (2, EventKind::BeforeTool, s(), None, vec![2, 0]),
            (3, EventKind::Stop, s(), None, vec![3, 0]),
            (4, EventKind::SessionEnd, s(), None, vec![4, 0]),
            (5, EventKind::AfterTool, s(), Some(2), vec![5, 0]),
            (6, EventKind::AfterTool, s(), Some(1), vec![6, 0]),
            (
                7,
                EventKind::BeforeTool,
                "restarted".into(),
                None,
                vec![7, 0]
            ),
        ]
    );
}

/// Wire refusals: an unspecified or unknown event is InvalidArgument, an unknown slot, an empty
/// session or empty fields a FailedPrecondition; none records a row or a blob.
#[tokio::test]
async fn malformed_lifecycle_reports_are_refused_without_a_row() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path());
    let cases = [
        (
            lifecycle("slot-1", "s", 0, STOP_FIELDS),
            Code::InvalidArgument,
            "no known event",
        ),
        (
            lifecycle("slot-1", "s", 9, STOP_FIELDS),
            Code::InvalidArgument,
            "no known event",
        ),
        (
            stop("slot-9", "s"),
            Code::FailedPrecondition,
            "unknown slot",
        ),
        (
            end("slot-1", ""),
            Code::FailedPrecondition,
            "empty session_id",
        ),
        (
            lifecycle("slot-1", "s", LifecycleEvent::Stop as i32, ""),
            Code::FailedPrecondition,
            "no lifecycle fields",
        ),
    ];
    for (request, code, why) in cases {
        let err = svc.lifecycle(request).await.unwrap_err();
        assert_eq!(err.code(), code, "{}", err.message());
        assert!(err.message().contains(why), "{}", err.message());
    }
    assert!(trace(tmp.path()).is_empty());
    assert!(blob_files(tmp.path()).is_empty());
    // None of them bound "s": it can start on slot-2.
    svc.lifecycle(stop("slot-2", "s")).await.unwrap();
    assert_eq!(stamp(&trace(tmp.path())[0]), vec![0, 1]);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
