//! Completion reports: an AfterTool (PostToolUse or PostToolUseFailure) recorded and paired with
//! its call's BeforeTool (SPEC sections 3 and 5).
//!
//! A report is paired by `(session_id, tool_use_id)` alone, through [`super::identity`]. Its
//! input is kept as reported, even when it differs from the BeforeTool's (a hook may have
//! rewritten it). A success's payload is the raw tool response; a failure's is the raw JSON text
//! of the hook's `error`, and its outcome keeps the interruption flag as reported (omitted stays
//! `None`). A report for a call with no recorded BeforeTool is an orphan: it is recorded with no
//! `before_event_id`, never paired with a guess. Success and failure pair the same way, and the
//! first report of a call is its completion whatever either outcome is. Every accepted report
//! ticks its slot's clock and records one event; nothing merges.

use trace_store::{Event, EventKind, Outcome};

use super::identity::CallKey;
use super::{Ack, Output, State};

/// A tool call's completion report, as the shim reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterTool {
    pub slot: String,
    pub session_id: String,
    pub tool: String,
    /// The raw hook `tool_input` reported with the completion.
    pub tool_input_json: String,
    /// The hook `tool_use_id`, which names the call within its session.
    pub tool_use_id: String,
    pub result: ToolResult,
}

/// How the call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResult {
    /// PostToolUse: the tool returned this raw hook `tool_response`, which may be any JSON value,
    /// `null` included.
    Success { tool_response_json: String },
    /// PostToolUseFailure: the call failed or was interrupted.
    Failure {
        /// The raw JSON text of the hook's `error` string, quotes included, so an empty message
        /// is the two bytes `""`. An empty field means the sender left the error out.
        error_json: String,
        /// The hook's `is_interrupt`, `None` when the hook input omitted it.
        is_interrupt: Option<bool>,
    },
}

impl ToolResult {
    /// The event's payload, its outcome, and the name of the payload field for a refusal.
    fn into_parts(self) -> (String, Outcome, &'static str) {
        match self {
            ToolResult::Success { tool_response_json } => {
                (tool_response_json, Outcome::Success, "tool_response")
            }
            ToolResult::Failure {
                error_json,
                is_interrupt,
            } => (error_json, Outcome::Failure { is_interrupt }, "error"),
        }
    }
}

fn refused(state: State, reason: impl Into<String>) -> (State, Vec<Output>) {
    let reason = reason.into();
    (state, vec![Output::Ack(Ack::Refused { reason })])
}

/// The AfterTool half of [`super::step`]: same contract, answered with an [`Ack`].
pub(super) fn after_tool(mut state: State, report: AfterTool, now: i64) -> (State, Vec<Output>) {
    if state.clocks.get(&report.slot).is_none() {
        let reason = format!("unknown slot {:?}: not part of this run", report.slot);
        return refused(state, reason);
    }
    // Refused before the identity is touched: an orphan recorded under an empty tool would claim
    // the call, and the corrected retry would then be refused as another tool.
    if report.tool.is_empty() {
        return refused(state, "the report has no tool");
    }
    // A raw JSON value is never empty, so an empty string is a field the sender left out. An
    // empty error message is still the nonempty JSON text `""`.
    if report.tool_input_json.is_empty() {
        return refused(state, "the report has no tool_input");
    }
    let (payload, outcome, payload_field) = report.result.into_parts();
    if payload.is_empty() {
        return refused(state, format!("the report has no {payload_field}"));
    }
    let key = CallKey {
        session_id: report.session_id.clone(),
        tool_use_id: report.tool_use_id.clone(),
    };
    let event_id = state.next_event_id;
    let before_event_id =
        match state
            .identities
            .record_after(&report.slot, key, event_id, &report.tool)
        {
            Ok(before) => before,
            Err(refusal) => return refused(state, refusal.to_string()),
        };
    // Clock rule 1 only: a completion is an event of its slot, and no knowledge channel.
    let stamp = state
        .clocks
        .tick(&report.slot)
        .expect("the slot is in the run: checked above");
    let event = Event {
        event_id,
        slot: report.slot,
        session_id: report.session_id,
        vclock: stamp.to_bytes(),
        kind: EventKind::AfterTool,
        tool: Some(report.tool),
        path: None,
        payload_hash: None,
        payload: Some(payload),
        input_payload_hash: None,
        input_payload: Some(report.tool_input_json),
        tool_use_id: Some(report.tool_use_id),
        before_event_id,
        outcome: Some(outcome),
        wall_time: now,
    };
    state.next_event_id += 1;
    (
        state,
        vec![Output::Record(event), Output::Ack(Ack::Recorded)],
    )
}
