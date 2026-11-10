//! An event's shape by kind.
//!
//! [`Event`] is the trace row: one struct with a column per field, most of them `None` for most
//! kinds. This module is the only place that knows which columns go with which kind. Writers
//! build a row through a constructor here, so a row with a foreign column cannot be built by
//! accident. Readers call [`Event::body`] and match on [`Body`], so they never test an `Option`
//! to find out what a row is.

use crate::{Event, EventKind, Outcome};

/// The fields every event has, whatever its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub event_id: u64,
    pub slot: String,
    pub session_id: String,
    /// The slot's full clock, as `clock::Clock::to_bytes`.
    pub vclock: Vec<u8>,
    /// Milliseconds since the Unix epoch. Display only.
    pub wall_time: i64,
}

/// What an event is, with the fields its kind carries. Payloads are not here: read them with
/// [`crate::read_payload`] and [`crate::read_input_payload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body<'a> {
    /// A proposed tool call. `tool_use_id` is `None` only in a trace written before the column.
    BeforeTool {
        tool: &'a str,
        tool_use_id: Option<&'a str>,
    },
    /// A completed tool call. `before_event_id` is `None` for an orphan completion.
    AfterTool {
        tool: &'a str,
        tool_use_id: Option<&'a str>,
        before_event_id: Option<u64>,
        outcome: Outcome,
    },
    Stop,
    SessionEnd,
}

/// A row whose columns do not fit its kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShapeError {
    #[error("event {event_id} of kind {kind} has no {column}")]
    Missing {
        event_id: u64,
        kind: &'static str,
        column: &'static str,
    },
    #[error("event {event_id} of kind {kind} has a {column}, which that kind never carries")]
    Foreign {
        event_id: u64,
        kind: &'static str,
        column: &'static str,
    },
    #[error("event {event_id} has kind {kind}, which nothing records yet")]
    NotRecordedYet { event_id: u64, kind: &'static str },
}

impl Event {
    fn blank(stamp: Stamp, kind: EventKind, payload: String) -> Event {
        Event {
            event_id: stamp.event_id,
            slot: stamp.slot,
            session_id: stamp.session_id,
            vclock: stamp.vclock,
            kind,
            tool: None,
            path: None,
            payload_hash: None,
            payload: Some(payload),
            input_payload_hash: None,
            input_payload: None,
            tool_use_id: None,
            before_event_id: None,
            outcome: None,
            wall_time: stamp.wall_time,
        }
    }

    /// A BeforeTool to append. Its payload is the raw hook `tool_input`.
    pub fn before_tool(
        stamp: Stamp,
        tool: String,
        tool_use_id: String,
        tool_input: String,
    ) -> Event {
        Event {
            tool: Some(tool),
            tool_use_id: Some(tool_use_id),
            ..Event::blank(stamp, EventKind::BeforeTool, tool_input)
        }
    }

    /// An AfterTool to append. `result` is the raw `tool_response` of a success or the raw
    /// `error` of a failure, and `tool_input` is the input the report carried.
    pub fn after_tool(
        stamp: Stamp,
        tool: String,
        tool_use_id: String,
        before_event_id: Option<u64>,
        outcome: Outcome,
        tool_input: String,
        result: String,
    ) -> Event {
        Event {
            tool: Some(tool),
            tool_use_id: Some(tool_use_id),
            before_event_id,
            outcome: Some(outcome),
            input_payload: Some(tool_input),
            ..Event::blank(stamp, EventKind::AfterTool, result)
        }
    }

    /// A Stop to append. Its payload is the hook's lifecycle fields as a JSON object.
    pub fn stop(stamp: Stamp, fields: String) -> Event {
        Event::blank(stamp, EventKind::Stop, fields)
    }

    /// A SessionEnd to append. Its payload is the hook's lifecycle fields as a JSON object.
    pub fn session_end(stamp: Stamp, fields: String) -> Event {
        Event::blank(stamp, EventKind::SessionEnd, fields)
    }

    /// This row as its kind, or why its columns do not fit that kind.
    pub fn body(&self) -> Result<Body<'_>, ShapeError> {
        let kind = self.kind.as_str();
        let missing = |column| ShapeError::Missing {
            event_id: self.event_id,
            kind,
            column,
        };
        let foreign = |column| ShapeError::Foreign {
            event_id: self.event_id,
            kind,
            column,
        };
        let completion_columns = [
            ("before_event_id", self.before_event_id.is_some()),
            ("outcome", self.outcome.is_some()),
            ("input_payload_hash", self.input_payload_hash.is_some()),
        ];
        let no_completion_columns = || match completion_columns.iter().find(|(_, set)| *set) {
            Some((column, _)) => Err(foreign(column)),
            None => Ok(()),
        };
        match self.kind {
            EventKind::BeforeTool => {
                no_completion_columns()?;
                Ok(Body::BeforeTool {
                    tool: self.tool.as_deref().ok_or(missing("tool"))?,
                    tool_use_id: self.tool_use_id.as_deref(),
                })
            }
            EventKind::AfterTool => Ok(Body::AfterTool {
                tool: self.tool.as_deref().ok_or(missing("tool"))?,
                tool_use_id: self.tool_use_id.as_deref(),
                before_event_id: self.before_event_id,
                outcome: self.outcome.ok_or(missing("outcome"))?,
            }),
            EventKind::Stop | EventKind::SessionEnd => {
                no_completion_columns()?;
                if self.tool.is_some() {
                    return Err(foreign("tool"));
                }
                if self.tool_use_id.is_some() {
                    return Err(foreign("tool_use_id"));
                }
                Ok(if self.kind == EventKind::Stop {
                    Body::Stop
                } else {
                    Body::SessionEnd
                })
            }
            EventKind::Acquire
            | EventKind::Release
            | EventKind::Invalidate
            | EventKind::Barrier
            | EventKind::Scatter
            | EventKind::Gather => Err(ShapeError::NotRecordedYet {
                event_id: self.event_id,
                kind,
            }),
        }
    }
}
