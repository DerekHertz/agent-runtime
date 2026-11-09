//! Session and tool-call identity in the pure core: a session binds to one slot, a call is known
//! by `(session_id, tool_use_id)`, and every refusal records nothing and changes nothing.

use clock::Clock;
use coordinator::core::identity::{Call, CallKey};
use coordinator::core::{step, Admission, BeforeTool, Input, Output, RunConfig, State, Verdict};
use trace_store::Event;

fn state() -> State {
    State::new(&RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
    })
    .unwrap()
}

fn before(slot: &str, session_id: &str, tool_use_id: &str) -> Input {
    Input::BeforeTool(BeforeTool {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: format!(r#"{{"call":"{tool_use_id}"}}"#),
        intent: String::new(),
        tool_use_id: tool_use_id.into(),
    })
}

fn key(session_id: &str, tool_use_id: &str) -> CallKey {
    CallKey {
        session_id: session_id.into(),
        tool_use_id: tool_use_id.into(),
    }
}

/// Run one input; split the outputs into the recorded events and the one admission.
fn run(st: State, input: Input) -> (State, Vec<Event>, Admission) {
    let (st, outputs) = step(st, input, 0);
    let mut events = Vec::new();
    let mut admission = None;
    for out in outputs {
        match out {
            Output::Record(e) => events.push(e),
            Output::Admit(a) => {
                assert!(admission.is_none(), "exactly one admission per input");
                admission = Some(a);
            }
        }
    }
    (st, events, admission.expect("every input is answered"))
}

fn allowed(st: State, input: Input) -> (State, Event) {
    let (st, mut events, adm) = run(st, input);
    assert_eq!(adm.verdict, Verdict::Allow, "{}", adm.reason);
    assert_eq!(events.len(), 1);
    (st, events.remove(0))
}

/// A refusal: a Deny, no event, and nothing in the state changed.
fn refused(st: State, input: Input) -> (State, String) {
    let next_id = st.next_event_id();
    let clocks_before = clocks(&st);
    let (st, events, adm) = run(st, input);
    assert_eq!(adm.verdict, Verdict::Deny);
    assert!(events.is_empty(), "a refused call records nothing");
    assert_eq!(st.next_event_id(), next_id, "a refused call uses no id");
    assert_eq!(clocks(&st), clocks_before, "a refused call ticks no clock");
    (st, adm.reason)
}

fn clocks(st: &State) -> Vec<Vec<u64>> {
    ["slot-1", "slot-2"]
        .iter()
        .map(|s| st.clock(s).unwrap().entries().to_vec())
        .collect()
}

fn stamp(e: &Event) -> Vec<u64> {
    Clock::from_bytes(&e.vclock).unwrap().entries().to_vec()
}

#[test]
fn a_restarted_session_on_the_same_slot_continues_the_slot_clock() {
    let (st, e1) = allowed(state(), before("slot-1", "sess-a", "toolu_1"));
    let (st, e2) = allowed(st, before("slot-1", "sess-a", "toolu_2"));
    // The session restarts: Claude Code gives it a new session_id.
    let (st, e3) = allowed(st, before("slot-1", "sess-b", "toolu_3"));
    assert_eq!(
        [stamp(&e1), stamp(&e2), stamp(&e3)],
        [vec![1, 0], vec![2, 0], vec![3, 0]]
    );
    assert_eq!(e3.session_id, "sess-b");
    assert_eq!(st.session_slot("sess-a"), Some("slot-1"));
    assert_eq!(st.session_slot("sess-b"), Some("slot-1"));
}

#[test]
fn a_session_cannot_move_to_another_slot() {
    let (st, _) = allowed(state(), before("slot-1", "sess-a", "toolu_1"));
    let (st, reason) = refused(st, before("slot-2", "sess-a", "toolu_2"));
    assert!(
        reason.contains("sess-a") && reason.contains("slot-1"),
        "{reason}"
    );
    assert_eq!(st.session_slot("sess-a"), Some("slot-1"));
    assert_eq!(st.call(&key("sess-a", "toolu_2")), None);
    // The refusal used nothing: the slot still takes the session's next call as event 2.
    let (_, e) = allowed(st, before("slot-1", "sess-a", "toolu_2"));
    assert_eq!((e.event_id, stamp(&e)), (2, vec![2, 0]));
}

#[test]
fn a_repeated_before_tool_is_refused() {
    let (st, first) = allowed(state(), before("slot-1", "sess-a", "toolu_1"));
    let (st, reason) = refused(st, before("slot-1", "sess-a", "toolu_1"));
    assert!(
        reason.contains("toolu_1") && reason.contains("already"),
        "{reason}"
    );
    assert_eq!(
        st.call(&key("sess-a", "toolu_1")),
        Some(&Call {
            before_event_id: first.event_id,
            tool: "Edit".into()
        }),
        "the first call stays the recorded one"
    );
}

#[test]
fn equal_tool_use_ids_in_different_sessions_are_different_calls() {
    let (st, e1) = allowed(state(), before("slot-1", "sess-a", "toolu_same"));
    let (st, e2) = allowed(st, before("slot-2", "sess-b", "toolu_same"));
    // Also within one slot: a restarted session may reuse an id it never saw.
    let (st, e3) = allowed(st, before("slot-1", "sess-c", "toolu_same"));
    assert_eq!(
        [e1.event_id, e2.event_id, e3.event_id],
        [1, 2, 3],
        "three events"
    );
    for (session, id) in [("sess-a", 1), ("sess-b", 2), ("sess-c", 3)] {
        assert_eq!(
            st.call(&key(session, "toolu_same"))
                .unwrap()
                .before_event_id,
            id
        );
    }
}

#[test]
fn overlapping_calls_of_one_session_keep_their_own_before_events() {
    // Two BeforeTool calls in flight together (parallel tool use): neither has completed.
    let (st, e1) = allowed(state(), before("slot-1", "sess-a", "toolu_1"));
    let (st, e2) = allowed(st, before("slot-1", "sess-a", "toolu_2"));
    assert_ne!(e1.event_id, e2.event_id);
    assert_eq!(e1.tool_use_id.as_deref(), Some("toolu_1"));
    assert_eq!(e2.tool_use_id.as_deref(), Some("toolu_2"));
    assert_eq!(
        st.call(&key("sess-a", "toolu_1")).unwrap().before_event_id,
        e1.event_id
    );
    assert_eq!(
        st.call(&key("sess-a", "toolu_2")).unwrap().before_event_id,
        e2.event_id
    );
}

#[test]
fn an_empty_session_or_tool_use_id_is_refused_and_binds_nothing() {
    let (st, reason) = refused(state(), before("slot-1", "", "toolu_1"));
    assert!(reason.contains("session_id"), "{reason}");
    let (st, reason) = refused(st, before("slot-1", "sess-a", ""));
    assert!(reason.contains("tool_use_id"), "{reason}");
    assert_eq!(st.session_slot(""), None);
    assert_eq!(
        st.session_slot("sess-a"),
        None,
        "a refused call binds no session"
    );
    // So the session can still start on another slot.
    let (st, e) = allowed(st, before("slot-2", "sess-a", "toolu_1"));
    assert_eq!(stamp(&e), vec![0, 1]);
    assert_eq!(st.session_slot("sess-a"), Some("slot-2"));
}

#[test]
fn an_unknown_slot_binds_no_session() {
    let (st, reason) = refused(state(), before("slot-9", "sess-a", "toolu_1"));
    assert!(reason.contains("slot-9"), "{reason}");
    assert_eq!(st.session_slot("sess-a"), None);
    assert_eq!(st.call(&key("sess-a", "toolu_1")), None);
}
