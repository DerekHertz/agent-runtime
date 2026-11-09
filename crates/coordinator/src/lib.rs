//! The coordinator: the single authority that admits tool calls and records every event.
//!
//! [`core`] is a pure `step(state, input, now) -> (state', outputs)`. [`adapter`] is the adapter
//! that carries it over tonic on a Unix socket and appends its recorded events to the SQLite trace.

pub mod adapter;
pub mod core;
