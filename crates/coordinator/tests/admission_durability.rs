//! Real-code checks for the admission path, one for each rule of the TLA+ model
//! `formal/admission/Admission.tla` (see `.agents/skills/formal-verify-admission/SKILL.md`).
//!
//! They drive the real `CoordinatorService` through its public `Coordinator::before_tool`, with the
//! real SQLite trace. No socket is bound, so they also run inside the Claude Code sandbox.
//!
//! Rules (plain words):
//! (a) every Allow a shim gets has its BeforeTool event already in the trace;
//! (b) the trace's event ids are 1, 2, 3, ...: unique, increasing, none skipped;
//! (c) a failed append leaves the core state alone, so the retry reuses the same id and stamp;
//! (d) each slot's stamps strictly increase;
//! (e) concurrent calls never make each other fail;
//! (f) every payload a row names is a stored blob: the blob is written before its row, a failed
//!     blob write leaves no row and the state alone, and a failed row leaves at most an orphan
//!     blob that no row names;
//! (g) a session binds to one slot and a `(session_id, tool_use_id)` is recorded at most once,
//!     and a failed append binds no session and uses no call identity.

use std::path::Path;
use std::sync::Arc;

use clock::Clock;
use coordinator::adapter::CoordinatorService;
use coordinator::core::RunConfig;
use runtime_proto::v1::admission::Verdict;
use runtime_proto::v1::coordinator_server::Coordinator;
use runtime_proto::v1::BeforeToolRequest;
use tonic::{Code, Request};
use trace_store::{read_events, read_payload, BlobHash, Event, TraceWriter, BLOBS_DIR, TRACE_FILE};

fn service(dir: &Path, slots: &[&str]) -> CoordinatorService {
    let config = RunConfig {
        slots: slots.iter().map(|s| s.to_string()).collect(),
        leases: false,
    };
    CoordinatorService::new(&config, TraceWriter::create(dir).unwrap()).unwrap()
}

/// A call from `slot`'s session whose `tool_use_id` and raw tool input both name it, so a trace
/// row can be matched to the call that wrote it.
fn request(slot: &str, call: &str) -> Request<BeforeToolRequest> {
    request_in(slot, &format!("session-of-{slot}"), call)
}

fn request_in(slot: &str, session_id: &str, call: &str) -> Request<BeforeToolRequest> {
    Request::new(BeforeToolRequest {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: raw_input(call),
        intent: String::new(),
        tool_use_id: call.into(),
    })
}

fn trace(dir: &Path) -> Vec<Event> {
    read_events(&dir.join(TRACE_FILE)).unwrap()
}

fn raw_input(call: &str) -> String {
    format!(r#"{{"call":"{call}"}}"#)
}

/// Whether one of `events` is the row of `call`, by its payload read back from its blob. Every
/// row's payload must resolve: a row naming a missing blob fails the test (rule f).
fn has_call(dir: &Path, events: &[Event], call: &str) -> bool {
    let want = raw_input(call).into_bytes();
    events.iter().any(|e| {
        let payload = read_payload(dir, e).expect("every row's payload blob resolves");
        payload.as_deref() == Some(want.as_slice())
    })
}

fn blob_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join(BLOBS_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn stamp(e: &Event) -> Vec<u64> {
    Clock::from_bytes(&e.vclock).unwrap().entries().to_vec()
}

/// Model action `Reply` after `AppendOk` + `Commit` + `Unlock`; rule (a), checked at reply time.
#[tokio::test]
async fn an_allow_has_its_event_in_the_trace_when_the_reply_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &["slot-1", "slot-2"]);
    for (n, slot) in ["slot-1", "slot-2", "slot-1", "slot-1"].iter().enumerate() {
        let call = format!("c{n}");
        let adm = svc
            .before_tool(request(slot, &call))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(adm.verdict, Verdict::Allow as i32);
        let events = trace(tmp.path());
        assert!(
            has_call(tmp.path(), &events, &call),
            "Allow for {call} before its event"
        );
        assert_eq!(events.len(), n + 1);
    }
}

/// Model actions `AppendFails` (nothing written, lock freed, state unchanged) then a retry that
/// takes the same id and stamp; rules (b), (c) and (d). The conflicting row is planted as the
/// adapter's own unit test does, then removed so the retry can succeed.
#[tokio::test]
async fn a_failed_append_leaves_the_state_alone_and_the_retry_reuses_id_and_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &["slot-1", "slot-2"]);
    let db = rusqlite::Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    db.execute(
        "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
         VALUES (2, 'planted', 'x', x'', 'BeforeTool', 0)",
        [],
    )
    .unwrap();

    let first = svc
        .before_tool(request("slot-1", "first"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first.verdict, Verdict::Allow as i32);

    // slot-2's call would take id 2 and stamp [0, 1], and collides with the planted row.
    let failed = svc.before_tool(request("slot-2", "failed")).await;
    let err = failed.expect_err("a failed append must be an error, never an Allow");
    assert_eq!(err.code(), Code::Internal);
    assert!(!has_call(tmp.path(), &trace(tmp.path()), "failed"));

    // A second failure changes nothing either: still the same would-be id and stamp.
    assert!(svc
        .before_tool(request("slot-2", "failed-again"))
        .await
        .is_err());

    // Remove the conflict (the table refuses DELETE by trigger, so drop it first).
    db.execute_batch("DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = 2;")
        .unwrap();

    let retry = svc
        .before_tool(request("slot-2", "retry"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry.verdict, Verdict::Allow as i32);
    let after = svc
        .before_tool(request("slot-1", "after"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(after.verdict, Verdict::Allow as i32);

    let events = trace(tmp.path());
    let ids: Vec<u64> = events.iter().map(|e| e.event_id).collect();
    assert_eq!(ids, vec![1, 2, 3], "ids consecutive, none skipped");
    assert!(
        has_call(tmp.path(), &events[1..2], "retry"),
        "the retry took id 2"
    );
    assert_eq!(stamp(&events[0]), vec![1, 0]);
    assert_eq!(
        stamp(&events[1]),
        vec![0, 1],
        "slot-2's clock was not ticked by the failures"
    );
    assert_eq!(stamp(&events[2]), vec![2, 0]);
}

/// Model action `NoRecord`: an unknown slot is denied, writes nothing and uses no id.
#[tokio::test]
async fn an_unknown_slot_is_denied_without_a_row_or_an_id() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &["slot-1"]);
    let denied = svc
        .before_tool(request("nobody", "ghost"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(denied.verdict, Verdict::Deny as i32);
    assert!(trace(tmp.path()).is_empty());

    svc.before_tool(request("slot-1", "real")).await.unwrap();
    let events = trace(tmp.path());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_id, 1);
}

/// An empty tool is invalid wire input, refused like the model's `NoRecord`: a Deny, no row, no
/// blob, no id, and the call's `(session_id, tool_use_id)` stays free for the corrected retry.
#[tokio::test]
async fn an_empty_tool_is_denied_without_a_row_and_its_retry_is_event_one() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &["slot-1", "slot-2"]);
    let mut bad = request("slot-1", "call-1").into_inner();
    bad.tool = String::new();
    let denied = svc
        .before_tool(Request::new(bad))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(denied.verdict, Verdict::Deny as i32);
    assert!(denied.reason.contains("no tool"), "{}", denied.reason);
    assert!(trace(tmp.path()).is_empty());
    assert!(blob_files(tmp.path()).is_empty());

    // Same session and tool_use_id, other slot: the session was not bound, the call not claimed.
    let retry = request_in("slot-2", "session-of-slot-1", "call-1");
    let allowed = svc.before_tool(retry).await.unwrap().into_inner();
    assert_eq!(allowed.verdict, Verdict::Allow as i32, "{}", allowed.reason);
    let events = trace(tmp.path());
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].event_id, stamp(&events[0])), (1, vec![0, 1]));
    assert_eq!(events[0].tool.as_deref(), Some("Edit"));
}

/// Model actions `Lock` .. `Unlock` of many calls at once; rules (a) at the end, (b), (d), (e).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_get_consecutive_ids_and_increasing_stamps_and_never_collide() {
    const TASKS: usize = 8;
    const PER_TASK: usize = 10;
    let tmp = tempfile::tempdir().unwrap();
    let svc = Arc::new(service(tmp.path(), &["slot-1", "slot-2"]));

    let mut handles = Vec::new();
    for t in 0..TASKS {
        let svc = Arc::clone(&svc);
        handles.push(tokio::spawn(async move {
            let slot = if t % 2 == 0 { "slot-1" } else { "slot-2" };
            for i in 0..PER_TASK {
                let adm = svc
                    .before_tool(request(slot, &format!("t{t}-{i}")))
                    .await
                    .expect("rule (e): no call fails because of another call")
                    .into_inner();
                assert_eq!(adm.verdict, Verdict::Allow as i32);
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let events = trace(tmp.path());
    let total = TASKS * PER_TASK;
    let ids: Vec<u64> = events.iter().map(|e| e.event_id).collect();
    assert_eq!(ids, (1..=total as u64).collect::<Vec<_>>());
    for t in 0..TASKS {
        for i in 0..PER_TASK {
            assert!(
                has_call(tmp.path(), &events, &format!("t{t}-{i}")),
                "t{t}-{i} lost"
            );
        }
    }
    // Every row keeps its own call's tool_use_id: its payload names the same call.
    for e in &events {
        let id = e
            .tool_use_id
            .as_deref()
            .expect("every row has a tool_use_id");
        assert!(has_call(tmp.path(), std::slice::from_ref(e), id), "{id}");
    }
    // Each slot's own entry counts 1, 2, 3, ... in id order.
    for (slot, index) in [("slot-1", 0usize), ("slot-2", 1)] {
        let own: Vec<u64> = events
            .iter()
            .filter(|e| e.slot == slot)
            .map(|e| stamp(e)[index])
            .collect();
        assert_eq!(own, (1..=own.len() as u64).collect::<Vec<_>>(), "{slot}");
        assert_eq!(own.len(), total / 2);
    }
}

/// Model action `BlobFails`, then a retry; rules (c) and (f). The blob directory is made
/// read-only, so the real `BlobStore::put` cannot create its temporary file: the call errors,
/// no row and no file appear, and the state is untouched. With write access back, the retry of
/// the same call gets the failed call's id and stamp.
#[tokio::test]
async fn a_failed_blob_write_leaves_no_row_and_the_retry_reuses_id_and_stamp() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &["slot-1", "slot-2"]);
    svc.before_tool(request("slot-1", "first")).await.unwrap();
    let blobs = tmp.path().join(BLOBS_DIR);
    let before = blob_files(tmp.path());

    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = svc.before_tool(request("slot-1", "second")).await;
    let again = svc.before_tool(request("slot-1", "second")).await;
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = failed.expect_err("a failed blob write must be an error, never an Allow");
    assert_eq!(err.code(), Code::Internal);
    assert!(err.message().contains("blob"), "{}", err.message());
    assert!(again.is_err());
    assert_eq!(trace(tmp.path()).len(), 1, "no row for the failed call");
    assert_eq!(
        blob_files(tmp.path()),
        before,
        "no blob or temporary file left"
    );

    let retry = svc
        .before_tool(request("slot-1", "second"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry.verdict, Verdict::Allow as i32);
    let events = trace(tmp.path());
    let ids: Vec<u64> = events.iter().map(|e| e.event_id).collect();
    assert_eq!(ids, vec![1, 2]);
    assert_eq!(stamp(&events[1]), vec![2, 0], "the failures ticked nothing");
    assert!(has_call(tmp.path(), &events[1..], "second"));
}

/// Model action `AppendFails` after `BlobOk`; rule (f). The blob is stored, then the INSERT
/// collides with a planted row: the call errors and the blob stays as an orphan that no row
/// names. The retry of the same payload reuses that one blob file.
#[tokio::test]
async fn a_failed_row_after_its_blob_leaves_a_harmless_orphan_that_the_retry_reuses() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &["slot-1"]);
    let db = rusqlite::Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    db.execute(
        "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
         VALUES (1, 'planted', 'x', x'', 'BeforeTool', 0)",
        [],
    )
    .unwrap();

    let err = svc
        .before_tool(request("slot-1", "orphan"))
        .await
        .expect_err("the INSERT collides");
    assert_eq!(err.code(), Code::Internal);
    let orphan = BlobHash::of(raw_input("orphan").as_bytes()).to_string();
    assert_eq!(blob_files(tmp.path()), vec![orphan.clone()]);
    let rows = trace(tmp.path());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].slot, "planted",
        "only the planted row is in the trace"
    );

    db.execute_batch("DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = 1;")
        .unwrap();
    let retry = svc
        .before_tool(request("slot-1", "orphan"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(retry.verdict, Verdict::Allow as i32);
    let events = trace(tmp.path());
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].event_id, stamp(&events[0])), (1, vec![1]));
    assert!(has_call(tmp.path(), &events, "orphan"));
    assert_eq!(blob_files(tmp.path()), vec![orphan], "one file, reused");
}

/// Model actions `AppendFails` on a session's first call, then `Step` refusing an identity
/// (`IdentityRefused`); rule (g). The failed call must bind no session and use no call identity:
/// the same call is then admitted from a different slot with the failed call's id. Once recorded,
/// repeating it or moving its session to another slot is denied without a row.
#[tokio::test]
async fn a_failed_append_binds_no_session_and_uses_no_call_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = service(tmp.path(), &["slot-1", "slot-2"]);
    let db = rusqlite::Connection::open(tmp.path().join(TRACE_FILE)).unwrap();
    db.execute(
        "INSERT INTO events (event_id, agent_id, session_id, vclock, kind, wall_time)
         VALUES (1, 'planted', 'x', x'', 'BeforeTool', 0)",
        [],
    )
    .unwrap();
    let err = svc
        .before_tool(request_in("slot-2", "sess-x", "c1"))
        .await
        .expect_err("the INSERT collides");
    assert_eq!(err.code(), Code::Internal);
    db.execute_batch("DROP TRIGGER events_no_delete; DELETE FROM events WHERE event_id = 1;")
        .unwrap();

    // Neither the binding to slot-2 nor the identity (sess-x, c1) was kept.
    let adm = svc
        .before_tool(request_in("slot-1", "sess-x", "c1"))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(adm.verdict, Verdict::Allow as i32, "{}", adm.reason);
    let events = trace(tmp.path());
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].event_id, stamp(&events[0])), (1, vec![1, 0]));
    assert_eq!(events[0].session_id, "sess-x");
    assert_eq!(events[0].tool_use_id.as_deref(), Some("c1"));

    for (slot, call, why) in [
        ("slot-1", "c1", "already recorded"),
        ("slot-2", "c2", "bound to slot"),
    ] {
        let denied = svc
            .before_tool(request_in(slot, "sess-x", call))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(denied.verdict, Verdict::Deny as i32);
        assert!(denied.reason.contains(why), "{}", denied.reason);
    }
    assert_eq!(trace(tmp.path()).len(), 1, "the refusals left no row");
    // And they used no id or stamp.
    svc.before_tool(request_in("slot-1", "sess-x", "c3"))
        .await
        .unwrap();
    let events = trace(tmp.path());
    assert_eq!((events[1].event_id, stamp(&events[1])), (2, vec![2, 0]));
}
