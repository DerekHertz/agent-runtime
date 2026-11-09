# Hook input handling

Inside a run the shim passes events it does not gate through silently and blocks calls whose hook JSON it cannot understand, so nothing malformed is recorded or admitted.

## Sub-features

- `input-post-ignored` ignores a PostToolUse event: exit 0, no output, no row.
- `input-malformed-blocks` exits 2 for non-JSON, empty stdin and a JSON array.
- `input-missing-field-blocks` exits 2 for a PreToolUse without `session_id`, `tool_name`, `tool_input` or `tool_use_id`.
- `input-no-pollution` leaves the trace empty and the coordinator usable.

## How to get to it (user POV)

- Claude Code sends PostToolUse, or a malformed or truncated hook payload, to the shim of a session in a run.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Coordinator running for empty run `r1`.

- **PostToolUse.** Run `verify.py drive hook-input`. The PostToolUse Edit fixture as `slot-1`: exit `0`, stdout `""`, stderr `""`.
- **Malformed.** Stdin `not json`, `""`, `[]`: each exits `2`, stdout `""`, stderr starts `agent-runtime:`.
- **Missing fields.** The Edit fixture with one of `session_id`, `tool_name`, `tool_input`, `tool_use_id` removed: each exits `2`.
- **Trace.** `trace.db` has zero rows and `runs/r1/blobs/` is empty.
- **Control.** The intact Edit fixture is then allowed as event `1`.

## Gotchas

- PostToolUse, PostToolUseFailure, Stop and SessionEnd are intentionally ignored in this slice (reporting arrives later); when AfterTool is implemented the PostToolUse expectation changes to a recorded event and this journey must be updated deliberately.
- Only the PostToolUse fixture exists; the other ignored events are covered by the shim's unit tests.
