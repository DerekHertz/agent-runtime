//! Knowledge channels in the pure core (SPEC section 5, ADR 0002): a slot's clock absorbs
//! another's only when it reads a released file or is denied for a lease. Nothing else merges,
//! and with leases off nothing merges at all.

use clock::{Clock, Order};
use coordinator::core::{
    step, BeforeTool, Input, Lifecycle, LifecycleKind, Output, RunConfig, State, Verdict,
};
use trace_store::{Event, EventKind};

fn state(leases: bool) -> State {
    State::new(&RunConfig {
        slots: vec!["slot-1".into(), "slot-2".into()],
        leases,
    })
    .unwrap()
}

/// Step a tool call and return the next state and its recorded event.
fn call(st: State, slot: &str, tool: &str, path: &str) -> (State, Event) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let input = Input::BeforeTool(BeforeTool {
        slot: slot.into(),
        session_id: format!("sess-{slot}"),
        tool: tool.into(),
        tool_input_json: format!(r#"{{"file_path":"{path}"}}"#),
        intent: String::new(),
        tool_use_id: format!("toolu_{id}"),
    });
    record(step(st, input, 0))
}

fn stop(st: State, slot: &str) -> (State, Event) {
    let input = Input::Lifecycle(Lifecycle {
        slot: slot.into(),
        session_id: format!("sess-{slot}"),
        kind: LifecycleKind::Stop,
        fields_json: "{}".into(),
    });
    record(step(st, input, 0))
}

fn record((st, outputs): (State, Vec<Output>)) -> (State, Event) {
    let event = outputs.into_iter().find_map(|out| match out {
        Output::Record(e) => Some(e),
        _ => None,
    });
    (st, event.expect("the input is recorded"))
}

fn clock(e: &Event) -> Clock {
    Clock::from_bytes(&e.vclock).unwrap()
}

fn entries(e: &Event) -> Vec<u64> {
    clock(e).entries().to_vec()
}

#[test]
fn a_read_after_a_write_and_release_orders_that_work_before_the_readers_next_events() {
    let st = state(true);
    let (st, first_write) = call(st, "slot-1", "Edit", "/w/a.rs");
    let (st, second_write) = call(st, "slot-1", "Edit", "/w/a.rs");
    let (st, before_release) = call(st, "slot-2", "Bash", "ls");
    // Until slot-2 reads the file, slot-1's writes are concurrent with what slot-2 does.
    assert_eq!(
        clock(&second_write).compare(&clock(&before_release)),
        Order::Concurrent
    );
    let (st, released) = stop(st, "slot-1");

    let (st, read) = call(st, "slot-2", "Read", "/w/a.rs");
    assert_eq!(entries(&read), [3, 2]);
    let (_, write) = call(st, "slot-2", "Edit", "/w/a.rs");
    assert_eq!(write.kind, EventKind::BeforeTool);
    for earlier in [&first_write, &second_write, &released] {
        assert_eq!(clock(earlier).compare(&clock(&write)), Order::Before);
    }
}

#[test]
fn a_denied_slot_absorbs_the_holders_clock_from_when_it_took_the_lease_and_no_later() {
    let st = state(true);
    let (st, _) = call(st, "slot-1", "Bash", "ls");
    let (st, took) = call(st, "slot-1", "Edit", "/w/a.rs");
    let (st, later) = call(st, "slot-1", "Edit", "/w/a.rs");
    let (_, deny) = call(st, "slot-2", "Edit", "/w/a.rs");
    assert_eq!(deny.kind, EventKind::Deny);
    assert_eq!(entries(&took), [2, 0]);
    assert_eq!(entries(&deny), [2, 1]);
    assert_eq!(clock(&took).compare(&clock(&deny)), Order::Before);
    assert_eq!(clock(&later).compare(&clock(&deny)), Order::Concurrent);
}

#[test]
fn slots_that_never_touch_each_others_files_stay_concurrent() {
    let mut st = state(true);
    let mut last = Vec::new();
    for round in 0..5 {
        for (slot, file) in [("slot-1", "/w/one.rs"), ("slot-2", "/w/two.rs")] {
            for tool in ["Read", "Edit", "Bash"] {
                let (next, event) = call(st, slot, tool, file);
                st = next;
                last.push(event);
            }
            if round % 2 == 1 {
                let (next, event) = stop(st, slot);
                st = next;
                last.push(event);
            }
        }
    }
    for e in &last {
        let other = if e.slot == "slot-1" { 1 } else { 0 };
        assert_eq!(
            entries(e)[other],
            0,
            "event {} learned of the other slot",
            e.event_id
        );
    }
    assert_eq!(
        st.clock("slot-1")
            .unwrap()
            .compare(st.clock("slot-2").unwrap()),
        Order::Concurrent
    );
}

#[test]
fn reading_a_file_nobody_released_or_one_still_leased_teaches_nothing() {
    let st = state(true);
    let (st, _) = call(st, "slot-1", "Edit", "/w/a.rs");
    let (st, held) = call(st, "slot-2", "Read", "/w/a.rs");
    let (_, never) = call(st, "slot-2", "Read", "/w/never.rs");
    assert_eq!(entries(&held), [0, 1]);
    assert_eq!(entries(&never), [0, 2]);
}

#[test]
fn with_leases_off_nothing_merges() {
    let st = state(false);
    let (st, _) = call(st, "slot-1", "Edit", "/w/a.rs");
    let (st, _) = stop(st, "slot-1");
    let (st, read) = call(st, "slot-2", "Read", "/w/a.rs");
    let (st, write) = call(st, "slot-2", "Edit", "/w/a.rs");
    assert_eq!((entries(&read), entries(&write)), (vec![0, 1], vec![0, 2]));
    assert_eq!(write.kind, EventKind::BeforeTool);
    assert!(!st.must_reread("/w/a.rs", "slot-2"));
}

#[test]
fn a_slot_told_to_reread_takes_no_lease_and_is_let_in_after_its_read() {
    let st = state(true);
    let (st, _) = call(st, "slot-1", "Edit", "/w/a.rs");
    let (st, _) = stop(st, "slot-1");
    let (st, outputs) = step(
        st,
        Input::BeforeTool(BeforeTool {
            slot: "slot-2".into(),
            session_id: "sess-slot-2".into(),
            tool: "Write".into(),
            tool_input_json: r#"{"file_path":"/w/a.rs"}"#.into(),
            intent: String::new(),
            tool_use_id: "stale".into(),
        }),
        0,
    );
    let admission = outputs.iter().find_map(|o| match o {
        Output::Admit(a) => Some(a.clone()),
        _ => None,
    });
    let admission = admission.unwrap();
    assert_eq!(admission.verdict, Verdict::Deny);
    assert!(admission
        .reason
        .starts_with("/w/a.rs changed since you last read it"));
    assert_eq!(st.lease_holder("/w/a.rs"), None);
    // The releasing slot's own view is current: it may write again without reading.
    let (st, again) = call(st, "slot-1", "Edit", "/w/a.rs");
    assert_eq!(again.kind, EventKind::BeforeTool);
    assert_eq!(st.lease_holder("/w/a.rs"), Some("slot-1"));
}
