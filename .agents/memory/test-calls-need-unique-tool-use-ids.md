---
name: test-calls-need-unique-tool-use-ids
description: "Since issue #3 PR 3 the coordinator refuses a repeated (session_id, tool_use_id) and a session on a second slot; test helpers and journeys must give each call its own id and each slot its own session"
metadata:
  node_type: memory
  type: project
  originSessionId: e63f23d5-9e1d-4416-b616-d23e403b25d8
  modified: 2026-11-09T23:28:07.636Z
---

The coordinator records each `(session_id, tool_use_id)` once and binds a session to the slot of its first recorded event (`crates/coordinator/src/core/identity.rs`). Sending the same hook fixture twice is a repeated call and gets a Deny with no row. Using one `session_id` from two slots gets a Deny too.

**Why:** in issue #3 PR 3, several existing tests and the `slot-clocks` verify journey reused one fixture or one session across calls or slots. They would have started failing with Denies, not because of a product bug.

**How to apply:** in a test helper or journey that makes several calls, give each call its own `tool_use_id`, and give each slot its own session unless the test is about rebinding. A retry after a failed append should reuse the same id, because it must still be admitted. Never weaken an assertion or accept a Deny to get around this. In `verify.py`, `with_ids()` swaps the ids by text so the raw `tool_input` bytes stay as recorded. See [[shim-may-exit-before-reading-stdin]].
