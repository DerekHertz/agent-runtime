//! Single-writer leases in the pure core (SPEC section 4): a slot's first admitted write to a
//! file takes its lease, another slot's write to that file is denied and recorded as a Deny
//! event, and the holder's Stop or SessionEnd releases it, after which every other slot must read
//! the file again before writing it. With leases off every call is allowed.

use clock::Clock;
use coordinator::core::identity::{CallKey, CallState, SessionPhase};
use coordinator::core::{
    step, Ack, Admission, BeforeTool, Input, Lifecycle, LifecycleKind, Output, RunConfig, State,
    Verdict,
};
use trace_store::{Body, Event, EventKind};

const FILE: &str = "/work/src/lib.rs";

fn state(leases: bool) -> State {
    State::new(&RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases,
    })
    .unwrap()
}

fn call(slot: &str, tool: &str, id: &str, path: &str, intent: &str) -> Input {
    let field = if tool == "Bash" {
        "command"
    } else {
        "file_path"
    };
    Input::BeforeTool(BeforeTool {
        slot: slot.into(),
        session_id: format!("sess-{slot}"),
        tool: tool.into(),
        tool_input_json: format!(r#"{{"{field}":"{path}"}}"#),
        intent: intent.into(),
        tool_use_id: id.into(),
    })
}

fn edit(slot: &str, id: &str) -> Input {
    call(slot, "Edit", id, FILE, "")
}

fn lifecycle(slot: &str, kind: LifecycleKind) -> Input {
    Input::Lifecycle(Lifecycle {
        slot: slot.into(),
        session_id: format!("sess-{slot}"),
        kind,
        fields_json: "{}".into(),
    })
}

/// Step once: the next state, the recorded event if any, and the admission if the input was a
/// tool call.
fn run(st: State, input: Input, now: i64) -> (State, Option<Event>, Option<Admission>) {
    let (st, outputs) = step(st, input, now);
    let (mut event, mut admission) = (None, None);
    for out in outputs {
        match out {
            Output::Record(e) => event = Some(e),
            Output::Admit(a) => admission = Some(a),
            Output::Ack(ack) => assert_eq!(ack, Ack::Recorded),
        }
    }
    (st, event, admission)
}

fn stamp(e: &Event) -> Vec<u64> {
    Clock::from_bytes(&e.vclock).unwrap().entries().to_vec()
}

#[test]
fn the_second_slot_to_write_a_file_is_denied_and_the_deny_is_an_event() {
    let st = state(true);
    let (st, first, admitted) = run(st, call("slot-1", "Edit", "a", FILE, "fix mean"), 10_000);
    assert_eq!(admitted.unwrap().verdict, Verdict::Allow);
    assert_eq!(first.unwrap().kind, EventKind::BeforeTool);
    assert_eq!(st.lease_holder(FILE), Some("slot-1"));

    let (st, deny, admission) = run(st, edit("slot-2", "b"), 135_500);
    let (deny, admission) = (deny.unwrap(), admission.unwrap());
    assert_eq!(admission.verdict, Verdict::Deny);
    assert_eq!(admission.owner, "slot-1");
    assert!(
        admission.reason.starts_with(
            "/work/src/lib.rs is leased by slot-1 (intent: \"fix mean\"), held for 2m 05s."
        ),
        "{}",
        admission.reason
    );
    assert_eq!(
        deny.body().unwrap(),
        Body::Deny {
            tool: "Edit",
            tool_use_id: "b",
            path: FILE
        }
    );
    // The reason the agent was given is the event's payload; the raw input is kept beside it.
    assert_eq!(
        deny.payload.as_deref(),
        Some(
            serde_json::Value::String(admission.reason.clone())
                .to_string()
                .as_str()
        )
    );
    assert_eq!(
        deny.input_payload.as_deref(),
        Some(r#"{"file_path":"/work/src/lib.rs"}"#)
    );
    // A Deny is an event of its slot. It ticks that slot's clock, and the slot absorbs the
    // holder's clock from when it took the lease (the Deny knowledge channel).
    assert_eq!((deny.event_id, stamp(&deny)), (2, vec![1, 1]));
    assert_eq!(st.lease_holder(FILE), Some("slot-1"));
    let key = CallKey {
        session_id: "sess-slot-2".into(),
        tool_use_id: "b".into(),
    };
    assert_eq!(
        st.call(&key).unwrap().state,
        CallState::Denied { deny_event_id: 2 }
    );

    // The denied call cannot be replayed, and that refusal records nothing.
    let (st, event, admission) = run(st, edit("slot-2", "b"), 136_000);
    assert_eq!(admission.unwrap().verdict, Verdict::Deny);
    assert!(event.is_none());
    assert_eq!(st.next_event_id(), 3);
}

#[test]
fn the_holder_keeps_writing_and_other_files_reads_and_bash_are_never_denied() {
    let st = state(true);
    let (st, ..) = run(st, edit("slot-1", "a"), 0);
    for (i, input) in [
        edit("slot-1", "a2"),
        call("slot-2", "Write", "b1", "/work/src/other.rs", ""),
        call("slot-2", "Read", "b2", FILE, ""),
        call("slot-2", "Bash", "b3", "echo hi > /work/src/lib.rs", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, event, admission) = run(st.clone(), input, 1);
        assert_eq!(admission.unwrap().verdict, Verdict::Allow, "case {i}");
        assert_eq!(event.unwrap().kind, EventKind::BeforeTool, "case {i}");
    }
}

#[test]
fn stop_and_session_end_each_release_every_lease_of_their_slot() {
    for kind in [LifecycleKind::Stop, LifecycleKind::SessionEnd] {
        let st = state(true);
        let (st, ..) = run(st, edit("slot-1", "a"), 0);
        let (st, ..) = run(st, call("slot-1", "Write", "a2", "/work/src/new.rs", ""), 0);
        let (st, _, denied) = run(st, edit("slot-2", "b"), 0);
        assert_eq!(denied.unwrap().verdict, Verdict::Deny);

        let (st, released, _) = run(st, lifecycle("slot-1", kind), 0);
        assert!(matches!(
            released.unwrap().kind,
            EventKind::Stop | EventKind::SessionEnd
        ));
        assert_eq!(st.lease_holder(FILE), None);
        assert_eq!(st.lease_holder("/work/src/new.rs"), None);

        // The file changed under slot-2, so it must read it again before it may write it.
        let (st, event, stale) = run(st, edit("slot-2", "b2"), 0);
        let stale = stale.unwrap();
        assert_eq!((stale.verdict, stale.owner.as_str()), (Verdict::Deny, ""));
        assert!(stale.reason.contains("Re-read first"), "{}", stale.reason);
        assert_eq!(event.unwrap().kind, EventKind::Deny);
        assert_eq!(st.lease_holder(FILE), None);
        assert!(st.must_reread(FILE, "slot-2"));
        let (st, ..) = run(st, call("slot-2", "Read", "b3", FILE, ""), 0);
        assert!(!st.must_reread(FILE, "slot-2"));
        let (st, _, admitted) = run(st, edit("slot-2", "b4"), 0);
        assert_eq!(admitted.unwrap().verdict, Verdict::Allow);
        assert_eq!(st.lease_holder(FILE), Some("slot-2"));
        // The released slot is now the one that is denied.
        let (_, event, denied) = run(st, edit("slot-1", "a3"), 0);
        assert_eq!(denied.unwrap().owner, "slot-2");
        assert_eq!(event.unwrap().kind, EventKind::Deny);
    }
}

#[test]
fn a_slot_that_stops_without_a_lease_releases_nobody_elses() {
    let st = state(true);
    let (st, ..) = run(st, edit("slot-1", "a"), 0);
    let (st, ..) = run(st, lifecycle("slot-2", LifecycleKind::Stop), 0);
    assert_eq!(st.lease_holder(FILE), Some("slot-1"));
}

#[test]
fn with_leases_off_both_slots_write_the_same_file() {
    let st = state(false);
    let (st, ..) = run(st, edit("slot-1", "a"), 0);
    let (st, event, admission) = run(st, edit("slot-2", "b"), 0);
    assert_eq!(admission.unwrap().verdict, Verdict::Allow);
    assert_eq!(event.unwrap().kind, EventKind::BeforeTool);
    assert_eq!(st.lease_holder(FILE), None);
}

#[test]
fn a_session_is_active_then_stopped_then_active_again_then_ended() {
    let phase = |st: &State| st.session("sess-slot-1").map(|s| s.phase);
    let st = state(true);
    assert_eq!(phase(&st), None);
    let (st, ..) = run(st, edit("slot-1", "a"), 0);
    assert_eq!(phase(&st), Some(SessionPhase::Active));
    let (st, ..) = run(st, lifecycle("slot-1", LifecycleKind::Stop), 0);
    assert_eq!(phase(&st), Some(SessionPhase::Stopped));
    let (st, ..) = run(st, edit("slot-1", "a2"), 0);
    assert_eq!(phase(&st), Some(SessionPhase::Active));
    let (st, ..) = run(st, lifecycle("slot-1", LifecycleKind::SessionEnd), 0);
    assert_eq!(phase(&st), Some(SessionPhase::Ended));
    assert_eq!(st.session("sess-slot-1").unwrap().slot, "slot-1");
}
