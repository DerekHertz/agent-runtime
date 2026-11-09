//! Classification of a proposed tool call (placeholder).
//!
//! Will turn the raw `tool_input_json` of a `BeforeTool` input into the paths it touches and, for
//! Bash, one of the three classes of SPEC section 7 (read-only, writes to known paths, unknown),
//! driven by run config. Pure: it reads only its arguments. Nothing in the first slice
//! classifies; the trace keeps the raw input so old runs can be re-classified offline.
