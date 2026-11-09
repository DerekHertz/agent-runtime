# Call identity

A session's first recorded call binds it to its slot for the run, and Claude Code's `tool_use_id` names one call within its session. The coordinator records each `(session_id, tool_use_id)` once, refuses a repeat or a session that names another slot without recording anything, and keeps one slot clock across that slot's sessions.

## Sub-features

- `identity-row` keeps the hook `tool_use_id` in the row's `tool_use_id` column.
- `identity-repeat-denied` denies a second PreToolUse with the same session and `tool_use_id`; no row, no blob, no tick.
- `identity-session-bound` denies a call whose session is already bound to another slot; no row.
- `identity-empty-denied` denies an empty `tool_use_id` or an empty `session_id`; no row.
- `identity-restart` lets a new session on the same slot continue that slot's clock.
- `identity-per-session` treats an equal `tool_use_id` in another session as a different call.
- `identity-overlap` gives two calls of one session, neither completed, their own events.

## How to get to it (user POV)

- A Claude Code session in a run makes tool calls, sometimes in parallel; its session restarts on the same slot; or a hook delivers the same PreToolUse twice.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Coordinator running for empty run `r1` with slots `slot-1,slot-2`.

Every input is the Edit fixture with only its top-level `session_id` or `tool_use_id` value changed by text, so the raw `tool_input` bytes stay as recorded.

- **First call.** Run `verify.py drive call-identity`. The Edit fixture as `slot-1` is allowed.
- **Refusals.** Each exits `0` with a deny decision: the same fixture again as `slot-1` (reason contains `already recorded`); the fixture's session with `tool_use_id=toolu_verify_moved` as `slot-2` (`bound to slot`); `tool_use_id=""` (`empty tool_use_id`); `session_id=""` (`empty session_id`).
- **Nothing recorded.** `trace.db` holds one row, event `1`, vclock `[1,0]`, and `blobs/` holds one file.
- **Allowed calls.** As `slot-1`, session `sess-verify-restart` with `toolu_verify_r1`; as `slot-2`, session `sess-verify-other` with the fixture's `tool_use_id`; as `slot-1`, the fixture's session with `toolu_verify_o1`, then `toolu_verify_o2`.
- **Rows.** `(event_id, agent_id, session_id, tool_use_id, vclock)` are `(1, slot-1, fixture session, fixture id, [1,0])`, `(2, slot-1, sess-verify-restart, toolu_verify_r1, [2,0])`, `(3, slot-2, sess-verify-other, fixture id, [0,1])`, `(4, slot-1, fixture session, toolu_verify_o1, [3,0])`, `(5, slot-1, fixture session, toolu_verify_o2, [4,0])`. Every row's blob is the fixture's raw `tool_input`.

## Gotchas

- A refused call is a Deny decision (exit `0`), as for an unknown slot; it is not a shim failure (exit `2`). A missing `tool_use_id` field is malformed hook input and does exit `2` (see [hook input](./hook-input.md)).
- Identity lives in the coordinator's memory for one run. A run cannot be resumed (see [run reservation](./run-reservation.md)), so no journey restarts the coordinator and expects old bindings.
- No AfterTool exists yet, so "overlapping" here means two BeforeTool events with no completion; pairing them with completions is a later PR.
- Hook JSON comes from a recorded fixture with edited ids, not a live Claude Code session.
