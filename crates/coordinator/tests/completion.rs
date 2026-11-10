//! AfterTool in the pure core: a completion is paired with its call's BeforeTool by
//! `(session_id, tool_use_id)` alone, keeps its own input and response, ticks its slot's clock,
//! and every refusal records nothing and changes nothing.

use clock::Clock;
use coordinator::core::identity::{Call, CallKey, CallState};
use coordinator::core::{
    step, Ack, AfterTool, BeforeTool, Input, Output, RunConfig, State, ToolResult, Verdict,
};
use trace_store::{Event, EventKind, Outcome};

fn state() -> State {
    State::new(&RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
    })
    .unwrap()
}

fn input_of(tool_use_id: &str) -> String {
    format!(r#"{{"call":"{tool_use_id}"}}"#)
}

fn before(slot: &str, session_id: &str, tool_use_id: &str) -> Input {
    Input::BeforeTool(BeforeTool {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: input_of(tool_use_id),
        intent: String::new(),
        tool_use_id: tool_use_id.into(),
    })
}

fn report(slot: &str, session_id: &str, tool_use_id: &str, response: &str) -> AfterTool {
    AfterTool {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Edit".into(),
        tool_input_json: input_of(tool_use_id),
        tool_use_id: tool_use_id.into(),
        result: ToolResult::Success {
            tool_response_json: response.into(),
        },
    }
}

fn after(slot: &str, session_id: &str, tool_use_id: &str) -> Input {
    Input::AfterTool(report(
        slot,
        session_id,
        tool_use_id,
        &format!(r#"{{"done":"{tool_use_id}"}}"#),
    ))
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

/// Step one input that must be recorded: exactly one event, then an Allow or a recorded Ack.
fn recorded(st: State, input: Input) -> (State, Event) {
    let (st, outputs) = step(st, input, 0);
    match outputs.as_slice() {
        [Output::Record(e), Output::Admit(a)] if a.verdict == Verdict::Allow => (st, e.clone()),
        [Output::Record(e), Output::Ack(Ack::Recorded)] => (st, e.clone()),
        other => panic!("expected one recorded event, got {other:?}"),
    }
}

/// A refused report: a refused Ack with its reason, no event, and nothing in the state changed.
fn refused(st: State, input: Input) -> (State, String) {
    let next_id = st.next_event_id();
    let clocks = [st.clock("slot-1").cloned(), st.clock("slot-2").cloned()];
    let (st, outputs) = step(st, input, 0);
    let reason = match outputs.as_slice() {
        [Output::Ack(Ack::Refused { reason })] => reason.clone(),
        other => panic!("expected a refused ack and nothing else, got {other:?}"),
    };
    assert_eq!(st.next_event_id(), next_id, "a refusal uses no id");
    assert_eq!(
        [st.clock("slot-1").cloned(), st.clock("slot-2").cloned()],
        clocks,
        "a refusal ticks no clock"
    );
    (st, reason)
}

#[test]
fn overlapping_calls_completed_in_reverse_order_pair_exactly() {
    let (st, ba) = recorded(state(), before("slot-1", "s", "A"));
    let (st, bb) = recorded(st, before("slot-1", "s", "B"));
    let (st, ab) = recorded(st, after("slot-1", "s", "B"));
    let (st, aa) = recorded(st, after("slot-1", "s", "A"));
    assert_eq!(
        [ba.event_id, bb.event_id, ab.event_id, aa.event_id],
        [1, 2, 3, 4]
    );
    assert_eq!(ab.before_event_id, Some(bb.event_id), "B pairs with B");
    assert_eq!(aa.before_event_id, Some(ba.event_id), "A pairs with A");
    for (e, id) in [(&ab, "B"), (&aa, "A")] {
        assert_eq!(e.kind, EventKind::AfterTool);
        assert_eq!(e.outcome, Some(Outcome::Success));
        assert_eq!(e.tool_use_id.as_deref(), Some(id));
        assert_eq!(
            e.payload.as_deref(),
            Some(&*format!(r#"{{"done":"{id}"}}"#))
        );
        assert_eq!(e.input_payload.as_deref(), Some(&*input_of(id)));
    }
    // Every accepted report ticks its slot's clock; nothing merges.
    assert_eq!(
        [stamp(&ba), stamp(&bb), stamp(&ab), stamp(&aa)],
        [vec![1, 0], vec![2, 0], vec![3, 0], vec![4, 0]]
    );
    assert_eq!(
        st.call(&key("s", "A")),
        Some(&Call {
            tool: "Edit".into(),
            state: CallState::Completed {
                before_event_id: Some(1),
                completed_event_id: 4
            }
        })
    );
    assert_eq!(
        st.call(&key("s", "B")).unwrap().completed_event_id(),
        Some(3)
    );
}

#[test]
fn equal_ids_in_two_sessions_pair_with_their_own_session() {
    let (st, b1) = recorded(state(), before("slot-1", "s1", "same"));
    let (st, b2) = recorded(st, before("slot-2", "s2", "same"));
    let (st, a2) = recorded(st, after("slot-2", "s2", "same"));
    let (_, a1) = recorded(st, after("slot-1", "s1", "same"));
    assert_eq!(a2.before_event_id, Some(b2.event_id));
    assert_eq!(a1.before_event_id, Some(b1.event_id));
    assert_eq!(stamp(&a2), vec![0, 2]);
    assert_eq!(stamp(&a1), vec![2, 0]);
}

#[test]
fn a_restarted_session_completion_continues_the_slot_clock() {
    let (st, _) = recorded(state(), before("slot-1", "old", "t1"));
    let (st, b) = recorded(st, before("slot-1", "new", "t2"));
    let (st, a) = recorded(st, after("slot-1", "new", "t2"));
    assert_eq!(
        (a.before_event_id, stamp(&a)),
        (Some(b.event_id), vec![3, 0])
    );
    assert_eq!(st.session_slot("new"), Some("slot-1"));
}

#[test]
fn a_changed_input_and_any_json_response_are_kept_exactly() {
    let (st, b) = recorded(state(), before("slot-1", "s", "t"));
    // A hook may rewrite the input between PreToolUse and PostToolUse: pair by identity anyway.
    let mut changed = report("slot-1", "s", "t", "null");
    changed.tool_input_json = r#"{"call":"t",  "rewritten":true}"#.into();
    let (st, a) = recorded(st, Input::AfterTool(changed));
    assert_eq!(a.before_event_id, Some(b.event_id));
    assert_eq!(
        a.input_payload.as_deref(),
        Some(r#"{"call":"t",  "rewritten":true}"#)
    );
    assert_eq!(b.payload.as_deref(), Some(&*input_of("t")), "both kept");
    assert_eq!(
        a.payload.as_deref(),
        Some("null"),
        "explicit null is a response"
    );
    for response in ["0", r#""text""#, "[]", "false"] {
        let (_, e) = recorded(
            st.clone(),
            Input::AfterTool(report("slot-1", "s", "t", response)),
        );
        assert_eq!(e.payload.as_deref(), Some(response));
    }
}

#[test]
fn an_orphan_completion_keeps_its_input_and_is_never_guessed() {
    // The latest call of the session is "t1"; the report names "t9", which was never seen.
    let (st, _) = recorded(state(), before("slot-1", "s", "t1"));
    let (st, orphan) = recorded(st, after("slot-1", "s", "t9"));
    assert_eq!(orphan.before_event_id, None, "no BeforeTool is guessed");
    assert_eq!(orphan.input_payload.as_deref(), Some(&*input_of("t9")));
    assert_eq!(orphan.payload.as_deref(), Some(r#"{"done":"t9"}"#));
    assert_eq!(stamp(&orphan), vec![2, 0]);
    assert_eq!(
        st.call(&key("s", "t9")),
        Some(&Call {
            tool: "Edit".into(),
            state: CallState::Completed {
                before_event_id: None,
                completed_event_id: 2
            }
        })
    );
    assert_eq!(st.call(&key("s", "t1")).unwrap().completed_event_id(), None);
    // A late BeforeTool for the orphan is a repeat of a recorded call.
    let (st, outputs) = step(st, before("slot-1", "s", "t9"), 0);
    assert!(
        matches!(outputs.as_slice(), [Output::Admit(a)] if a.verdict == Verdict::Deny),
        "{outputs:?}"
    );
    // An orphan's first event binds its session too.
    let (st, _) = recorded(st, after("slot-2", "fresh", "x"));
    assert_eq!(st.session_slot("fresh"), Some("slot-2"));
}

#[test]
fn a_repeated_completion_is_recorded_again_and_the_first_stays_authoritative() {
    let (st, b) = recorded(state(), before("slot-1", "s", "t"));
    let (st, first) = recorded(st, after("slot-1", "s", "t"));
    let (st, again) = recorded(st, Input::AfterTool(report("slot-1", "s", "t", "2")));
    assert_eq!(first.before_event_id, Some(b.event_id));
    assert_eq!(again.before_event_id, Some(b.event_id), "same BeforeTool");
    assert_ne!(first.event_id, again.event_id, "a separate event");
    assert_eq!(again.payload.as_deref(), Some("2"));
    assert_eq!(
        st.call(&key("s", "t")).unwrap().completed_event_id(),
        Some(first.event_id),
        "not overwritten"
    );
    // A repeated orphan pairs the same way: still no BeforeTool, first completion kept.
    let (st, o1) = recorded(st, after("slot-1", "s", "o"));
    let (st, o2) = recorded(st, after("slot-1", "s", "o"));
    assert_eq!((o1.before_event_id, o2.before_event_id), (None, None));
    assert_eq!(
        st.call(&key("s", "o")).unwrap().completed_event_id(),
        Some(o1.event_id)
    );
}

#[test]
fn refused_reports_record_nothing_and_change_nothing() {
    let (st, _) = recorded(state(), before("slot-1", "s", "t"));
    let mut other_tool = report("slot-1", "s", "t", "{}");
    other_tool.tool = "Bash".into();
    let no_input = AfterTool {
        tool_input_json: String::new(),
        ..report("slot-1", "s", "t", "{}")
    };
    let cases = [
        (after("slot-9", "s9", "t"), "unknown slot"),
        (after("slot-1", "", "t"), "session_id"),
        (after("slot-1", "s", ""), "tool_use_id"),
        (after("slot-2", "s", "t"), "bound to slot"),
        (Input::AfterTool(other_tool), "recorded as \"Edit\""),
        (Input::AfterTool(no_input), "no tool_input"),
        (
            Input::AfterTool(report("slot-1", "s", "t", "")),
            "no tool_response",
        ),
    ];
    let mut st = st;
    for (input, why) in cases {
        let (next, reason) = refused(st, input);
        assert!(reason.contains(why), "{why}: {reason}");
        st = next;
    }
    assert_eq!(st.call(&key("s", "t")).unwrap().completed_event_id(), None);
    assert_eq!(st.session_slot("s9"), None);
    // The refusals used nothing: the real completion is event 2 at [2, 0].
    let (_, a) = recorded(st, after("slot-1", "s", "t"));
    assert_eq!(
        (a.event_id, a.before_event_id, stamp(&a)),
        (2, Some(1), vec![2, 0])
    );
}

#[test]
fn an_orphan_with_no_tool_is_refused_and_its_corrected_retry_is_recorded() {
    // Nothing recorded yet: the report would be an orphan that binds a fresh session.
    let mut no_tool = report("slot-2", "fresh", "o", "{}");
    no_tool.tool = String::new();
    let (st, reason) = refused(state(), Input::AfterTool(no_tool.clone()));
    assert!(reason.contains("no tool"), "{reason}");
    assert_eq!(st.call(&key("fresh", "o")), None, "no identity claimed");
    assert_eq!(st.session_slot("fresh"), None, "no session bound");
    // A refused orphan for a known call leaves that call alone too.
    let (st, b) = recorded(st, before("slot-1", "s", "t"));
    no_tool.slot = "slot-1".into();
    no_tool.session_id = "s".into();
    no_tool.tool_use_id = "t".into();
    let (st, _) = refused(st, Input::AfterTool(no_tool));
    assert_eq!(st.call(&key("s", "t")).unwrap().completed_event_id(), None);
    // The corrected orphan retry gets the next unused id and stamp, and claims the call as Edit.
    let (st, orphan) = recorded(st, after("slot-2", "fresh", "o"));
    assert_eq!(
        (orphan.event_id, orphan.before_event_id, stamp(&orphan)),
        (2, None, vec![0, 1])
    );
    assert_eq!(st.session_slot("fresh"), Some("slot-2"));
    assert_eq!(st.call(&key("fresh", "o")).unwrap().tool, "Edit");
    // The corrected retry for the known call pairs with its BeforeTool.
    let (_, a) = recorded(st, after("slot-1", "s", "t"));
    assert_eq!(
        (a.event_id, a.before_event_id, stamp(&a)),
        (3, Some(b.event_id), vec![2, 0])
    );
}
