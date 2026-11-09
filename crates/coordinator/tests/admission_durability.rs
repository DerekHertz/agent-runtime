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
//! (e) concurrent calls never make each other fail.

use std::path::Path;
use std::sync::Arc;

use clock::Clock;
use coordinator::adapter::CoordinatorService;
use coordinator::core::RunConfig;
use runtime_proto::v1::admission::Verdict;
use runtime_proto::v1::coordinator_server::Coordinator;
use runtime_proto::v1::BeforeToolRequest;
use tonic::{Code, Request};
use trace_store::{read_events, Event, TraceWriter, TRACE_FILE};

fn service(dir: &Path, slots: &[&str]) -> CoordinatorService {
    let config = RunConfig {
        slots: slots.iter().map(|s| s.to_string()).collect(),
    };
    CoordinatorService::new(&config, TraceWriter::create(dir).unwrap()).unwrap()
}

/// A call whose raw tool input names it, so a trace row can be matched to the call that wrote it.
fn request(slot: &str, call: &str) -> Request<BeforeToolRequest> {
    Request::new(BeforeToolRequest {
        slot: slot.into(),
        session_id: format!("session-of-{slot}"),
        tool: "Edit".into(),
        tool_input_json: format!(r#"{{"call":"{call}"}}"#),
        intent: String::new(),
    })
}

fn trace(dir: &Path) -> Vec<Event> {
    read_events(&dir.join(TRACE_FILE)).unwrap()
}

fn has_call(events: &[Event], call: &str) -> bool {
    let want = format!(r#"{{"call":"{call}"}}"#);
    events
        .iter()
        .any(|e| e.tool_input.as_deref() == Some(want.as_str()))
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
            has_call(&events, &call),
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
    assert!(!has_call(&trace(tmp.path()), "failed"));

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
    assert!(has_call(&events[1..2], "retry"), "the retry took id 2");
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
            assert!(has_call(&events, &format!("t{t}-{i}")), "t{t}-{i} lost");
        }
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
