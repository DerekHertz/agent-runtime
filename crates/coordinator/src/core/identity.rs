//! Session and tool-call identity (SPEC section 3, "Slots and launching").
//!
//! A session's first recorded event binds it to its slot for the rest of the run. Claude Code's
//! `tool_use_id` names one tool call within its session, so a call is known by the pair
//! `(session_id, tool_use_id)`: equal ids in two sessions are two calls. The run remembers every
//! call it recorded, so a repeated BeforeTool is refused rather than recorded twice.

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
    /// The event id of the call's BeforeTool.
    pub before_event_id: u64,
    pub tool: String,
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
}

/// Session-to-slot bindings and the calls recorded so far.
#[derive(Debug, Clone, Default)]
pub struct Identities {
    slot_of: HashMap<String, String>,
    calls: HashMap<CallKey, Call>,
}

impl Identities {
    /// Record a BeforeTool's identity for `slot`, which the caller has checked is in the run.
    /// Binds the session on its first call. All or nothing: on `Err` nothing has changed.
    pub fn record_before(&mut self, slot: &str, key: CallKey, call: Call) -> Result<(), Refusal> {
        if key.session_id.is_empty() {
            return Err(Refusal::EmptySession);
        }
        if key.tool_use_id.is_empty() {
            return Err(Refusal::EmptyToolUseId);
        }
        if let Some(bound) = self.slot_of.get(&key.session_id) {
            if bound != slot {
                return Err(Refusal::OtherSlot {
                    session_id: key.session_id,
                    bound: bound.clone(),
                });
            }
        }
        if self.calls.contains_key(&key) {
            return Err(Refusal::Repeated {
                session_id: key.session_id,
                tool_use_id: key.tool_use_id,
            });
        }
        self.slot_of
            .entry(key.session_id.clone())
            .or_insert_with(|| slot.to_owned());
        self.calls.insert(key, call);
        Ok(())
    }

    /// The slot a session is bound to, or `None` before its first recorded event.
    pub fn slot_of(&self, session_id: &str) -> Option<&str> {
        self.slot_of.get(session_id).map(String::as_str)
    }

    pub fn call(&self, key: &CallKey) -> Option<&Call> {
        self.calls.get(key)
    }
}
