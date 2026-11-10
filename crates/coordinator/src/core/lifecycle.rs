//! Lifecycle reports: a session's Stop or SessionEnd hook recorded as an event of its slot (SPEC
//! sections 3 and 5).
//!
//! Stop and SessionEnd share one transition and differ only in the event kind they record. A
//! report names its session, which binds to its slot like any other first event and is refused
//! when it is empty or bound to another slot. It names no call, so it changes no recorded call,
//! completion or binding: after a SessionEnd a late completion still pairs with its BeforeTool,
//! and the session still cannot move to another slot. A new session on the same slot continues
//! the slot's clock. A report moves its session to `Stopped` or `Ended` and releases every lease
//! its slot holds; the Stop or SessionEnd event is the record of that release. Every accepted
//! report ticks its slot's clock once and records one event; nothing merges.

use trace_store::{Event, Stamp};

use super::identity::SessionPhase;
use super::{Ack, Output, State};

/// Which lifecycle hook a report is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleKind {
    /// Stop: the session finished a turn.
    Stop,
    /// SessionEnd: the session ended.
    SessionEnd,
}

/// A Stop or SessionEnd report, as the shim reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifecycle {
    pub slot: String,
    pub session_id: String,
    pub kind: LifecycleKind,
    /// The lifecycle fields the hook carried, as a JSON object of their raw values (SPEC section
    /// 5). The shim builds it; an omitted field is absent, never filled in. At least `{}`, so an
    /// empty string is a report without its fields.
    pub fields_json: String,
}

fn refused(state: State, reason: impl Into<String>) -> (State, Vec<Output>) {
    let reason = reason.into();
    (state, vec![Output::Ack(Ack::Refused { reason })])
}

/// The lifecycle half of [`super::step`]: same contract, answered with an [`Ack`].
pub(super) fn lifecycle(mut state: State, report: Lifecycle, now: i64) -> (State, Vec<Output>) {
    if state.clocks.get(&report.slot).is_none() {
        let reason = format!("unknown slot {:?}: not part of this run", report.slot);
        return refused(state, reason);
    }
    if report.fields_json.is_empty() {
        return refused(state, "the report has no lifecycle fields");
    }
    let phase = match report.kind {
        LifecycleKind::Stop => SessionPhase::Stopped,
        LifecycleKind::SessionEnd => SessionPhase::Ended,
    };
    if let Err(refusal) = state
        .identities
        .record_lifecycle(&report.slot, &report.session_id, phase)
    {
        return refused(state, refusal.to_string());
    }
    // The slot's turn or session is over, so it lets go of every file it was writing.
    if let Some(leases) = &mut state.leases {
        leases.release_all(&report.slot);
    }
    // Clock rule 1 only: a lifecycle report is an event of its slot, and no knowledge channel.
    let stamp = state
        .clocks
        .tick(&report.slot)
        .expect("the slot is in the run: checked above");
    let stamp = Stamp {
        event_id: state.next_event_id,
        slot: report.slot,
        session_id: report.session_id,
        vclock: stamp.to_bytes(),
        wall_time: now,
    };
    let event = match report.kind {
        LifecycleKind::Stop => Event::stop(stamp, report.fields_json),
        LifecycleKind::SessionEnd => Event::session_end(stamp, report.fields_json),
    };
    state.next_event_id += 1;
    (
        state,
        vec![Output::Record(event), Output::Ack(Ack::Recorded)],
    )
}
