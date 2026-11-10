//! The core is pure: these tests drive `step` with no tonic, no SQLite and no wall clock.

use std::sync::atomic::{AtomicU64, Ordering};

use clock::{Clock, Order};
use coordinator::core::{
    step, Admission, BeforeTool, ConfigError, Input, Output, RunConfig, State, Verdict,
};
use trace_store::{Event, EventKind};

fn state(slots: &[&str]) -> State {
    State::new(&RunConfig {
        slots: slots.iter().map(|s| s.to_string()).collect(),
    })
    .unwrap()
}

/// A call from `slot`'s session with its own `tool_use_id`, as Claude Code gives each call one.
fn before(slot: &str, tool: &str) -> Input {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = format!("toolu_{}", NEXT.fetch_add(1, Ordering::Relaxed));
    before_with_id(slot, tool, &id)
}

fn before_with_id(slot: &str, tool: &str, tool_use_id: &str) -> Input {
    Input::BeforeTool(BeforeTool {
        slot: slot.into(),
        session_id: format!("session-of-{slot}"),
        tool: tool.into(),
        tool_input_json: r#"{"file_path": "/work/src/lib.rs"}"#.into(),
        intent: String::new(),
        tool_use_id: tool_use_id.into(),
    })
}

/// Run one input and split the outputs into the recorded event and the admission.
fn run(st: State, input: Input, now: i64) -> (State, Vec<Event>, Admission) {
    let (st, outputs) = step(st, input, now);
    let mut events = Vec::new();
    let mut admission = None;
    for out in outputs {
        match out {
            Output::Record(e) => events.push(e),
            Output::Admit(a) => {
                assert!(admission.is_none(), "exactly one admission per input");
                admission = Some(a);
            }
            Output::Ack(ack) => panic!("a BeforeTool is answered with an admission: {ack:?}"),
        }
    }
    (st, events, admission.expect("every input is answered"))
}

fn stamp(e: &Event) -> Clock {
    Clock::from_bytes(&e.vclock).unwrap()
}

#[test]
fn first_event_of_a_slot_has_its_own_entry_at_one() {
    let st = state(&["slot-1", "slot-2"]);
    let (_, events, adm) = run(st, before("slot-1", "Edit"), 10);
    assert_eq!(adm.verdict, Verdict::Allow);
    assert_eq!(events.len(), 1);
    assert_eq!(stamp(&events[0]).entries(), &[1, 0]);

    let st = state(&["slot-1", "slot-2"]);
    let (_, events, _) = run(st, before("slot-2", "Edit"), 10);
    assert_eq!(stamp(&events[0]).entries(), &[0, 1]);
}

#[test]
fn event_carries_slot_session_tool_kind_and_now() {
    let st = state(&["slot-1"]);
    let (_, events, _) = run(st, before_with_id("slot-1", "Write", "toolu_w"), 1_234_567);
    let e = &events[0];
    assert_eq!(e.kind, EventKind::BeforeTool);
    assert_eq!(e.slot, "slot-1");
    assert_eq!(e.session_id, "session-of-slot-1");
    assert_eq!(e.tool.as_deref(), Some("Write"));
    assert_eq!(events[0].path.as_deref(), Some("/work/src/lib.rs"));
    assert_eq!(e.payload_hash, None);
    // The raw tool input is recorded byte for byte (SPEC section 7: re-classify offline).
    assert_eq!(
        e.payload.as_deref(),
        Some(r#"{"file_path": "/work/src/lib.rs"}"#)
    );
    assert_eq!(e.wall_time, 1_234_567);
    assert_eq!(e.tool_use_id.as_deref(), Some("toolu_w"));
}

#[test]
fn successive_events_of_a_slot_increase() {
    let mut st = state(&["slot-1", "slot-2"]);
    let mut prev: Option<Clock> = None;
    for n in 1..=4u64 {
        let (next, events, _) = run(st, before("slot-1", "Read"), 0);
        st = next;
        let c = stamp(&events[0]);
        assert_eq!(c.entries(), &[n, 0]);
        if let Some(p) = prev {
            assert_eq!(p.compare(&c), Order::Before);
        }
        prev = Some(c);
    }
}

#[test]
fn slots_tick_independently_and_their_stamps_are_concurrent() {
    let st = state(&["slot-1", "slot-2"]);
    let (st, e1, _) = run(st, before("slot-1", "Edit"), 0);
    let (st, e2, _) = run(st, before("slot-2", "Edit"), 0);
    let (c1, c2) = (stamp(&e1[0]), stamp(&e2[0]));
    assert_eq!(c1.entries(), &[1, 0]);
    assert_eq!(c2.entries(), &[0, 1]);
    assert_eq!(c1.compare(&c2), Order::Concurrent);

    // Still no merge after more traffic: slot-1 never learns of slot-2's events.
    let (_, e3, _) = run(st, before("slot-1", "Edit"), 0);
    assert_eq!(stamp(&e3[0]).entries(), &[2, 0]);
}

#[test]
fn event_ids_are_monotonic_across_slots() {
    let mut st = state(&["slot-1", "slot-2"]);
    let mut ids = Vec::new();
    for slot in ["slot-1", "slot-2", "slot-2", "slot-1"] {
        let (next, events, _) = run(st, before(slot, "Bash"), 0);
        st = next;
        ids.push(events[0].event_id);
    }
    assert_eq!(ids, vec![1, 2, 3, 4]);
    assert_eq!(st.next_event_id(), 5);
}

#[test]
fn unknown_slot_is_denied_and_state_is_unchanged() {
    let st = state(&["slot-1", "slot-2"]);
    let (st, _, _) = run(st, before("slot-1", "Edit"), 0);
    let (st, events, adm) = run(st, before("slot-9", "Edit"), 0);

    assert!(events.is_empty());
    assert_eq!(adm.verdict, Verdict::Deny);
    assert!(adm.reason.contains("slot-9"), "reason: {}", adm.reason);
    assert_eq!(st.next_event_id(), 2);
    assert_eq!(st.clock("slot-1").unwrap().entries(), &[1, 0]);
    assert_eq!(st.clock("slot-2").unwrap().entries(), &[0, 0]);
    assert!(st.clock("slot-9").is_none());
}

#[test]
fn step_is_deterministic() {
    let a = run(
        state(&["slot-1", "slot-2"]),
        before_with_id("slot-2", "Edit", "toolu_d"),
        42,
    );
    let b = run(
        state(&["slot-1", "slot-2"]),
        before_with_id("slot-2", "Edit", "toolu_d"),
        42,
    );
    assert_eq!(a.1, b.1);
    assert_eq!(a.2, b.2);
}

#[test]
fn run_config_rejects_bad_slot_lists() {
    let cfg = |slots: &[&str]| RunConfig {
        slots: slots.iter().map(|s| s.to_string()).collect(),
    };
    assert_eq!(State::new(&cfg(&[])).unwrap_err(), ConfigError::NoSlots);
    assert_eq!(
        State::new(&cfg(&["a", ""])).unwrap_err(),
        ConfigError::EmptySlot
    );
    assert_eq!(
        State::new(&cfg(&["a", "b", "a"])).unwrap_err(),
        ConfigError::DuplicateSlot("a".into())
    );
}

#[test]
fn a_before_tool_records_the_file_its_tool_names_and_none_for_bash() {
    let st = state(&["slot-1"]);
    let mut paths = Vec::new();
    let mut st = st;
    for tool in ["Edit", "Write", "Read", "Bash"] {
        let (next, events, admission) = run(st, before("slot-1", tool), 0);
        assert_eq!(admission.verdict, Verdict::Allow);
        paths.push(events[0].path.clone());
        st = next;
    }
    let file = Some("/work/src/lib.rs".to_string());
    assert_eq!(paths, [file.clone(), file.clone(), file, None]);
}
