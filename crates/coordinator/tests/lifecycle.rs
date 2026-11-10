//! Stop and SessionEnd reports in the pure core: each accepted report ticks its slot's clock once
//! and records one event of its own kind, with its fields as the payload and no tool metadata. A
//! report binds a new session like any first event, is refused for an unknown slot, an empty or
//! rebound session, or missing fields, and changes no recorded call, completion or binding: a
//! late completion after a Stop or SessionEnd still pairs, the ended session stays on its slot,
//! and a new session on the same slot continues the slot's clock.

use clock::Clock;
use coordinator::core::identity::{Call, CallKey, CallState};
use coordinator::core::{
    step, Ack, AfterTool, BeforeTool, Input, Lifecycle, LifecycleKind, Output, RunConfig, State,
    ToolResult, Verdict,
};
use trace_store::{Event, EventKind, Outcome};

const STOP_FIELDS: &str = r#"{"stop_hook_active":false,"last_assistant_message":"done"}"#;
const END_FIELDS: &str = r#"{"reason":"other"}"#;

fn state() -> State {
    State::new(&RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases: false,
    })
    .unwrap()
}

fn before(slot: &str, session_id: &str, tool_use_id: &str) -> Input {
    Input::BeforeTool(BeforeTool {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: format!(r#"{{"file_path":"/work/{tool_use_id}"}}"#),
        intent: String::new(),
        tool_use_id: tool_use_id.into(),
    })
}

fn after(slot: &str, session_id: &str, tool_use_id: &str) -> Input {
    Input::AfterTool(AfterTool {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: format!(r#"{{"file_path":"/work/{tool_use_id}"}}"#),
        tool_use_id: tool_use_id.into(),
        result: ToolResult::Success {
            tool_response_json: "null".into(),
        },
    })
}

fn report(slot: &str, session_id: &str, kind: LifecycleKind, fields: &str) -> Input {
    Input::Lifecycle(Lifecycle {
        slot: slot.into(),
        session_id: session_id.into(),
        kind,
        fields_json: fields.into(),
    })
}

fn stop(slot: &str, session_id: &str) -> Input {
    report(slot, session_id, LifecycleKind::Stop, STOP_FIELDS)
}

fn end(slot: &str, session_id: &str) -> Input {
    report(slot, session_id, LifecycleKind::SessionEnd, END_FIELDS)
}

fn key(session_id: &str, tool_use_id: &str) -> CallKey {
    CallKey {
        session_id: session_id.into(),
        tool_use_id: tool_use_id.into(),
    }
}

fn stamp(e: &Event) -> Vec<u64> {
    Clock::from_bytes(&e.vclock).unwrap().entries().to_vec()
}

fn recorded(st: State, input: Input) -> (State, Event) {
    let (st, outputs) = step(st, input, 7);
    match outputs.as_slice() {
        [Output::Record(e), Output::Admit(a)] if a.verdict == Verdict::Allow => (st, e.clone()),
        [Output::Record(e), Output::Ack(Ack::Recorded)] => (st, e.clone()),
        other => panic!("expected one recorded event, got {other:?}"),
    }
}

/// A refused report: a refused Ack with its reason, no event, and no id, tick or binding used.
fn refused(st: State, input: Input, session_id: &str) -> (State, String) {
    let next_id = st.next_event_id();
    let clocks = [st.clock("slot-1").cloned(), st.clock("slot-2").cloned()];
    let bound = st.session_slot(session_id).map(str::to_owned);
    let (st, outputs) = step(st, input, 0);
    let reason = match outputs.as_slice() {
        [Output::Ack(Ack::Refused { reason })] => reason.clone(),
        other => panic!("expected a refused ack and nothing else, got {other:?}"),
    };
    assert_eq!(st.next_event_id(), next_id, "a refusal uses no id");
    assert_eq!(
        [st.clock("slot-1").cloned(), st.clock("slot-2").cloned()],
        clocks
    );
    assert_eq!(st.session_slot(session_id).map(str::to_owned), bound);
    (st, reason)
}

#[test]
fn stop_and_session_end_record_their_own_kind_with_the_fields_and_tick_once() {
    let st = state();
    let (st, s) = recorded(st, stop("slot-1", "x"));
    let (st, e) = recorded(st, end("slot-1", "x"));
    assert_eq!(
        (s.event_id, s.kind, stamp(&s)),
        (1, EventKind::Stop, vec![1, 0])
    );
    assert_eq!(
        (e.event_id, e.kind, stamp(&e)),
        (2, EventKind::SessionEnd, vec![2, 0])
    );
    for (event, fields) in [(&s, STOP_FIELDS), (&e, END_FIELDS)] {
        assert_eq!(event.payload.as_deref(), Some(fields));
        assert_eq!(event.slot, "slot-1");
        assert_eq!(event.session_id, "x");
        assert_eq!(event.wall_time, 7);
        assert_eq!(
            (
                &event.tool,
                &event.tool_use_id,
                event.before_event_id,
                event.outcome,
                &event.input_payload,
                &event.path
            ),
            (&None, &None, None, None::<Outcome>, &None, &None)
        );
    }
    // The first lifecycle event of a session binds it, as any first event does.
    assert_eq!(st.session_slot("x"), Some("slot-1"));
    assert_eq!(st.clock("slot-2").unwrap().entries(), &[0, 0]);
    // `{}` (a hook that carried none of the fields) is recorded as it is.
    let (_, empty) = recorded(st, report("slot-1", "x", LifecycleKind::Stop, "{}"));
    assert_eq!(empty.payload.as_deref(), Some("{}"));
}

#[test]
fn a_session_end_keeps_calls_completions_and_the_binding_for_late_reports() {
    let st = state();
    let (st, a) = recorded(st, before("slot-1", "x", "ta"));
    let (st, b) = recorded(st, before("slot-1", "x", "tb"));
    let (st, done_b) = recorded(st, after("slot-1", "x", "tb"));
    let (st, _) = recorded(st, stop("slot-1", "x"));
    let (st, _) = recorded(st, end("slot-1", "x"));
    assert_eq!(
        st.call(&key("x", "ta")),
        Some(&Call {
            tool: "Edit".into(),
            state: CallState::Open {
                before_event_id: a.event_id
            }
        })
    );
    // A late completion after SessionEnd pairs with its BeforeTool, not as an orphan.
    let (st, late) = recorded(st, after("slot-1", "x", "ta"));
    assert_eq!(late.before_event_id, Some(a.event_id));
    assert_eq!(
        st.call(&key("x", "ta")).unwrap().completed_event_id(),
        Some(late.event_id)
    );
    // The completed call's first completion stays; a repeat after SessionEnd pairs the same way.
    let (st, again) = recorded(st, after("slot-1", "x", "tb"));
    assert_eq!(again.before_event_id, Some(b.event_id));
    assert_eq!(
        st.call(&key("x", "tb")).unwrap().completed_event_id(),
        Some(done_b.event_id)
    );
    // A repeated BeforeTool is still refused after SessionEnd: the call is remembered.
    let (st, outputs) = step(st, before("slot-1", "x", "ta"), 0);
    assert!(matches!(outputs.as_slice(), [Output::Admit(a)] if a.verdict == Verdict::Deny));
    assert_eq!(st.next_event_id(), 8);
    assert_eq!(st.clock("slot-1").unwrap().entries(), &[7, 0]);
}

#[test]
fn a_new_session_on_the_same_slot_continues_its_clock_and_the_ended_one_cannot_move() {
    let st = state();
    let (st, _) = recorded(st, before("slot-1", "x", "t1"));
    let (st, ended) = recorded(st, end("slot-1", "x"));
    assert_eq!(stamp(&ended), vec![2, 0]);
    // A restarted session on slot-1 continues slot-1's clock.
    let (st, next) = recorded(st, stop("slot-1", "y"));
    assert_eq!((next.session_id.as_str(), stamp(&next)), ("y", vec![3, 0]));
    let (st, call) = recorded(st, before("slot-1", "y", "t1"));
    assert_eq!(
        stamp(&call),
        vec![4, 0],
        "equal ids in two sessions are two calls"
    );
    // The ended session stays bound to slot-1: any event of it from slot-2 is refused.
    let (st, why) = refused(st, end("slot-2", "x"), "x");
    assert!(why.contains("bound to slot"), "{why}");
    let (st, outputs) = step(st, before("slot-2", "x", "t9"), 0);
    assert!(matches!(outputs.as_slice(), [Output::Admit(a)] if a.verdict == Verdict::Deny));
    let (st, why) = refused(st, after("slot-2", "x", "t1"), "x");
    assert!(why.contains("bound to slot"), "{why}");
    // Slot-2's clock is untouched by all of it.
    assert_eq!(st.clock("slot-2").unwrap().entries(), &[0, 0]);
    assert_eq!(st.next_event_id(), 5);
}

#[test]
fn a_lifecycle_report_binds_its_session_and_then_tool_calls_must_follow() {
    let st = state();
    let (st, _) = recorded(st, stop("slot-2", "z"));
    assert_eq!(st.session_slot("z"), Some("slot-2"));
    let (st, outputs) = step(st, before("slot-1", "z", "t1"), 0);
    assert!(
        matches!(outputs.as_slice(), [Output::Admit(a)] if a.verdict == Verdict::Deny && a.reason.contains("bound to slot"))
    );
    let (_, call) = recorded(st, before("slot-2", "z", "t1"));
    assert_eq!(stamp(&call), vec![0, 2]);
}

#[test]
fn refusals_record_nothing_and_change_nothing() {
    let st = state();
    let (st, _) = recorded(st, before("slot-1", "x", "t1"));
    for (input, session, why) in [
        (stop("slot-9", "x"), "x", "unknown slot"),
        (end("", "x"), "x", "unknown slot"),
        (stop("slot-1", ""), "", "empty session_id"),
        (end("slot-2", "x"), "x", "bound to slot"),
        (
            report("slot-1", "x", LifecycleKind::Stop, ""),
            "x",
            "no lifecycle fields",
        ),
        (
            report("slot-2", "new", LifecycleKind::SessionEnd, ""),
            "new",
            "no lifecycle fields",
        ),
    ] {
        let (next, reason) = refused(st.clone(), input, session);
        assert!(reason.contains(why), "{reason}");
        assert_eq!(next.session_slot("new"), None);
        assert_eq!(
            next.call(&key("x", "t1")).unwrap().completed_event_id(),
            None
        );
    }
    // The corrected report is event 2 at slot-1's next stamp.
    let (_, ok) = recorded(st, end("slot-1", "x"));
    assert_eq!((ok.event_id, stamp(&ok)), (2, vec![2, 0]));
}
