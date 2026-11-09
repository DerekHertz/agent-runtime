# Admitted tool call is traced

When a Claude Code session in a run is about to use a tool, the shim asks the coordinator; an admitted call prints the allow decision and leaves exactly one stamped BeforeTool event in the run's trace, with the agent's raw tool input preserved.

## Sub-features

- `admit-allow` prints the allow decision JSON, exit 0, empty stderr.
- `admit-one-event` records exactly one BeforeTool row for slot, session and tool.
- `admit-clock` stamps the slot's clock: slot-2 of 2 gets `[0,1]`, event id 1.
- `admit-raw-input` stores `tool_input` byte-identical to the fixture's raw `tool_input`.
- `admit-shutdown` stops cleanly on SIGTERM and removes the socket.

## How to get to it (user POV)

- Claude Code runs `art-shim` for a PreToolUse Edit with `AGENT_RUNTIME_SLOT` and `AGENT_RUNTIME_SOCKET` set by the launcher.
- By hand: `AGENT_RUNTIME_SLOT=slot-2 AGENT_RUNTIME_SOCKET=<sock> art-shim < crates/shim/tests/fixtures/pre_tool_use_edit.json`.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes; no leftover harness state.
- Empty run `r1` with slots `slot-1,slot-2`, started by the journey.

- **Start.** Launch the coordinator and wait for the socket. Run `verify.py drive admitted-tool-call`. The socket accepts a connection and stderr says `art-coordinator: listening on <sock>`.
- **Call.** Send the Edit fixture as `slot-2`. Exit code `0`; stdout is exactly `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}`; stderr empty.
- **Read the trace.** Query `trace.db` read-only. Exactly one row: `kind=BeforeTool`, `agent_id=slot-2`, `session_id=90c75de7-f344-4da3-9b24-152c5bfda374`, `tool=Edit`, `event_id=1`, `vclock=[0,1]`, `tool_input` byte-identical to the fixture's raw `tool_input` text.
- **Shut down.** SIGTERM the coordinator. Exit code `0` and the socket file is gone.
- **Proof.** `summary.json` lists these checks; `admitted-tool-call/trace-after-edit.json` holds the row; `01-edit-slot-2.json` holds the command output.

## Gotchas

- `agent_id` is the slot column; do not look for a `slot` column.
- Compare `tool_input` as a string, not parsed JSON: the fixture's key order and escapes must survive.
- `path` and `payload_hash` are NULL in this slice; classification and blobs arrive later. Do not assert on them.
- `wall_time` is recorded as informational evidence only.
- Needs Unix-socket bind: a sandboxed run is BLOCKED by doctor, not failed.
