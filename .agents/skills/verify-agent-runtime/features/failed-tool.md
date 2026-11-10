# Failed and interrupted tool calls

A shim registered with `--hook-event PostToolUseFailure` reports each received PostToolUseFailure hook to the coordinator, which records one AfterTool event with outcome `failure`, paired with the call's BeforeTool by `(session_id, tool_use_id)` exactly as a success is. The event's payload is the raw JSON text of the hook's `error` string, and the raw tool input it reports is a second blob. The hook's `is_interrupt` is stored as 1 or 0, or NULL when the hook omitted it. A recorded report is silent. A report that cannot be made exits 1 (nonblocking) with a reason on stderr, never 2.

## Sub-features

- `failed-pairs-reversed` pairs two overlapping calls that fail in reverse order with their own BeforeTool events, ticking the slot's clock for each.
- `failed-interrupt-flag` stores `is_interrupt: true` as 1, `false` as 0, and an omitted flag as NULL.
- `failed-raw-error` stores the error as its raw JSON text, so an empty message is the two bytes `""`; no envelope is synthesized.
- `failed-input-kept` keeps the PostToolUseFailure input under `input_payload_hash`, for paired calls and orphans.
- `failed-mixed-history` records a success after a failure, and a failure after a success, as rows of their own naming the same BeforeTool; earlier rows are unchanged.
- `failed-orphan` records a failure with no BeforeTool with `before_event_id` NULL.
- `failed-blob-retry` refuses a failure whose error blob cannot be stored (exit 1, no row), and the corrected retry takes the same event id and stamp.
- `failed-refused` refuses a failure that names another tool than its call: exit 1, no row.
- `failed-nonblocking` exits 1 for a missing or mistyped `error`, a mistyped or `null` `is_interrupt`, malformed JSON, another event under this registration, a half-set environment, a missing or stopped coordinator, and a stray argument after the event.
- `failed-silent-outside` stays silent outside a run, and the legacy no-argument registration still ignores PostToolUseFailure.

## How to get to it (user POV)

- A Claude Code session in a run whose settings register `art-shim --hook-event PostToolUseFailure` for PostToolUseFailure receives PostToolUseFailure, for example after a Bash command exits non-zero.

The [official hook reference](https://code.claude.com/docs/en/hooks#posttoolusefailure-input) says cancelling a running tool does not fire this hook. The synthetic `is_interrupt: true` input proves handling of that metadata; it is not evidence of live cancellation delivery.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Coordinator running for empty run `r1` with slots `slot-1,slot-2`.

Inputs are the Bash PreToolUse fixture and the PostToolUseFailure fixture (recorded in the M0 probe, paths shortened), with only `session_id`, `tool_use_id`, `tool_name`, `hook_event_name`, `error`, `is_interrupt` or `tool_response` changed by text, so every other byte stays as recorded. Failures run `art-shim --hook-event PostToolUseFailure`; successes run `--hook-event PostToolUse`; gates run `art-shim` with no arguments. Every call uses session `sess-verify-failure` on `slot-1`.

- **Run.** `verify.py drive failed-tool`. Gates for `toolu_fail_a` and `toolu_fail_b`; a failure for b with `"error":"Interrupted by user","is_interrupt":true`; the fixture's failure for a (`is_interrupt` false); a repeat for a with `"error":""` and no flag; a success for a; a gate and a success for `toolu_fail_c`, then the fixture's failure for c; an orphan failure for `toolu_fail_d` with `"error":"lost"` and no flag. Every report exits `0` with empty stdout and stderr.
- **Rows.** `(event_id, kind, tool_use_id, before_event_id, outcome, is_interrupt, vclock)`: `(1, BeforeTool, a, NULL, NULL, NULL, [1,0])`, `(2, BeforeTool, b, …, [2,0])`, `(3, AfterTool, b, 2, failure, 1, [3,0])`, `(4, AfterTool, a, 1, failure, 0, [4,0])`, `(5, AfterTool, a, 1, failure, NULL, [5,0])`, `(6, AfterTool, a, 1, success, NULL, [6,0])`, `(7, BeforeTool, c, …, [7,0])`, `(8, AfterTool, c, 7, success, NULL, [8,0])`, `(9, AfterTool, c, 7, failure, 0, [9,0])`, `(10, AfterTool, d, NULL, failure, NULL, [10,0])`. The failure payload blobs are `"Interrupted by user"`, the fixture's raw `error`, `""`, the fixture's raw `error` and `"lost"`. Every AfterTool names the fixture's raw `tool_input` blob; every BeforeTool keeps the PreToolUse input. Every hash a row names is a file in `blobs/`, and nothing else is.
- **Blob write fails.** With `blobs/` made read-only, a failure for `toolu_fail_e` with `"error":"E failed"` exits `1` with a reason containing `blob`, and no row is added. With the mode restored, the same report is row 11 at `[11,0]`, an orphan failure with `is_interrupt` 0 and payload `"E failed"`.
- **Refusals and failures.** Each exits `1`, stdout `""`, stderr starting `agent-runtime:`: a failure for a with `"tool_name":"Edit"` (reason contains `recorded as`); no `error`; `"error":null`; `"error":1`; `"is_interrupt":"true"`; `"is_interrupt":null`; stdin `not json`; the PostToolUse fixture; only `AGENT_RUNTIME_SLOT` set; a socket path with no coordinator; `--hook-event PostToolUseFailure extra`; the failure fixture under `--hook-event PostToolUse`. The trace still holds 11 rows.
- **Silent.** No variables set: exit `0`, no output. The legacy no-argument `art-shim` with the failure fixture: exit `0`, no output, no row.
- **Coordinator gone.** After the coordinator stops, a failure report exits `1`.

## Gotchas

- `is_interrupt` is optional in the hook schema. The shim forwards an omitted flag as unset and the trace stores NULL; reading NULL as `false` would claim the hook said it was not an interruption.
- The `error` is kept as JSON text, quotes and escapes included, so the payload of a failure is a JSON string value like a success's payload is a JSON value. It is the hook's own value, not a synthesized envelope.
- The blob-failure step changes the mode of the run's `blobs/` directory for one shim call and restores it. A drive run as root would not see the failure, because root ignores the mode.
- Hook JSON comes from recorded fixtures with edited fields, not a live Claude Code session.
