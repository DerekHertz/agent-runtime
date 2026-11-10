//! Classification of a proposed tool call: the file it reads or writes, from its raw
//! `tool_input_json` (SPEC section 7).
//!
//! The rule itself is [`trace_store::Access`], so the coordinator and an offline reader of an old
//! trace classify the same way. Pure: it reads only its arguments. Bash is not classified yet.

pub use trace_store::Access;

/// What a call of `tool` with the raw hook `tool_input` touches.
pub fn classify(tool: &str, tool_input_json: &str) -> Access {
    Access::classify(tool, tool_input_json.as_bytes())
}
