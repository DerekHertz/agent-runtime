# Issue #3 PR 5 walkthrough: failed and interrupted AfterTool reports

**Status: draft. HUMAN FEEDBACK PENDING.** Derek has not reviewed this PR. Nothing below is his feedback. When he comments, record it in `docs/reviews/pr-<number>.md` once the PR number exists, and keep this draft as the walkthrough presented.

- **Branch:** `codex/issue-3-failed-tools`, PR 5 of the [issue #3 plan](../plans/issue-3.md). Implementation commit `7d0fe7994d06913e4e48f5a5a8d3a5d4bb7f739a`; later report-only commits retain these verified runtime sources.
- **Parent and base:** `c2e2a2edbfd2dde2c4ef9126a8b302bab10ae3a0` on `codex/issue-3-after-tool` (PR 4, [its walkthrough](issue-3-after-tool.md)). It stacks on PR 4 because it extends PR 4's `AfterTool` oneof, `core::completion`, `Outcome` and trace schema.
- **Scope:** PostToolUseFailure only: a failed or interrupted tool call becomes an AfterTool with outcome `failure`. Stop, SessionEnd and the acceptance run are PR 6. No lifecycle events.
- **Merge hold:** unmerged until Derek has reviewed all issue #3 reports together. The issue stays open.

## Independent chief verification (2026-11-09 Pacific / 2026-11-10 UTC)

**PASS at implementation `7d0fe7994d06913e4e48f5a5a8d3a5d4bb7f739a`.** All 30 runtime source files match their committed Git objects; aggregate SHA256 `5bd62fb1095d6ac24bbe151feca48ebabf65df9432032d9ee295a62cce22bd96`. The task diff stayed unchanged during execution. Hosted results are tracked on this PR's Checks page.

| Check | Verified result |
| --- | --- |
| Workspace tests and static checks | **192 Rust tests passed**, including all 29 socket-dependent tests blocked in the author's sandbox. Formatting, Clippy with warnings denied, the 1,000-line cap and whitespace checks pass. |
| Real-binary verification | **10 journeys, 141 checks passed** through the actual shim, coordinator, Unix sockets, SQLite and blobs. End-of-run sources, receipt and binary hashes match. |
| Formal verification | **4,492,736 distinct states**, all 10 witnesses reached and all 17 deliberately broken variants caught. Source sync and the actual persistence tests pass. Optional interruption values and payload contents are checked by Rust tests, outside the model's abstraction. |
| Freshness proof | Baseline PASS/0; source and binary perturbations FAIL/1 after the real journey passes. Originals remain unchanged. |
| Genuine legacy trace | The issue #2 trace returns its exact original input and absent new metadata, with every database/run-directory byte unchanged. The reader crate matches the source used by the successful probe. An initial external-probe compile error assumed a flat interrupt field; the probe was adapted to the typed Outcome API and rerun. |
| Reviews and fixture provenance | Native product, journey/model and separate Codex CLI reviews have no remaining findings. The base false-flag fixture matches the recorded M0 e6 event after three path normalizations; true and omitted flags are synthetic variants. No new live comparison was run. |

Reproduce through the [verification skill](../../.agents/skills/verify-agent-runtime/SKILL.md) and [formal skill](../../.agents/skills/formal-verify-admission/SKILL.md). Local raw evidence is gitignored: `.claude/reviews/issue3-pr5-verified/`, `issue3-pr5-legacy/`, `issue3-pr5-fixture-provenance.json` and `issue3-pr5-codex-review.txt`. The author results below describe the earlier sandbox run; this section is the independent local verdict.

## What it does

A shim registered as `art-shim --hook-event PostToolUseFailure` reports each received PostToolUseFailure hook. The coordinator records one AfterTool event that pairs with the call's BeforeTool by `(session_id, tool_use_id)`, exactly as a success does. The event's payload is the raw JSON text of the hook's `error` string, its input blob is the raw `tool_input` the report carried, and its outcome is `failure` with the hook's `is_interrupt` kept as `true`, `false` or omitted. A recorded report exits 0 silently; a report that cannot be made exits 1 with a reason on stderr, never 2.

## Core modules

- **Proto** ([`runtime.proto`](../../proto/runtime.proto), SPEC section 7): the existing `oneof result` gains `ToolFailure failure = 7` with `string error_json = 1` and `optional bool is_interrupt = 2`. proto3 `optional` gives the field presence, so "omitted" reaches the coordinator as `None`, not `false`. An unset oneof is still `InvalidArgument`.
- **Shim** ([`cli.rs`](../../crates/shim/src/cli.rs), [`hook.rs`](../../crates/shim/src/hook.rs)):
  - `HookEvent::PostToolUseFailure` with `Policy::Nonblocking`. `cli::parse` recognizes it before arity, so `--hook-event PostToolUseFailure extra` exits 1. Only Stop and SessionEnd remain "not handled yet" report events.
  - `hook::parse_input` reads `error` and `is_interrupt` as raw JSON with the `present` deserializer, so `null` counts as present. `hook::failure` requires `error` and requires it to be a string (its raw text starts with `"`), and forwards that raw text unchanged, escapes included. `is_interrupt` may be omitted; when present it must be `true` or `false`. Anything else, `null` included, is refused. The stdin event must be PostToolUseFailure, as before.
  - Fixture `tests/fixtures/post_tool_use_failure_bash.json` is the M0 probe's observed PostToolUseFailure (Claude Code 2.1.318, e6) with local paths shortened to `/work`. It carries `error`, `is_interrupt: false` and `duration_ms`.
- **Core** ([`completion.rs`](../../crates/coordinator/src/core/completion.rs)): `ToolResult::Failure { error_json, is_interrupt }`. `ToolResult::into_parts` gives the payload (response or error), the outcome and the field name for the refusal. `after_tool` keeps its order: unknown slot, empty tool, empty input, empty payload ("no error"), then `Identities::record_after`, then one tick. `record_after` is unchanged, so a failure pairs, binds, orphans and completes exactly like a success, and the first report of either outcome stays the completion.
- **Adapter** ([`adapter.rs`](../../crates/coordinator/src/adapter.rs)): maps `failure` to `ToolResult::Failure`, passing `is_interrupt` through as `Option<bool>`. `step_and_record` is unchanged: lock, step on a clone, blobs, row, commit, reply.
- **Trace store** ([`lib.rs`](../../crates/trace-store/src/lib.rs)): `Outcome::Failure { is_interrupt: Option<bool> }`. A new nullable `is_interrupt` column (added after `input_payload_hash` in `LATER_COLUMNS`) stores 1, 0 or NULL. `append` writes it in the same single INSERT, after the same two blobs. `read_events` builds the outcome from both columns through `Outcome::from_columns` and refuses any pair `append` never writes (`success` with a flag, a flag with no outcome, a value other than 0 or 1, an unknown text) as `Error::UnknownOutcome`.

## Runtime flow

1. Claude Code runs `art-shim --hook-event PostToolUseFailure`. The policy is "exit 1 on failure" before config, stdin or a panic. Outside a run it exits 0 and contacts nobody.
2. The shim checks the event, `error` and `is_interrupt`, and sends `AfterToolReport { result: failure }` with the raw input and raw error text.
3. `step_and_record` locks and steps on a clone. An unknown slot, empty tool, input or error field, another slot, or another tool gets a refused Ack and no Record.
4. Otherwise the copy binds the session, pairs by identity or records an orphan, sets the completion only if the call has none, and ticks the slot. Blob (error), blob (input), row. Commit. Ack.
5. If a write fails, the copy is dropped: no id, stamp, binding or completion is used; the corrected retry gets the same id and stamp and still pairs. The shim exits 1.

## Decisions and tradeoffs

- **No envelope.** The interruption flag has its own column, so the payload can be the hook's own `error` value. A synthesized `{"error":…,"is_interrupt":…}` blob would have mixed hook bytes with bytes we wrote, and the trace could not then claim its payloads are raw. The cost is one more nullable column.
- **Outcome `failure`, flag separate.** `interrupted` is not a third outcome: it would duplicate `is_interrupt = 1` and give "omitted" nowhere to go. A reader asking "was it interrupted?" reads the column and must treat NULL as unknown.
- **The error is JSON text.** `error_json` is `"…"` with quotes, like `tool_response_json`. An empty message is the two bytes `""` and is recorded; an empty field is a missing error and is refused. Type checks happen at the hook boundary (the shim). The coordinator refuses only an empty field: it has no JSON parser, so a non-shim client could send a non-string `error_json`. Today the shim is the only client.
- **`null` is refused** for both fields. The schema says `error` is a string and `is_interrupt` an optional boolean; `null` is neither, and accepting it as "omitted" would be a guess.
- **Mixed outcomes are history.** A success after a failure, or a failure after a success, is its own row naming the same BeforeTool. Nothing is overwritten and no BeforeTool is reused for another call. The first report is the completion in memory; in the trace the earliest AfterTool naming a BeforeTool is the completion, as in PR 4.
- **Registration.** `docs/hook-registration.md` adds a PostToolUseFailure entry with a 30 s timeout. Matching the call's tool name is still required, so a failure report naming another tool than its BeforeTool is refused (exit 1).
- **Size.** About 680 changed lines in tracked files plus 12 new files (about 1,500 lines). Production Rust is about 160 lines; the rest is tests (core 7, durability 5, trace store 5, adapter 1, shim unit 4, CLI 3, e2e 1), the TLA+ model and configs, the verify journey and docs.

## Agent verification (2026-11-09, this checkout, inside the Claude Code sandbox)

These are this author's runs, not the chief's independent verdict.

| Check | Result |
| --- | --- |
| `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`; `scripts/check-file-lines.sh`; `git diff --check`; trailing whitespace in untracked files | pass |
| `cargo test --workspace --locked --offline --no-fail-fast` | 192 tests: 163 pass. All 29 failures are the Unix-socket bind `Operation not permitted`: the parent's 25 (coordinator lib 3, `coordinator/tests/adapter.rs` 1, `shim/tests/cli.rs` 19, `shim/tests/e2e.rs` 2) plus this PR's 4 (`shim/tests/cli.rs` 3, `shim/tests/e2e_failure.rs` 1). **BLOCKED, not run with binding allowed:** a retry outside the sandbox was refused by the permission gate. |
| New tests passing in the sandbox | `coordinator/tests/failure.rs` 7: reverse-order pairing with flag `Some(true)`/`Some(false)` and one tick each; omitted flag stays `None` and an empty message `""` is recorded; orphan keeps input and error, late BeforeTool refused; success after failure and failure after success keep the first completion; repeated failure; seven refusals change nothing and the corrected report is event 2; a refused orphan failure binds nothing. `coordinator/tests/failure_durability.rs` 5 (real RPC, SQLite, blobs): failed error blob then retry with the same id and pair; failed INSERT leaves orphan blobs, retry pairs, stored `is_interrupt` NULL versus 0 read with plain SQL; failed orphan binds no session; mixed outcomes; wire refusals (empty error field, default `ToolFailure`, other tool) with no row or blob. `adapter::tests::failed_failure_report_append_completes_nothing`. `trace-store/tests/failure.rs` 5: round trip of all three flags with raw error bytes, success stores NULL, six invalid stored pairs refused, a PR 4-shaped trace reads unchanged and is refused for writing, an empty PR 4 trace gains the column. Shim unit: 4 new in `hook.rs` and 3 extended in `cli.rs` (33 in all): the observed fixture's raw input and error, escapes and spacing kept, missing and mistyped fields (`error` null, number, object, array, bool; `is_interrupt` null, string, number), registration mismatch both ways. |
| New tests not run here (need sockets) | `shim/tests/cli.rs` +3: a silent failure report carries raw input, error and flag (fixture, omitted, interrupted); refused, unreachable or half-configured reports exit 1; malformed, mistyped or mismatched input exits 1 without contact. Existing CLI tests gained the failure fixture for the legacy and outside-run cases. `shim/tests/e2e_failure.rs` 1: real shim to real coordinator: two BeforeTools, reversed failures (interrupted, fixture), a repeat with `""` and no flag, an orphan, a tool mismatch refused, a read-only `blobs/` failure then the corrected retry as event 7 at `[7, 0]`, exact payload and input bytes, and every blob named. |
| `formal/admission/check.sh` | **RESULT: PASS** (31 rows). Baseline with `Kinds = {"B", "A", "F"}`: 6,935,600 generated, **4,492,736 distinct**, depth 19, all 13 rules hold, including the new (i) `RowKeepsItsOutcome`. **10/10 witnesses** reached (new: failure retry, mixed outcomes). **17/17 broken variants** caught (new: `failure_as_success` on `RowKeepsItsOutcome`, `success_only_completion` on `CoreMatchesTrace`). Sync PASS after the refresh. Code proofs: 3 adapter tests, `admission_durability` 8, `after_tool_durability` 6, `failure_durability` 5. The baseline took 44 s and the model section about 6 minutes on 12 cores. |
| Real-code mutation | Four scratch mutations, each applied to the real file, tested, restored from a byte copy with a fresh mtime and checked by sha256: `failure_as_success`, `success_only_completion` (identity.rs completes only when told it is a success), `omitted_interrupt_read_as_false` (hook.rs) and `store_false_as_null` (lib.rs). Each made at least one named test fail; the restored sources then passed again. Evidence: `.claude/reviews/evidence/failed-tools-mutation/summary.json` (gitignored). |
| `verify.py build` | OK. Cleaned the five workspace packages only. Receipt: sources `5bd62fb1095d6ac24bbe151feca48ebabf65df9432032d9ee295a62cce22bd96` (30 files), `art-coordinator` `b8ff6775…710d`, `art-shim` `5faa2bde…1d7d`. |
| `verify.py doctor` | Every check passes except the Unix-socket bind (`EPERM`). |
| Journey inputs, no socket | Every `failed-tool` journey input was piped to the built `art-shim` with an unbound socket path: the seven valid reports got past parsing and failed only at connect; the nine refused ones (seven bad inputs, a trailing argument, the success registration) failed with the reason the journey expects. This checks the inputs, not the journey. |
| `verify.py drive all` (new journey `failed-tool`), `freshness_proof.py`, socket tests | **Not run** (need socket binding). The chief runs them with binding allowed, `verify.py build` last before `drive`. |
| Codex review | **Not run.** This assignment allows no nested agents, so independent review is incomplete until the chief runs it. |

Still to run where binding a Unix socket is allowed:

```sh
cargo test --workspace --locked --offline
.agents/skills/verify-agent-runtime/scripts/verify.py build
.agents/skills/verify-agent-runtime/scripts/verify.py doctor
.agents/skills/verify-agent-runtime/scripts/verify.py drive all
.agents/skills/verify-agent-runtime/scripts/freshness_proof.py
.agents/skills/verify-agent-runtime/scripts/verify.py cleanup
```

## Formal model

The order of lock, step, blobs, append, commit and reply did not change, so every action keeps its code line. `Kinds` gains `"F"`, a failed report. `IsAfter` sends it through the AfterTool branch of `Step` and the same `Writes`; its payload blob is its own error (`ErrBlob`). Every row carries `outcome` (`"-"`, `"success"`, `"failure"`), and rule (i) `RowKeepsItsOutcome` checks it against the report. `CoreMatchesTrace` already counts every AfterTool row for the completion, so it now checks first-completion across outcomes. The interruption flag is opaque data to the model, like payload bytes; the real tests cover its three states. The chart scope (`AdmissionChart.cfg`) keeps `{"B", "A"}` and was not redrawn. `SYNC.sha256` was refreshed for `adapter.rs`, `core/completion.rs`, `core/identity.rs` (comment only) and `trace-store/src/lib.rs` after the coverage table in the [formal skill](../../.agents/skills/formal-verify-admission/SKILL.md) was rechecked line by line.

## Agent findings and repairs

No independent review has run yet, so there are no review findings. Issues the author found and fixed before freezing:

- The first draft of the model referenced `kindOf` in definitions placed before `VARIABLES`; TLC refused to parse it. The definitions moved below the variables. No rule changed.
- The editing tool turned `é` escapes in three test literals into the literal character, which would have weakened the "escapes kept" tests. The literals were restored byte by byte and rechecked with `grep`.

## Limitations

- This records failure hooks received by the runtime. The official hook reference says cancelling a running tool does not fire PostToolUseFailure; the synthetic interruption case does not prove that every cancelled tool is traced.

- No new live Claude Code session was run for this PR. The failure fixture is one observed PostToolUseFailure with paths shortened. No interrupted call (`is_interrupt: true`) or omitted flag was observed live; those inputs are edited fixtures. The dogfooding rows stay `not run`.
- The coordinator does not check that `error_json` is a JSON string; the shim does.
- No coordinator resume: bindings, calls and completions live in memory for one run.
- The model's bounds are unchanged: two concurrent calls plus one seeded BeforeTool. Three reports of one call (for example failure, retry failure, success) are covered by real tests only.
- The panic-to-exit-1 path is still covered by a policy unit test and by reading the code, not by forcing a panic in the binary.
- Orphan blobs from failed appends are never collected.

## Human review

PENDING. No feedback from Derek yet.
