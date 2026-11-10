//! Session and tool-call identity (SPEC section 3, "Slots and launching").
//!
//! A session's first recorded event binds it to its slot for the rest of the run. Claude Code's
//! `tool_use_id` names one tool call within its session, so a call is known by the pair
//! `(session_id, tool_use_id)`: equal ids in two sessions are two calls. The run remembers every
//! call it recorded, so a repeated BeforeTool is refused rather than recorded twice, and an
//! AfterTool is paired with its call's BeforeTool by that pair alone, never by its input (a hook
//! may have rewritten the input in between) and never by guessing from the latest call.

use std::collections::HashMap;

/// A tool call's identity within the run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallKey {
    pub session_id: String,
    pub tool_use_id: String,
}

/// What the run remembers about a recorded call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub state: CallState,
}

/// Where a recorded call stands. A call enters as `Open` through its BeforeTool, or as
/// `Completed` with no BeforeTool through an orphan AfterTool. `Open` moves to `Completed` on the
/// call's first AfterTool, success or failure. `Completed` never changes: a later report for the
/// same call is recorded as its own event and leaves the first completion in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// The BeforeTool is recorded as `before_event_id`, and no AfterTool yet.
    Open { before_event_id: u64 },
    /// The first AfterTool is recorded as `completed_event_id`. `before_event_id` is `None` when
    /// the coordinator first saw the call through that AfterTool (an orphan completion, for
    /// example after a PreToolUse timeout).
    Completed {
        before_event_id: Option<u64>,
        completed_event_id: u64,
    },
}

impl Call {
    /// The event id of the call's BeforeTool, `None` for an orphan completion.
    pub fn before_event_id(&self) -> Option<u64> {
        match self.state {
            CallState::Open { before_event_id } => Some(before_event_id),
            CallState::Completed {
                before_event_id, ..
            } => before_event_id,
        }
    }

    /// The event id of the call's first AfterTool, `None` while the call is open.
    pub fn completed_event_id(&self) -> Option<u64> {
        match self.state {
            CallState::Open { .. } => None,
            CallState::Completed {
                completed_event_id, ..
            } => Some(completed_event_id),
        }
    }
}

/// Why a call's identity is refused. Nothing is recorded for a refused call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the hook input has an empty session_id")]
    EmptySession,
    #[error("the hook input has an empty tool_use_id")]
    EmptyToolUseId,
    #[error("session {session_id:?} is bound to slot {bound:?}; a session cannot change slots")]
    OtherSlot { session_id: String, bound: String },
    #[error("tool call {tool_use_id:?} of session {session_id:?} is already recorded")]
    Repeated {
        session_id: String,
        tool_use_id: String,
    },
    #[error(
        "tool call {tool_use_id:?} of session {session_id:?} was recorded as {recorded:?}, \
         not {reported:?}"
    )]
    OtherTool {
        session_id: String,
        tool_use_id: String,
        recorded: String,
        reported: String,
    },
}

/// Session-to-slot bindings and the calls recorded so far.
#[derive(Debug, Clone, Default)]
pub struct Identities {
    slot_of: HashMap<String, String>,
    calls: HashMap<CallKey, Call>,
}

impl Identities {
    /// Record a BeforeTool's identity for `slot`, which the caller has checked is in the run, as
    /// event `event_id`. Binds the session on its first event. All or nothing: on `Err` nothing
    /// has changed.
    pub fn record_before(
        &mut self,
        slot: &str,
        key: CallKey,
        event_id: u64,
        tool: &str,
    ) -> Result<(), Refusal> {
        self.check_session(slot, &key)?;
        if self.calls.contains_key(&key) {
            return Err(Refusal::Repeated {
                session_id: key.session_id,
                tool_use_id: key.tool_use_id,
            });
        }
        self.bind(slot, &key.session_id);
        let call = Call {
            tool: tool.to_owned(),
            state: CallState::Open {
                before_event_id: event_id,
            },
        };
        self.calls.insert(key, call);
        Ok(())
    }

    /// Record an AfterTool's identity for `slot`, which the caller has checked is in the run, as
    /// event `event_id`, and return the event id of the call's BeforeTool (`None` for an orphan).
    /// Binds the session on its first event. A known call must name the same tool. The first
    /// AfterTool of a call becomes its completion; a repeated report keeps the first one. An
    /// orphan is remembered too, so its repeats pair the same way and a late BeforeTool for it is
    /// refused as a repeat. All or nothing: on `Err` nothing has changed.
    pub fn record_after(
        &mut self,
        slot: &str,
        key: CallKey,
        event_id: u64,
        tool: &str,
    ) -> Result<Option<u64>, Refusal> {
        self.check_session(slot, &key)?;
        if let Some(call) = self.calls.get(&key) {
            if call.tool != tool {
                return Err(Refusal::OtherTool {
                    recorded: call.tool.clone(),
                    reported: tool.to_owned(),
                    session_id: key.session_id,
                    tool_use_id: key.tool_use_id,
                });
            }
        }
        self.bind(slot, &key.session_id);
        let call = self.calls.entry(key).or_insert_with(|| Call {
            tool: tool.to_owned(),
            state: CallState::Completed {
                before_event_id: None,
                completed_event_id: event_id,
            },
        });
        if let CallState::Open { before_event_id } = call.state {
            call.state = CallState::Completed {
                before_event_id: Some(before_event_id),
                completed_event_id: event_id,
            };
        }
        Ok(call.before_event_id())
    }

    /// Record a lifecycle report's (Stop or SessionEnd) session for `slot`, which the caller has
    /// checked is in the run: refuse an empty or rebound session, else bind it on its first
    /// event. A lifecycle report names no call, so the recorded calls, their completions and every
    /// binding stay as they are: a SessionEnd does not unbind its session or forget its calls, so
    /// a late completion still pairs and the session cannot move to another slot. All or nothing:
    /// on `Err` nothing has changed.
    pub fn record_lifecycle(&mut self, slot: &str, session_id: &str) -> Result<(), Refusal> {
        self.check_slot(slot, session_id)?;
        self.bind(slot, session_id);
        Ok(())
    }

    /// Refuse empty ids and a session bound to another slot. Changes nothing.
    fn check_session(&self, slot: &str, key: &CallKey) -> Result<(), Refusal> {
        if key.session_id.is_empty() {
            return Err(Refusal::EmptySession);
        }
        if key.tool_use_id.is_empty() {
            return Err(Refusal::EmptyToolUseId);
        }
        self.check_slot(slot, &key.session_id)
    }

    /// Refuse an empty session and a session bound to another slot. Changes nothing.
    fn check_slot(&self, slot: &str, session_id: &str) -> Result<(), Refusal> {
        if session_id.is_empty() {
            return Err(Refusal::EmptySession);
        }
        match self.slot_of.get(session_id) {
            Some(bound) if bound != slot => Err(Refusal::OtherSlot {
                session_id: session_id.to_owned(),
                bound: bound.clone(),
            }),
            _ => Ok(()),
        }
    }

    fn bind(&mut self, slot: &str, session_id: &str) {
        self.slot_of
            .entry(session_id.to_owned())
            .or_insert_with(|| slot.to_owned());
    }

    /// The slot a session is bound to, or `None` before its first recorded event.
    pub fn slot_of(&self, session_id: &str) -> Option<&str> {
        self.slot_of.get(session_id).map(String::as_str)
    }

    pub fn call(&self, key: &CallKey) -> Option<&Call> {
        self.calls.get(key)
    }
}
