# Stop and SessionEnd

A shim registered with `--hook-event Stop` or `--hook-event SessionEnd` reports that hook to the coordinator, which records one `Stop` or `SessionEnd` event on the session's slot and ticks that slot's clock. The event's payload is a JSON object with exactly the lifecycle fields the hook carried (`stop_hook_active` and `last_assistant_message` for Stop, `reason` for SessionEnd), each as its raw JSON text. A lifecycle report names no call and releases nothing. A recorded report is silent. A report that cannot be made exits 1 (nonblocking) with a reason on stderr, never 2, so it cannot block Stop.

## Sub-features

- `lifecycle-rows` records Stop and SessionEnd as rows of their slot with no tool, `tool_use_id` or outcome.
- `lifecycle-late-completion` pairs a PostToolUse that arrives after its session's SessionEnd with the call's BeforeTool.
- `lifecycle-binding-kept` refuses an ended session that reports from another slot: exit 1, no row.
- `lifecycle-new-session` lets a new session on the same slot continue that slot's clock.
- `lifecycle-nonblocking` exits 1 for a mistyped field, malformed JSON, another event under this registration, and a stopped coordinator.
- `lifecycle-silent-outside` stays silent outside a run.

## Driving it with verify.py

Preconditions: `verify.py doctor` passes. The journey starts a coordinator for empty run `r1` with slots `slot-1,slot-2`.

- **Run.** `verify.py drive lifecycle`. On `slot-1` with session `sess-verify-lifecycle`: a gate for `toolu_life_a`, a Stop, a SessionEnd, then the PostToolUse for `toolu_life_a`. The same session's Stop from `slot-2` exits `1` naming `slot-1`. Session `sess-verify-lifecycle-2` then reports a Stop on `slot-1`.
- **Rows.** `(event_id, kind, session, tool_use_id, before_event_id, vclock)`: `(1, BeforeTool, s, a, NULL, [1,0])`, `(2, Stop, s, NULL, NULL, [2,0])`, `(3, SessionEnd, s, NULL, NULL, [3,0])`, `(4, AfterTool, s, a, 1, [4,0])`, `(5, Stop, s-2, NULL, NULL, [5,0])`. The Stop payloads are `{"stop_hook_active":false,"last_assistant_message":"done"}` and the SessionEnd payload is `{"reason":"other"}`.
- **Failures.** Each exits `1`, stdout `""`, stderr starting `agent-runtime:`: `"stop_hook_active":"no"`; `"reason":1`; stdin `not json`; the SessionEnd fixture under `--hook-event Stop`; a Stop after the coordinator stops. The trace still holds 5 rows.
- **Silent.** No variables set: exit `0`, no output.

## Gotchas

- The Stop and SessionEnd fixtures were observed in the M0 probe, with paths shortened (see `crates/shim/tests/fixtures/README.md`). The mistyped-field inputs are edits of them by text.
- SessionEnd does not unbind its session or forget its calls. That is why the late completion pairs and the other-slot report is refused.
- Issue #3's five acceptance criteria are driven end to end by the Rust test `crates/shim/tests/e2e_acceptance.rs`, which also checks blob hashes with blake3. This journey does not repeat it.
