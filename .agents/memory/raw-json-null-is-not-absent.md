---
name: raw-json-null-is-not-absent
description: "serde reads an explicit JSON null into Option<Box<RawValue>> as None; use a `present` deserialize_with when null is a valid value"
metadata:
  node_type: memory
  type: project
  originSessionId: ecf8b896-c2fd-41ca-b301-50a40fdac74f
  modified: 2026-11-10T00:11:50.134Z
---

With serde, a field typed `Option<Box<RawValue>>` (or any `Option<T>`) deserializes an explicit `null` as `None`, the same as a missing field. A PostToolUse `tool_response` of `null` is a valid response, so `crates/shim/src/hook.rs` uses `#[serde(default, deserialize_with = "present")]`, where `present` deserializes the `RawValue` and wraps it in `Some`. A missing field still gives `None` through `default`; an explicit `null` becomes the raw text `null`.

**Why:** issue #3 PR 4 had to keep a tool's raw response byte for byte, including `null`; the plain `Option` would have turned a real `null` response into "the report has no tool_response" and dropped a valid report.

**How to apply:** for any hook field where `null` is a meaningful value (responses, later error payloads), use the `present` pattern and test `null` and other scalars explicitly. Where `null` is not a valid value (a tool input), filter it out on purpose and say so. Keep raw JSON as `RawValue` so spacing and key order survive. See [[test-calls-need-unique-tool-use-ids]] for test fixtures.
