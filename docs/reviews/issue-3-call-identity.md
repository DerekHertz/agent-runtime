# Issue #3 PR 3 walkthrough: session and tool-call identity

**Status: draft. HUMAN FEEDBACK PENDING.** Derek has not reviewed this PR. Nothing below is his feedback. When he comments, record it in `docs/reviews/pr-<number>.md` once the PR number exists, and keep this draft as the walkthrough presented.

- **Branch:** `codex/issue-3-call-identity`, PR 3 of the [issue #3 plan](../plans/issue-3.md).
- **Parent and base:** `f2d21d8aaff4f37ea386725bc8fb4c01783545e6` on `codex/issue-3-payload-blobs` (PR 2, [its walkthrough](issue-3-payload-blobs.md)), which sits on PR #15 ([its review](pr-15.md)). This PR changes PR 2's code (`TraceWriter::append`, the schema, `read_events`), so it stacks on PR 2 rather than `main`.
- **Verified source snapshot:** the `verify.py build` receipt fingerprints this PR's Rust sources to `dc7b11dcef3c7b34cb66bf10d9197ea6c439c517d965b5f9d6189a8437c3b56a` (sha256 over `Cargo.*`, `proto/**`, `crates/**/src/**`).
- **Merge hold:** this PR stays unmerged until Derek has reviewed all issue #3 reports together. The issue stays open.

## What it does

Claude Code gives every tool call a `tool_use_id` that is unique within its session. This PR carries it from the hook to the trace and gives the coordinator two identity rules:

- A session binds to the slot of its first recorded event, for the rest of the run. A call that names the session under another slot is denied and recorded nowhere. A new session on the same slot (a restart) continues that slot's clock.
- A call is known by `(session_id, tool_use_id)`. A second BeforeTool for the same pair is denied and recorded nowhere. Equal ids in different sessions are different calls.

Empty `session_id` or `tool_use_id` is denied the same way. Every denial records no event, uses no event id, ticks no clock and changes no binding. These rules give PR 4 what it needs to pair an AfterTool with its BeforeTool event.

## Core modules

- **`proto/runtime.proto`:** `BeforeToolRequest.tool_use_id = 6`. The SPEC section 7 excerpt matches it. SPEC section 3 states the two rules, and the section 5 schema table gains the nullable `tool_use_id` column.
- **Shim** ([`hook.rs`](../../crates/shim/src/hook.rs)): reads the top-level `tool_use_id` like `session_id`. A PreToolUse without it is malformed input and fails closed (exit 2). The raw `tool_input` is still forwarded byte for byte. The shim does not check for empty strings; the core is the one place that checks identity.
- **`core::identity`** ([`identity.rs`](../../crates/coordinator/src/core/identity.rs), new): `Identities` holds `session_id -> slot` and `CallKey(session_id, tool_use_id) -> Call { before_event_id, tool }`. `record_before` does all the checks and the insert in one call: on `Err(Refusal)` nothing has changed. There are no completion fields and no AfterTool API yet.
- **`core::step`** ([`core/mod.rs`](../../crates/coordinator/src/core/mod.rs)): first the unknown-slot Deny as before, then `record_before` on the copied state, then the clock tick and the event, which now carries `tool_use_id`. A refusal returns the state unchanged with only an `Admit(Deny)`. `State` exposes `session_slot` and `call` for tests.
- **Adapter** ([`adapter.rs`](../../crates/coordinator/src/adapter.rs)): copies the wire field. `before_tool` is otherwise unchanged. It steps on a clone and commits only after every append succeeds, so a failed blob write or INSERT also discards the new binding and the call identity.
- **Trace store** ([`lib.rs`](../../crates/trace-store/src/lib.rs)): `Event.tool_use_id: Option<String>` and a nullable `tool_use_id TEXT` column, appended last.
  - `read_events` opens read-only and checks `pragma_table_info`. A trace without the column (issue #2 or PR 2) reads with `tool_use_id = None`. Nothing is invented and nothing is written.
  - `TraceWriter::create` adds the column with `ALTER TABLE` only to an empty trace, after the `RunNotEmpty` refusal. A populated legacy trace is refused before any schema change. Coordinator resume is still unsupported.

## Runtime flow

1. The shim parses PreToolUse and sends `slot, session_id, tool, tool_input_json, intent, tool_use_id`.
2. `before_tool` locks and steps on a clone. Unknown slot gives Deny. Then empty ids, a session bound to another slot, or a repeated `(session, tool_use_id)` give Deny. In all of these cases there is no Record and the state is unchanged.
3. Otherwise the copy binds the session (first time only), remembers the call with its event id, ticks the slot clock, and emits one Record and Allow.
4. Append: blob, then the row with `tool_use_id`. Commit the copy. Reply.
5. If the append fails, the copy is dropped. The retry of the same call gets the same id and stamp, and it is not refused as a repeat of itself.

## Tradeoffs and decisions

- **A refusal is a Deny (exit 0 with a deny decision), not an Err (exit 2).** This matches the unknown-slot path: both block the call, and a Deny gives the model a reason. A missing field is still malformed input and gives exit 2.
- **Identity lives only in coordinator memory.** It is lost when the coordinator stops, and a stopped run cannot be restarted (`RunNotEmpty`), so it is never rebuilt from a trace. If a reply is lost after a successful append, Claude Code may still run the call, and the same id is then refused as a repeat. Claude Code does not resend a PreToolUse for the same `tool_use_id` today. This was not tested against a live session.
- **Every recorded call stays in memory for the run.** It costs one map entry per call, and `State::clone` per call grows with the run. Not measured.
- **`Call.tool` is kept** so PR 4 can check that an AfterTool names the same tool. It is read today only through `State::call` in tests.
- **The column is added only to empty legacy traces.** This keeps the "writers start empty" rule and never rewrites recorded history.

## Agent verification (2026-11-09, this checkout, inside the Claude Code sandbox)

| Check | Result |
| --- | --- |
| `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`; `scripts/check-file-lines.sh` | pass |
| `cargo test --workspace --locked --offline --no-fail-fast` | 122 tests: 106 pass. All 16 failures panic at a Unix-socket bind with `Operation not permitted` (coordinator lib 3, `coordinator/tests/adapter.rs` 1, `shim/tests/cli.rs` 11, `shim/tests/e2e.rs` 1). **Not yet run with socket binding allowed.** |
| New tests (pass in the sandbox) | `coordinator/tests/identity.rs` 7: restart continues the clock, same session on another slot refused, repeated BeforeTool refused, equal ids in two sessions distinct, overlapping calls keep their own event ids, empty ids refused and bind nothing, unknown slot binds nothing. `admission_durability.rs` +1: a failed INSERT on a new session's first call binds nothing and uses no identity, then the same call is admitted from the other slot as event 1 `[1,0]`, and a repeat or a rebind is denied with no row. `trace-store/tests/call_identity.rs` 4: id stored, NULL stored, issue #2 and PR 2 shaped traces read with `None` and are left byte-identical, empty legacy trace gains the column. |
| Updated tests | Existing helpers now send a unique, real `tool_use_id` per call; no assertion was weakened. The concurrent test (80 calls) now also requires every row's `tool_use_id` to name its own call. The adapter integration test now uses one session per slot; it used to send one session from two slots, which is now refused. |
| `formal/admission/check.sh` | RESULT: PASS. Baseline 75424 generated / 48960 distinct / depth 15, all rules hold, including new (g) `SessionStaysOnSlot` and `CallRecordedOnce` and the identity part of `CoreMatchesTrace`. 4/4 witnesses reached (2 new: identity refusal; failed call retried with its identity). 10/10 broken variants caught (3 new: `no_binding_check`, `no_repeat_check`, `identity_before_append`). Sync PASS after the refresh, now covering `core/identity.rs`. Code: `failed_append_keeps_the_old_state` and `admission_durability` (7 tests) pass. Chart scope: 91 distinct, no error. |
| Real-code mutation | Disabling the repeat check in `record_before` failed `a_repeated_before_tool_is_refused` and `a_failed_append_binds_no_session_and_uses_no_call_identity`. Disabling the binding check failed `a_session_cannot_move_to_another_slot` and the same durability test. Afterwards `identity.rs` was restored and its sha256 matched (`c0f70de0…`). Logs: `.claude/reviews/evidence/identity-mutation/` (gitignored). |
| Legacy traces, real writer code | A consumer built against the parent's `trace-store` (`git archive f2d21d8`) wrote a PR 2 trace with a blob. A second consumer built against this branch read it, and a copy of the genuine issue #2 trace (`.claude/reviews/real-legacy-trace/`, sha256 `b26bd37c…`, unchanged). Both PASS: `tool_use_id` is `None`, the payload bytes are exact, the writer is refused with `RunNotEmpty`, and the `trace.db` bytes and columns are unchanged. The PR 2 trace came from the library, not through the binaries, because sockets are blocked here. Sources: `.claude/reviews/evidence/pr3-legacy-compat/` (gitignored). |
| `verify.py build` / `doctor` | Build and receipt OK. Doctor is BLOCKED only on Unix-socket bind (EPERM); every other check passes. |
| `verify.py drive all` | **Not run** (needs socket binding). New journey `call-identity`: refusals for a repeat, a rebound session and empty ids, with no row and one blob; a restart, an equal id in another session and two overlapping calls, with exact `(event_id, slot, session, tool_use_id, vclock)` rows. `slot-clocks` now gives its repeated Edit its own `tool_use_id`. `admitted-tool-call` checks the stored id, and `hook-input` checks a missing `tool_use_id`. |

Still to run where binding a Unix socket is allowed, in this order:

```sh
cargo test --workspace --locked --offline
.agents/skills/verify-agent-runtime/scripts/verify.py build
.agents/skills/verify-agent-runtime/scripts/verify.py doctor
.agents/skills/verify-agent-runtime/scripts/verify.py drive all
.agents/skills/verify-agent-runtime/scripts/verify.py cleanup
```

Hosted CI: not run (nothing pushed). Codex review: not run by this owner; the chief runs it.

## Limitations

- No AfterTool, Stop or SessionEnd yet (the subsequent completion and lifecycle PRs), so nothing reads `before_event_id` outside tests.
- The model has two calls, two sessions and two ids. Empty ids are covered by unit tests only. File-system durability remains an assumption, as in PR 2.
- No live Claude Code session was run. The hook JSON comes from recorded fixtures with edited ids. The dogfooding rows in `docs/research/dogfooding.md` stay `not run`.

## Human review

PENDING. No feedback from Derek yet.

## Final chief verification

The complete frozen implementation was verified with real dependencies and Unix-socket binding enabled:

- `cargo test --workspace --locked --offline`: 122 passed.
- Formatting, workspace Clippy with warnings denied, the 1,000-line cap, and diff checks passed.
- `verify.py build`, `doctor`, all eight journeys (77 checks), and cleanup passed. Identity refusal, restarted sessions, overlapping calls and exact payloads were exercised through real binaries, sockets, SQLite and blobs.
- Formal checks passed 48,960 distinct states, four witnesses, and all ten broken variants. Empty IDs are covered by code tests rather than the bounded model. Filesystem flushing remains an assumption.
- Independent native-agent recheck and separate Codex CLI review found no actionable regressions. The CLI sandbox could not bind Unix sockets; the chief's binding-enabled run above passed them all.

Numbered command logs, source manifests and review output are retained locally under `.claude/reviews/issue3-pr3-*`. Hosted CI is shown on the attached PR. Human feedback is still pending; none of these automated results authorize merging.

