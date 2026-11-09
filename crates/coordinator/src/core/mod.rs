//! The coordinator core: a pure `step(state, input, now) -> (state', outputs)`.
//!
//! No I/O, no clock reads, no randomness, and no tonic, proto or SQLite types: wall time arrives as
//! the `now` argument and effects leave as [`Output`]s for an adapter to carry out. One module per
//! concern, so later tickets touch separate files.

pub mod classification;
pub mod clocks;
pub mod defer;
pub mod identity;
pub mod leases;
pub mod snapshots;

use std::collections::HashSet;

use trace_store::{Event, EventKind};

use clocks::Clocks;
use identity::{Call, CallKey, Identities};

/// The fixed set of slots a run is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    pub slots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("a run needs at least one slot")]
    NoSlots,
    #[error("slot names must not be empty")]
    EmptySlot,
    #[error("slot {0:?} is listed more than once")]
    DuplicateSlot(String),
}

impl RunConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.slots.is_empty() {
            return Err(ConfigError::NoSlots);
        }
        let mut seen = HashSet::new();
        for slot in &self.slots {
            if slot.is_empty() {
                return Err(ConfigError::EmptySlot);
            }
            if !seen.insert(slot.as_str()) {
                return Err(ConfigError::DuplicateSlot(slot.clone()));
            }
        }
        Ok(())
    }
}

/// Everything the core remembers between steps.
#[derive(Debug, Clone)]
pub struct State {
    clocks: Clocks,
    identities: Identities,
    next_event_id: u64,
}

impl State {
    /// A fresh state for a run: every slot's clock at zero, event ids starting at 1.
    pub fn new(config: &RunConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            clocks: Clocks::new(&config.slots),
            identities: Identities::default(),
            next_event_id: 1,
        })
    }

    pub fn next_event_id(&self) -> u64 {
        self.next_event_id
    }

    /// A slot's current clock, or `None` for a slot not in the run.
    pub fn clock(&self, slot: &str) -> Option<&clock::Clock> {
        self.clocks.get(slot)
    }

    /// The slot a session is bound to, or `None` before its first recorded event.
    pub fn session_slot(&self, session_id: &str) -> Option<&str> {
        self.identities.slot_of(session_id)
    }

    /// A recorded call, by its `(session_id, tool_use_id)`.
    pub fn call(&self, key: &CallKey) -> Option<&Call> {
        self.identities.call(key)
    }
}

/// An event reaching the coordinator, already converted from the wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    BeforeTool(BeforeTool),
}

/// A proposed tool call (PreToolUse), as the shim reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeTool {
    pub slot: String,
    pub session_id: String,
    pub tool: String,
    /// The raw hook `tool_input`.
    pub tool_input_json: String,
    pub intent: String,
    /// The hook `tool_use_id`, which names the call within its session.
    pub tool_use_id: String,
}

/// An effect the adapter must carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Append this event to the run's trace.
    Record(Event),
    /// Answer the caller with this admission.
    Admit(Admission),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    Defer,
}

/// The coordinator's verdict on a proposed tool call (GLOSSARY "Admission").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub verdict: Verdict,
    pub reason: String,
    /// The slot holding the lease, when the verdict is a Deny or Defer because of one.
    pub owner: String,
    /// Set when a Read is redirected to a released version.
    pub updated_input_json: String,
}

impl Admission {
    pub fn allow() -> Self {
        Self {
            verdict: Verdict::Allow,
            reason: String::new(),
            owner: String::new(),
            updated_input_json: String::new(),
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Deny,
            reason: reason.into(),
            owner: String::new(),
            updated_input_json: String::new(),
        }
    }
}

/// Advance the coordinator by one input at wall time `now` (milliseconds since the Unix epoch,
/// display only). Returns the next state and the effects to perform, `Record`s before the `Admit`.
///
/// A refused call (unknown slot, empty or rebound session, empty or repeated `tool_use_id`) gets a
/// Deny, no `Record`, and the state back unchanged. The adapter commits the returned state only
/// after every `Record` is appended, so a failed append binds no session and uses no call identity.
pub fn step(mut state: State, input: Input, now: i64) -> (State, Vec<Output>) {
    match input {
        Input::BeforeTool(call) => {
            if state.clocks.get(&call.slot).is_none() {
                let reason = format!("unknown slot {:?}: not part of this run", call.slot);
                return (state, vec![Output::Admit(Admission::deny(reason))]);
            }
            let key = CallKey {
                session_id: call.session_id.clone(),
                tool_use_id: call.tool_use_id.clone(),
            };
            let recorded = Call {
                before_event_id: state.next_event_id,
                tool: call.tool.clone(),
            };
            if let Err(refusal) = state.identities.record_before(&call.slot, key, recorded) {
                return (
                    state,
                    vec![Output::Admit(Admission::deny(refusal.to_string()))],
                );
            }
            // Clock rule 1 only. Nothing merges here: ADR 0002, clocks cross only through
            // knowledge channels. A new session of a slot continues that slot's clock.
            let stamp = state
                .clocks
                .tick(&call.slot)
                .expect("the slot is in the run: checked above");
            let event = Event {
                event_id: state.next_event_id,
                slot: call.slot,
                session_id: call.session_id,
                vclock: stamp.to_bytes(),
                kind: EventKind::BeforeTool,
                tool: Some(call.tool),
                path: None,
                payload_hash: None,
                tool_input: Some(call.tool_input_json),
                tool_use_id: Some(call.tool_use_id),
                wall_time: now,
            };
            state.next_event_id += 1;
            (
                state,
                vec![Output::Record(event), Output::Admit(Admission::allow())],
            )
        }
    }
}
