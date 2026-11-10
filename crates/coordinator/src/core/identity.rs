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
    /// The event id of the call's BeforeTool, or `None` when the coordinator first saw the call
    /// through its AfterTool (an orphan completion, for example after a PreToolUse timeout).
    pub before_event_id: Option<u64>,
    pub tool: String,
    /// The event id of the call's first AfterTool, success or failure, which stays the
    /// authoritative completion. A later report for the same call, with either outcome, is
    /// recorded as its own event and changes nothing here.
    pub completed_event_id: Option<u64>,
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
        self.bind(slot, &key);
        let call = Call {
            before_event_id: Some(event_id),
            tool: tool.to_owned(),
            completed_event_id: None,
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
        self.bind(slot, &key);
        let call = self.calls.entry(key).or_insert_with(|| Call {
            before_event_id: None,
            tool: tool.to_owned(),
            completed_event_id: None,
        });
        call.completed_event_id.get_or_insert(event_id);
        Ok(call.before_event_id)
    }

    /// Refuse empty ids and a session bound to another slot. Changes nothing.
    fn check_session(&self, slot: &str, key: &CallKey) -> Result<(), Refusal> {
        if key.session_id.is_empty() {
            return Err(Refusal::EmptySession);
        }
        if key.tool_use_id.is_empty() {
            return Err(Refusal::EmptyToolUseId);
        }
        match self.slot_of.get(&key.session_id) {
            Some(bound) if bound != slot => Err(Refusal::OtherSlot {
                session_id: key.session_id.clone(),
                bound: bound.clone(),
            }),
            _ => Ok(()),
        }
    }

    fn bind(&mut self, slot: &str, key: &CallKey) {
        self.slot_of
            .entry(key.session_id.clone())
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
