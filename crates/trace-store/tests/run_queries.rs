//! Causal queries over a loaded run: the order laws as properties over arbitrary clocks, the
//! concurrent-writes list on a real trace with real blobs, and the size the spec asks for.

use std::time::Instant;

use clock::Clock;
use proptest::prelude::*;
use trace_store::{ConcurrentWrites, Event, Outcome, Run, RunError, Stamp, TraceWriter};

fn stamp(event_id: u64, slot: &str, clock: &[u64]) -> Stamp {
    Stamp {
        event_id,
        slot: slot.into(),
        session_id: format!("sess-{slot}"),
        vclock: Clock::from_entries(clock.to_vec()).to_bytes(),
        wall_time: 0,
    }
}

fn edit(event_id: u64, slot: &str, clock: &[u64], path: &str) -> Event {
    let input = format!(r#"{{"file_path":"{path}","old_string":"a","new_string":"b"}}"#);
    Event::before_tool(
        stamp(event_id, slot, clock),
        "Edit".into(),
        format!("t{event_id}"),
        input,
    )
}

fn after(event_id: u64, slot: &str, clock: &[u64], before: u64, outcome: Outcome) -> Event {
    Event::after_tool(
        stamp(event_id, slot, clock),
        "Edit".into(),
        format!("t{before}"),
        Some(before),
        outcome,
        "{}".into(),
        "null".into(),
    )
}

/// A run in memory, each write's input taken from the event itself.
fn run_of(events: Vec<Event>) -> Run {
    Run::from_events(events, |e| e.payload.clone().map(String::into_bytes)).unwrap()
}

fn clocks() -> impl Strategy<Value = Vec<Vec<u64>>> {
    prop::collection::vec(prop::collection::vec(0u64..4, 3), 1..12)
}

fn run_with(clocks: &[Vec<u64>]) -> Run {
    run_of(
        clocks
            .iter()
            .enumerate()
            .map(|(i, c)| Event::stop(stamp(i as u64 + 1, "slot-1", c), "{}".into()))
            .collect(),
    )
}

proptest! {
    #[test]
    fn happens_before_is_a_strict_partial_order(clocks in clocks()) {
        let run = run_with(&clocks);
        let ids: Vec<u64> = (1..=clocks.len() as u64).collect();
        for &a in &ids {
            prop_assert!(!run.happens_before(a, a).unwrap());
            for &b in &ids {
                let ab = run.happens_before(a, b).unwrap();
                prop_assert!(!(ab && run.happens_before(b, a).unwrap()));
                for &c in &ids {
                    if ab && run.happens_before(b, c).unwrap() {
                        prop_assert!(run.happens_before(a, c).unwrap());
                    }
                }
            }
        }
    }

    #[test]
    fn concurrent_is_symmetric_irreflexive_and_the_complement_of_order(clocks in clocks()) {
        let run = run_with(&clocks);
        let ids: Vec<u64> = (1..=clocks.len() as u64).collect();
        for &a in &ids {
            prop_assert!(!run.concurrent(a, a).unwrap());
            for &b in &ids {
                let ab = run.concurrent(a, b).unwrap();
                prop_assert_eq!(ab, run.concurrent(b, a).unwrap());
                let ordered = run.happens_before(a, b).unwrap() || run.happens_before(b, a).unwrap();
                prop_assert_eq!(ab, a != b && !ordered);
            }
        }
    }

    #[test]
    fn the_causal_past_is_exactly_the_events_that_happen_before(clocks in clocks()) {
        let run = run_with(&clocks);
        let ids: Vec<u64> = (1..=clocks.len() as u64).collect();
        for &a in &ids {
            let want: Vec<u64> = ids.iter().copied().filter(|&b| run.happens_before(b, a).unwrap()).collect();
            prop_assert_eq!(run.causal_past(a).unwrap(), want);
        }
    }
}

#[test]
fn a_loaded_trace_lists_only_cross_slot_concurrent_writes_to_one_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mut w = TraceWriter::create(tmp.path()).unwrap();
    let failure = Outcome::Failure { is_interrupt: None };
    let events = [
        edit(1, "slot-1", &[1, 0], "/work/src/lib.rs"),
        edit(2, "slot-2", &[0, 1], "/work/src/lib.rs"),
        // slot-2 has seen slot-1's first event: ordered after 1, so no pair with it.
        edit(3, "slot-2", &[1, 2], "/work/src/lib.rs"),
        edit(4, "slot-1", &[2, 0], "/work/src/other.rs"),
        // A write that failed is not a write.
        edit(5, "slot-1", &[3, 0], "/work/src/lib.rs"),
        after(6, "slot-1", &[4, 0], 5, failure),
        // The same slot's writes are always ordered.
        edit(7, "slot-1", &[5, 0], "/work/src/lib.rs"),
        after(8, "slot-1", &[6, 0], 7, Outcome::Success),
    ];
    for e in &events {
        w.append(e).unwrap();
    }
    drop(w);

    let run = Run::load(tmp.path()).unwrap();
    assert_eq!(run.events().len(), 8);
    assert_eq!(
        run.writes().map(|e| e.event.event_id).collect::<Vec<_>>(),
        [1, 2, 3, 4, 7]
    );
    let pair = |first, second| ConcurrentWrites {
        path: "/work/src/lib.rs".into(),
        first,
        second,
    };
    assert_eq!(
        run.concurrent_writes(),
        [pair(1, 2), pair(2, 7), pair(3, 7)]
    );

    assert!(run.happens_before(1, 3).unwrap());
    assert!(run.concurrent(1, 2).unwrap());
    assert!(run.knew("slot-2", 1, 3).unwrap());
    assert!(!run.knew("slot-2", 1, 2).unwrap());
    assert!(run.knew("slot-1", 1, 1).unwrap());
    assert_eq!(run.causal_past(3).unwrap(), [1, 2]);
    assert!(matches!(
        run.knew("slot-1", 1, 3),
        Err(RunError::OtherSlot { .. })
    ));
    assert!(matches!(
        run.happens_before(1, 99),
        Err(RunError::NoSuchEvent(99))
    ));
}

#[test]
fn fifty_thousand_events_are_queried_in_well_under_a_second() {
    let slots = 20usize;
    let mut clock = vec![0u64; slots];
    let events: Vec<Event> = (0..50_000u64)
        .map(|i| {
            let slot = (i as usize) % slots;
            clock[slot] += 1;
            let mut stamped = vec![0u64; slots];
            stamped[slot] = clock[slot];
            edit(
                i + 1,
                &format!("slot-{slot}"),
                &stamped,
                &format!("/work/src/f{}.rs", i / 20),
            )
        })
        .collect();
    let run = run_of(events);
    let start = Instant::now();
    let pairs = run.concurrent_writes();
    let past = run.causal_past(50_000).unwrap();
    let elapsed = start.elapsed();
    // 2,500 paths, each written once by each of the 20 slots: every pair on a path is concurrent.
    assert_eq!(pairs.len(), 2_500 * (20 * 19 / 2));
    assert_eq!(past.len(), 2_499);
    // The spec's bound is for an optimized build. An unoptimized test build runs the same
    // 525,000 clock comparisons several times slower, so it only guards against a blowup.
    let bound_ms = if cfg!(debug_assertions) {
        10_000
    } else {
        1_000
    };
    println!("50,000 events: concurrent_writes and causal_past took {elapsed:?}");
    assert!(elapsed.as_millis() < bound_ms, "queries took {elapsed:?}");
}
