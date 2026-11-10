---
name: omitted-hook-field-is-not-false
description: "An optional hook field (PostToolUseFailure is_interrupt) has three states; keep omitted apart from false end to end: raw parse, proto3 optional, NULL column"
metadata:
  node_type: memory
  type: project
  originSessionId: a3efdfaa-b5e6-4a4c-9f92-b1ade9206a22
  modified: 2026-11-10T01:34:28.696Z
---

PostToolUseFailure's `is_interrupt` is optional in the hook schema, so "omitted" and `false` are different reports. Issue #3 PR 5 keeps the three states end to end: the shim reads the field as raw JSON with the `present` deserializer (so `null` and wrong types are seen and refused, not read as absent), the wire carries `optional bool is_interrupt` (proto3 `optional` compiles through protox and prost to `Option<bool>`), the core holds `Option<bool>`, and the trace stores 1, 0 or NULL in its own `is_interrupt` column. A string field whose empty value is legitimate (`error: ""`) is forwarded as its raw JSON text (`""`), so "empty message" and "field missing" stay distinct too.

**Why:** any `unwrap_or(false)`, a plain `bool` proto field, or a NOT NULL column silently turns "the hook did not say" into "the hook said no". A scratch mutation that read an omitted flag as `false` was caught only by a test that checks `None` explicitly.

**How to apply:** for every optional hook field, test omitted, explicit `true`/`false` (or empty), `null` and a wrong type at the shim, and check the stored NULL with plain SQL, not only through the typed reader. See [[raw-json-null-is-not-absent]] and [[proto-build-uses-protox]].
