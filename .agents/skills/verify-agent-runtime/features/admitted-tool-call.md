# Admitted tool call is traced

When a Claude Code session in a run is about to use a tool, the shim asks the coordinator; an admitted call prints the allow decision and leaves exactly one stamped BeforeTool event in the run's trace. The agent's raw tool input is kept byte for byte in the run's blob store, and the row names it by hash.

## Sub-features

- `admit-allow` prints the allow decision JSON, exit 0, empty stderr.
- `admit-one-event` records exactly one BeforeTool row for slot, session and tool.
- `admit-clock` stamps the slot's clock: slot-2 of 2 gets `[0,1]`, event id 1.
- `admit-raw-input` stores the fixture's raw `tool_input` byte for byte in `blobs/<payload_hash hex>`; the row's `tool_input` column is NULL.
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
- **Read the trace.** Query `trace.db` read-only. Exactly one row: `kind=BeforeTool`, `agent_id=slot-2`, `session_id=90c75de7-f344-4da3-9b24-152c5bfda374`, `tool=Edit`, `tool_use_id=toolu_01Uy28i9ZxHpdQkCRWhbwK3g`, `event_id=1`, `vclock=[0,1]`, `tool_input` NULL, and a 32-byte `payload_hash`.
- **Read the blob.** `runs/r1/blobs/` holds exactly one file, named by the lowercase hex of `payload_hash`. Its bytes are byte-identical to the fixture's raw `tool_input` text.
- **Shut down.** SIGTERM the coordinator. Exit code `0` and the socket file is gone.
- **Proof.** `summary.json` lists these checks; `admitted-tool-call/trace-after-edit.json` holds the row; `01-edit-slot-2.json` holds the command output.

## Gotchas

- `agent_id` is the slot column; do not look for a `slot` column.
- Compare the blob as bytes or a string, not parsed JSON: the fixture's key order and escapes must survive.
- The harness does not rehash the blob, because python's stdlib has no blake3. It checks that the row names the file and that the file holds the exact bytes. The Rust tests (`crates/trace-store/tests/blobs.rs`, official blake3 vectors) check that names are the right hashes.
- Traces written before blobs (issue #2) kept the input inline in `tool_input` with a NULL `payload_hash`. The harness reports such a row as `payload_source: inline`. No journey produces one, since the binaries only write new traces.
- `path` is NULL in this slice; classification arrives later. Do not assert on it.
- `wall_time` is recorded as informational evidence only.
- Needs Unix-socket bind: a sandboxed run is BLOCKED by doctor, not failed.
