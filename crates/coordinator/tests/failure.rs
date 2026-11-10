//! Failed and interrupted AfterTool reports (PostToolUseFailure) in the pure core: they pair like
//! successes by `(session_id, tool_use_id)` alone, keep their raw input and raw error, keep the
//! interruption flag exactly as reported (omitted is `None`, never `false`), tick their slot's
//! clock once, and leave the first completion of a call authoritative whichever outcome came
//! first. Every refusal records nothing and changes nothing.

use clock::Clock;
use coordinator::core::identity::{Call, CallKey};
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
    format!(r#"{{"command":"run {tool_use_id}"}}"#)
}

fn before(slot: &str, session_id: &str, tool_use_id: &str) -> Input {
    Input::BeforeTool(BeforeTool {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Bash".into(),
        tool_input_json: input_of(tool_use_id),
        intent: String::new(),
        tool_use_id: tool_use_id.into(),
    })
}

fn report(slot: &str, session_id: &str, tool_use_id: &str, result: ToolResult) -> AfterTool {
    AfterTool {
        slot: slot.into(),
        session_id: session_id.into(),
        tool: "Bash".into(),
        tool_input_json: input_of(tool_use_id),
        tool_use_id: tool_use_id.into(),
        result,
    }
}

fn failure(error_json: &str, is_interrupt: Option<bool>) -> ToolResult {
    ToolResult::Failure {
        error_json: error_json.into(),
        is_interrupt,
    }
}

fn success(response: &str) -> ToolResult {
    ToolResult::Success {
        tool_response_json: response.into(),
    }
}

fn failed(slot: &str, session_id: &str, tool_use_id: &str, is_interrupt: Option<bool>) -> Input {
    let error = format!(r#""Exit code 1 in {tool_use_id}""#);
    Input::AfterTool(report(
        slot,
        session_id,
        tool_use_id,
        failure(&error, is_interrupt),
    ))
}

fn succeeded(slot: &str, session_id: &str, tool_use_id: &str) -> Input {
    Input::AfterTool(report(slot, session_id, tool_use_id, success("{}")))
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
fn a_failure_pairs_by_identity_and_keeps_its_input_error_and_flag() {
    let (st, ba) = recorded(state(), before("slot-1", "s", "A"));
    let (st, bb) = recorded(st, before("slot-1", "s", "B"));
    let (st, fb) = recorded(st, failed("slot-1", "s", "B", Some(true)));
    let (st, fa) = recorded(st, failed("slot-1", "s", "A", Some(false)));
    assert_eq!((fb.before_event_id, fa.before_event_id), (Some(2), Some(1)));
    assert_eq!(
        fb.outcome,
        Some(Outcome::Failure {
            is_interrupt: Some(true)
        })
    );
    assert_eq!(
        fa.outcome,
        Some(Outcome::Failure {
            is_interrupt: Some(false)
        })
    );
    for (e, id) in [(&fb, "B"), (&fa, "A")] {
        assert_eq!(e.kind, EventKind::AfterTool);
        assert_eq!(e.tool.as_deref(), Some("Bash"));
        assert_eq!(e.tool_use_id.as_deref(), Some(id));
        let error = format!(r#""Exit code 1 in {id}""#);
        assert_eq!(e.payload.as_deref(), Some(error.as_str()), "the raw error");
        assert_eq!(e.input_payload.as_deref(), Some(input_of(id).as_str()));
        assert_eq!(
            (e.payload_hash.as_ref(), e.input_payload_hash.as_ref()),
            (None, None)
        );
    }
    // Each accepted failure ticks slot-1 once; nothing merges.
    assert_eq!(
        [stamp(&ba), stamp(&bb), stamp(&fb), stamp(&fa)],
        [vec![1, 0], vec![2, 0], vec![3, 0], vec![4, 0]]
    );
    assert_eq!(st.call(&key("s", "B")).unwrap().completed_event_id, Some(3));
    assert_eq!(st.call(&key("s", "A")).unwrap().completed_event_id, Some(4));
}

#[test]
fn an_omitted_interrupt_flag_stays_omitted_and_an_empty_message_is_kept() {
    let (st, _) = recorded(state(), before("slot-2", "s", "A"));
    let input = report("slot-2", "s", "A", failure(r#""""#, None));
    let (_, e) = recorded(st, Input::AfterTool(input));
    assert_eq!(
        e.outcome,
        Some(Outcome::Failure { is_interrupt: None }),
        "omitted is not false"
    );
    assert_ne!(
        e.outcome,
        Some(Outcome::Failure {
            is_interrupt: Some(false)
        })
    );
    assert_eq!(
        e.payload.as_deref(),
        Some(r#""""#),
        "an empty error message"
    );
    assert_eq!(stamp(&e), vec![0, 2]);
}

#[test]
fn an_orphan_failure_keeps_its_input_and_error_and_names_no_before() {
    let (st, _) = recorded(state(), before("slot-1", "s", "other"));
    let (st, orphan) = recorded(st, failed("slot-1", "s", "lost", None));
    assert_eq!(orphan.event_id, 2);
    assert_eq!(orphan.before_event_id, None, "never guessed from event 1");
    assert_eq!(
        orphan.input_payload.as_deref(),
        Some(input_of("lost").as_str())
    );
    assert_eq!(orphan.payload.as_deref(), Some(r#""Exit code 1 in lost""#));
    assert_eq!(
        st.call(&key("s", "lost")),
        Some(&Call {
            before_event_id: None,
            tool: "Bash".into(),
            completed_event_id: Some(2),
        })
    );
    // A late BeforeTool for the orphan is a repeat of a recorded call.
    let (st, outputs) = step(st, before("slot-1", "s", "lost"), 0);
    assert!(
        matches!(outputs.as_slice(), [Output::Admit(a)] if a.verdict == Verdict::Deny),
        "{outputs:?}"
    );
    assert_eq!(st.next_event_id(), 3);
}

#[test]
fn success_after_failure_and_failure_after_success_keep_the_first_completion() {
    let (st, _) = recorded(state(), before("slot-1", "s", "F"));
    let (st, _) = recorded(st, before("slot-1", "s", "S"));
    // F fails first, then a success arrives for it; S succeeds first, then a failure arrives.
    let (st, f1) = recorded(st, failed("slot-1", "s", "F", Some(false)));
    let (st, s1) = recorded(st, succeeded("slot-1", "s", "S"));
    let (st, f2) = recorded(st, succeeded("slot-1", "s", "F"));
    let (st, s2) = recorded(st, failed("slot-1", "s", "S", Some(true)));
    let ids = [f1.event_id, s1.event_id, f2.event_id, s2.event_id];
    assert_eq!(ids, [3, 4, 5, 6]);
    assert_eq!(
        [
            f1.before_event_id,
            f2.before_event_id,
            s1.before_event_id,
            s2.before_event_id
        ],
        [Some(1), Some(1), Some(2), Some(2)],
        "every report names its own BeforeTool; no BeforeTool is reused for another call"
    );
    assert_eq!(
        f2.outcome,
        Some(Outcome::Success),
        "history keeps each outcome"
    );
    assert_eq!(
        s2.outcome,
        Some(Outcome::Failure {
            is_interrupt: Some(true)
        })
    );
    assert_eq!(st.call(&key("s", "F")).unwrap().completed_event_id, Some(3));
    assert_eq!(st.call(&key("s", "S")).unwrap().completed_event_id, Some(4));
    assert_eq!(
        [stamp(&f1), stamp(&s1), stamp(&f2), stamp(&s2)],
        [vec![3, 0], vec![4, 0], vec![5, 0], vec![6, 0]]
    );
}

#[test]
fn a_repeated_failure_is_recorded_again_and_the_first_stays_authoritative() {
    let (st, _) = recorded(state(), before("slot-1", "s", "A"));
    let (st, first) = recorded(st, failed("slot-1", "s", "A", None));
    let (st, again) = recorded(st, failed("slot-1", "s", "A", Some(true)));
    assert_eq!((first.event_id, again.event_id), (2, 3));
    assert_eq!(again.before_event_id, Some(1));
    assert_eq!(st.call(&key("s", "A")).unwrap().completed_event_id, Some(2));
}

#[test]
fn refused_failures_record_nothing_and_change_nothing() {
    let (st, _) = recorded(state(), before("slot-1", "s", "A"));
    let base = report("slot-1", "s", "A", failure(r#""boom""#, Some(false)));
    let cases: [(AfterTool, &str); 7] = [
        (
            AfterTool {
                slot: "slot-9".into(),
                ..base.clone()
            },
            "unknown slot",
        ),
        (
            AfterTool {
                tool: String::new(),
                ..base.clone()
            },
            "no tool",
        ),
        (
            AfterTool {
                tool_input_json: String::new(),
                ..base.clone()
            },
            "no tool_input",
        ),
        (
            AfterTool {
                result: failure("", Some(true)),
                ..base.clone()
            },
            "no error",
        ),
        (
            AfterTool {
                tool: "Edit".into(),
                ..base.clone()
            },
            "recorded as",
        ),
        (
            AfterTool {
                slot: "slot-2".into(),
                ..base.clone()
            },
            "bound to slot",
        ),
        (
            AfterTool {
                tool_use_id: String::new(),
                ..base.clone()
            },
            "empty tool_use_id",
        ),
    ];
    let mut st = st;
    for (bad, want) in cases {
        let (next, reason) = refused(st, Input::AfterTool(bad));
        assert!(reason.contains(want), "{reason:?} should contain {want:?}");
        st = next;
    }
    assert_eq!(st.call(&key("s", "A")).unwrap().completed_event_id, None);
    assert_eq!(st.session_slot("s"), Some("slot-1"));
    // The corrected report is the call's first completion, at the id no refusal used.
    let (st, e) = recorded(st, Input::AfterTool(base));
    assert_eq!(
        (e.event_id, e.before_event_id, stamp(&e)),
        (2, Some(1), vec![2, 0])
    );
    assert_eq!(st.call(&key("s", "A")).unwrap().completed_event_id, Some(2));
}

#[test]
fn a_refused_orphan_failure_claims_no_call_and_binds_no_session() {
    let bad = report("slot-2", "fresh", "X", failure("", None));
    let (st, reason) = refused(state(), Input::AfterTool(bad));
    assert!(reason.contains("no error"), "{reason}");
    assert_eq!(st.session_slot("fresh"), None);
    assert_eq!(st.call(&key("fresh", "X")), None);
    let (_, e) = recorded(st, failed("slot-1", "fresh", "X", None));
    assert_eq!(
        (e.event_id, stamp(&e), e.before_event_id),
        (1, vec![1, 0], None)
    );
}
