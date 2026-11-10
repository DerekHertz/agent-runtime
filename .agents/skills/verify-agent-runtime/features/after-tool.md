# AfterTool reports

A shim registered with `--hook-event PostToolUse` reports each successful tool call to the coordinator, which records one AfterTool event paired with the call's BeforeTool by `(session_id, tool_use_id)`. The raw tool response is the event's payload and the raw tool input it reports is a second blob. A recorded report is silent. A report that cannot be made exits 1 (nonblocking) with a reason on stderr, never 2.

## Sub-features

- `after-pairs-reversed` pairs two overlapping calls completed in reverse order with their own BeforeTool events, ticking the slot's clock for each.
- `after-raw-null` stores an explicit `null` response as the four bytes `null`.
- `after-input-kept` keeps the PostToolUse input under `input_payload_hash` even though it differs from the PreToolUse input, which stays the BeforeTool's payload.
- `after-repeat` records a repeated report as its own row naming the same BeforeTool, leaving the first row unchanged.
- `after-orphan` records a report with no BeforeTool with `before_event_id` NULL, its input and its response.
- `after-per-session` pairs an equal `tool_use_id` in another session with that session's own call.
- `after-refused` refuses a report that names another tool than its call: exit 1, no row.
- `after-nonblocking-failures` exits 1 for malformed JSON, a PreToolUse event under the PostToolUse registration, a missing `tool_response`, a half-set environment, a missing coordinator and a stopped one.
- `after-silent-outside` stays silent outside a run, and the legacy no-argument registration still ignores PostToolUse.

## How to get to it (user POV)

- A Claude Code session in a run whose settings register `art-shim --hook-event PostToolUse` for PostToolUse finishes tool calls, sometimes in parallel and out of order, or the hook fires for a call whose PreToolUse was not recorded.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Coordinator running for empty run `r1` with slots `slot-1,slot-2`.

Inputs are the Edit fixtures with only `session_id`, `tool_use_id`, `tool_response` or `tool_name` changed by text, so every other byte stays as recorded. Reports run `art-shim --hook-event PostToolUse`; gates run `art-shim` with no arguments.

- **Reverse order.** Run `verify.py drive after-tool`. As `slot-1`: PreToolUse `toolu_verify_a`, PreToolUse `toolu_verify_b` (both allowed), PostToolUse `toolu_verify_b` with `"tool_response":null`, PostToolUse `toolu_verify_a`. Each report exits `0` with empty stdout and stderr.
- **Rows.** `(event_id, kind, tool_use_id, before_event_id, outcome, vclock)` are `(1, BeforeTool, a, NULL, NULL, [1,0])`, `(2, BeforeTool, b, NULL, NULL, [2,0])`, `(3, AfterTool, b, 2, success, [3,0])`, `(4, AfterTool, a, 1, success, [4,0])`. Row 3's payload blob is `null`; row 4's is the fixture's `tool_response`. Rows 3 and 4 name the PostToolUse `tool_input` blob in `input_payload_hash`; rows 1 and 2 keep the PreToolUse `tool_input` as their payload.
- **Repeat, orphan, other session.** A second PostToolUse `toolu_verify_a` with response `42`, a PostToolUse `toolu_verify_orphan`, then as `slot-2` in session `sess-verify-other` a PreToolUse and a PostToolUse `toolu_verify_a`. Rows 5 to 8: `(5, a, before 1, [5,0])`, `(6, orphan, before NULL, [6,0])`, `(7, other session BeforeTool, [0,1])`, `(8, other session AfterTool, before 7, [0,2])`. Every hash a row names is a file in `blobs/`, and nothing else is.
- **Refusals and failures.** Each exits `1`, stdout `""`, stderr starting `agent-runtime:`: PostToolUse `toolu_verify_b` with `"tool_name":"Bash"` (reason contains `recorded as`); an orphan PostToolUse `toolu_verify_no_tool` with `"tool_name":""` (reason contains `no tool`); `art-shim --hook-event PostToolUse extra`; stdin `not json`; the PreToolUse fixture; the PostToolUse fixture without `tool_response`; only `AGENT_RUNTIME_SLOT` set; a socket path with no coordinator. `art-shim --hook-event PreToolUse extra` with the PreToolUse fixture exits `2` instead. The trace still holds 8 rows.
- **Silent.** No variables set: exit `0`, no output. The legacy no-argument `art-shim` with the PostToolUse fixture: exit `0`, no output, no row.
- **Coordinator gone.** After the coordinator stops, a report exits `1`.

## Gotchas

- A report's refusal is an exit `1`, unlike a PreToolUse refusal, which is a deny decision with exit `0`. Exit `2` on PostToolUse would only feed an error to the model; on Stop it would keep the session from stopping.
- The hook's PreToolUse and PostToolUse Edit fixtures have different `tool_input` bytes. Pairing never compares inputs; both versions are kept.
- This journey covers successful completions (PostToolUse). Failed and interrupted ones (PostToolUseFailure) are the [failed-tool](./failed-tool.md) journey. Stop and SessionEnd come in a later PR; registering one of them with `--hook-event` exits 1 with "does not report ... yet".
- The 10 s report bound is covered by `crates/shim/tests/cli.rs` (`a_hung_coordinator_ends_the_report_within_its_bound`), not by this journey, which would have to wait 10 s.
- Hook JSON comes from recorded fixtures with edited fields, not a live Claude Code session.
