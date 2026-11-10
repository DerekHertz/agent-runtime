# Issue #3 PR 4 walkthrough: successful AfterTool reports

**Status: draft. HUMAN FEEDBACK PENDING.** Derek has not reviewed this PR. Nothing below is his feedback. When he comments, record it in `docs/reviews/pr-<number>.md` once the PR number exists, and keep this draft as the walkthrough presented.

- **Branch:** `codex/issue-3-after-tool`, PR 4 of the [issue #3 plan](../plans/issue-3.md). Uncommitted working tree; the chief commits and publishes.
- **Parent and base:** `717ab08013150b8d06e656f475edf92cf22540fe` on `codex/issue-3-call-identity` (PR 3, [its walkthrough](issue-3-call-identity.md)). It stacks on PR 3 because it extends PR 3's `Identities`, schema and reader.
- **Scope:** successful PostToolUse only. Failed and interrupted reports (PostToolUseFailure) are PR 5; Stop, SessionEnd and the acceptance run are PR 6. The six-PR breakdown is unchanged.
- **Merge hold:** unmerged until Derek has reviewed all issue #3 reports together. The issue stays open.

## What it does

A shim registered as `art-shim --hook-event PostToolUse` reports each successful tool call. The coordinator records one AfterTool event that names the call's BeforeTool by `(session_id, tool_use_id)`, keeps the raw tool response as the event's payload, and keeps the raw tool input the report carried as a second blob. A recorded report exits 0 silently. A report that cannot be made exits 1 with a reason on stderr, never 2.

## Core modules

- **Proto** ([`runtime.proto`](../../proto/runtime.proto), SPEC section 7): `rpc AfterTool(AfterToolReport) returns (Ack)`. The report carries slot, session, tool, raw `tool_input_json`, `tool_use_id` and `oneof result { ToolSuccess success = 6; }`. An unset `result` is refused with `InvalidArgument`; it is never read as success. An empty tool, input or response string is refused by the core.
- **Shim** ([`cli.rs`](../../crates/shim/src/cli.rs) new, [`main.rs`](../../crates/shim/src/main.rs), [`hook.rs`](../../crates/shim/src/hook.rs), [`transport.rs`](../../crates/shim/src/transport.rs)):
  - `cli::parse` reads `--hook-event` first, so `Policy` (`FailClosed` exit 2, `Nonblocking` exit 1) is fixed before config, stdin, or the `catch_unwind` panic path. The event named after `--hook-event` sets the policy before arity is checked, so stray trailing arguments after a report event still exit 1. No arguments is the issue #2 PreToolUse gate and still ignores other events.
  - `hook::parse_input` requires the stdin event to match the registered one. `tool_response` uses a `present` deserializer, so `null`, numbers, strings and odd spacing are kept as raw text. A plain `Option<Box<RawValue>>` would have read `null` as missing. A `null` tool input still counts as missing, as before.
  - `transport::after_tool` bounds connect and RPC at 10 s (`REPORT_TIMEOUT`). The documented hook timeout is 30 s.
- **Core** ([`completion.rs`](../../crates/coordinator/src/core/completion.rs) new, [`identity.rs`](../../crates/coordinator/src/core/identity.rs), [`core/mod.rs`](../../crates/coordinator/src/core/mod.rs)):
  - `Call` is now `{ before_event_id: Option<u64>, tool, completed_event_id: Option<u64> }`.
  - `completion::after_tool` refuses an unknown slot, then an empty tool, input or response, before any identity or clock change.
  - `Identities::record_after` refuses empty ids, a session bound to another slot, or a known call named with another tool. It binds the session, keeps the first completion (`get_or_insert`) and returns the BeforeTool id, or `None` for an orphan. An orphan is remembered, so its repeats pair the same way and a late BeforeTool for it is refused as a repeat.
  - `step` dispatches to `before_tool` or `completion::after_tool`. Each accepted report ticks its slot (clock rule 1, no merge) and emits one Record and `Output::Ack(Ack::Recorded)`. A refusal emits only `Ack::Refused { reason }` and leaves the state unchanged.
- **Adapter** ([`adapter.rs`](../../crates/coordinator/src/adapter.rs)): `before_tool` became `step_and_record`, shared by both RPCs: lock, step on a clone, append, commit, reply, in the same order as before. A refused report becomes `FailedPrecondition`.
- **Trace store** ([`lib.rs`](../../crates/trace-store/src/lib.rs)):
  - `Event.tool_input` was overloaded once AfterTool had two payloads, so it is renamed `payload` (the bytes under `payload_hash`: a BeforeTool's input, an AfterTool's response) with a new `input_payload` / `input_payload_hash` pair. Every caller was migrated. There is one new reader, `read_input_payload`.
  - New nullable columns `before_event_id`, `outcome` (`success`) and `input_payload_hash`. `append` puts the payload blob, then the input blob, then runs one INSERT.
  - `read_events` reads a trace that lacks any later column with NULL in its place, read-only. `TraceWriter::create` adds missing columns only to an empty trace; a populated legacy trace is still refused with `RunNotEmpty` before any schema change.

## Runtime flow

1. Claude Code runs `art-shim --hook-event PostToolUse` with the hook JSON. The policy is now "exit 1 on failure". With no slot variables the shim exits 0 and contacts nobody.
2. The shim checks that the event is PostToolUse and sends `AfterToolReport` with raw input and response.
3. `step_and_record` locks and steps on a clone. An unknown slot, empty ids or payloads, another slot or another tool give a refused Ack with no Record.
4. Otherwise the copy binds the session, pairs by identity (or records an orphan), keeps or sets the first completion, and ticks the slot. Blob (response), then blob (input), then the row. Commit. Ack.
5. If any write fails, the copy is dropped: no id, stamp, binding or completion is used, and the retry pairs the same way with the same id and stamp. The shim exits 1.

## Tradeoffs and decisions

- **Pairing ignores the input.** A hook may rewrite the input between PreToolUse and PostToolUse. Both versions are kept: the BeforeTool's payload and the AfterTool's `input_payload_hash`. Matching inputs would have mispaired or orphaned such calls.
- **The response is the primary payload.** This matches SPEC "hash of the tool input or output". The input goes in its own column, not an envelope blob, so an orphan keeps its input and equal inputs share one blob with the BeforeTool.
- **A repeated report is a new row, not a refusal.** History is never overwritten. The first completion stays authoritative in memory. In the trace the repeats are told apart by event order (the earliest AfterTool naming a BeforeTool is the completion); no extra column was added.
- **A tool mismatch is refused** (exit 1, no row) instead of being recorded as an orphan, because the identity is known and the report contradicts it.
- **A bad command line inside a run fails closed** (exit 2), because it may be a gate. The exception is a command line that starts `--hook-event <report event>` (PostToolUse, or PostToolUseFailure, Stop or SessionEnd, which this build does not handle yet). It exits 1 even with stray trailing arguments, so an early or mistyped Stop registration can never block stopping. `--hook-event PreToolUse extra` and unknown or malformed hints still exit 2. The hint is trusted. A registration that names the wrong event gets that event's policy, and nothing here protects against that. Outside a run every command line is silent.
- **Report timeout.** The shim gives up after 10 s, under the documented 30 s hook timeout, so it can always say why. Every report registration needs at least 20 s. SessionEnd's 1.5 s default budget must be raised to at least 20 s when its PR registers it (`docs/hook-registration.md`). A report it gave up on may still have been recorded if the coordinator answered late. A retry would then be recorded as a repeat, which is visible and does not overwrite anything.
- **Size.** About 1,780 changed lines in tracked files plus 18 new files. Most of it is tests (around 900 lines), the TLA+ model and its 24 configs, the skills and the docs. Production Rust is roughly 450 lines across the shim, core, adapter and trace store.

## Agent verification (2026-11-09, this checkout, inside the Claude Code sandbox)

This is the first author's run, before the review repair below. It is kept as recorded. The repair's results are in [Agent review round 1](#agent-review-round-1-finding-and-repair).

- **Verified source snapshot:** the `verify.py build` receipt fingerprints this PR's Rust sources to `03d7331b8c3a5a149fe9081d9f0915696bb74fedd46724ce6d75a501dedb62e0`.

| Check | Result |
| --- | --- |
| `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`; `scripts/check-file-lines.sh`; `git diff --check` | pass |
| `cargo test --workspace --locked --offline --no-fail-fast` | 159 tests: 134 pass. All 25 failures panic at a Unix-socket bind with `Operation not permitted`: coordinator lib 3, `coordinator/tests/adapter.rs` 1, `shim/tests/cli.rs` 19, `shim/tests/e2e.rs` 2. **Not run with socket binding allowed**: the unsandboxed run was refused by the permission gate. |
| New tests passing in the sandbox | `coordinator/tests/completion.rs` 7: reverse-order pairing, equal ids in two sessions, restarted session clock, changed input plus `null` and scalar responses, orphan never guessed (and late BeforeTool refused), repeated completion keeps the first, seven refusals with nothing changed. `coordinator/tests/after_tool_durability.rs` 5: failed response blob, failed input blob after an existing response blob, failed INSERT with orphan blobs and a reverse-order retry with the same ids and stamps, failed orphan binds no session, and wire refusals (no `result`, empty response, other tool). `adapter::tests::failed_after_tool_append_completes_nothing` checks the committed completion directly. `trace-store/tests/after_tool.rs` 6: response, input, `before_event_id` and `outcome` round trip, `null` and scalars byte-exact, orphan input kept, failed INSERT leaves both blobs, unknown outcome is an error, PR 3-shaped trace read with NULLs and left byte-identical, empty PR 3 trace gains the columns. Shim unit 27 (`cli.rs` and `hook.rs`: policy per registration, event mismatch, raw `null`, missing fields). |
| New tests not run here (need sockets) | `shim/tests/cli.rs` +10: silent recorded report with raw input and `null`, refused report exit 1, missing or dead socket and half-set env exit 1, malformed, mismatched or response-less input exit 1 without contact, hung coordinator ends within the 10 s bound, silent outside a run, bad command line exit 2 but `--hook-event Stop` exit 1, registered PreToolUse gate. `shim/tests/e2e.rs` +1: real shim to real coordinator, BeforeA, BeforeB, AfterB (`null`), AfterA, exact rows, stamps and blobs. |
| `formal/admission/check.sh` | RESULT: PASS. Baseline 2892816 generated / 1883120 distinct / depth 19, all 12 rules hold, including the new (h) `CallKeepsItsTool` and `AfterPairsWithItsBefore` and the completion part of `CoreMatchesTrace`. 8/8 witnesses reached (originally recorded as 9/9; corrected in round 2, see below) (4 new: failed completion retried and paired, repeated completion, orphan after another event, out-of-order pairing). 15/15 broken variants caught (5 new: `completion_before_append`, `last_completion`, `guess_before`, `no_tool_check`, `input_blob_after_row`). Sync PASS after the refresh, now covering `core/completion.rs`. Code proofs: 2 adapter unit tests, `admission_durability` 7, `after_tool_durability` 5. About 4 minutes on 8 cores; TLC now runs with `-workers auto`. Chart scope: 1164 distinct, no error; the statechart was not redrawn. |
| Real-code mutation | Five scratch mutations, each applied to the real file, tested, and restored with a matching sha256: last completion wins, no tool check, orphan guessed from the latest event, completion committed before the append, input blob after the row. Each made at least one test fail. Evidence: `.claude/reviews/evidence/after-tool-mutation/summary.json` (gitignored). |
| `verify.py build` / `doctor` | Build and receipt OK. Doctor is BLOCKED only on Unix-socket bind (`EPERM` with `--scratch-root /tmp/claude-503`; `/tmp` itself is not writable here). Every other check passes. |
| `verify.py drive all` | **Not run** (needs socket binding). New journey `after-tool` (`scripts/after_tool.py`, [feature](../../.agents/skills/verify-agent-runtime/features/after-tool.md)): reversed completion with a `null` response, both inputs kept, repeat, orphan, equal id in another session, blobs match rows exactly, a tool-mismatch refusal and six nonblocking failures (exit 1), silent outside a run, the legacy registration still ignoring PostToolUse, and exit 1 after the coordinator stops. The journey code has not been executed. |

Still to run where binding a Unix socket is allowed, in this order:

```sh
cargo test --workspace --locked --offline
.agents/skills/verify-agent-runtime/scripts/verify.py build
.agents/skills/verify-agent-runtime/scripts/verify.py doctor
.agents/skills/verify-agent-runtime/scripts/verify.py drive all
.agents/skills/verify-agent-runtime/scripts/verify.py cleanup
```

## Agent review round 1: finding and repair

These are agent findings, not Derek's feedback. An independent Codex review of the frozen author diff reported one P2 and two hardening points. A fresh Claude repair owner checked each against the code before changing anything.

| Finding | Verified | Repair |
| --- | --- | --- |
| **P2.** An orphan AfterTool with `tool = ""` was accepted. It claimed the call's identity under `""`, bound the session, and the corrected retry was then refused `OtherTool`. | Yes. `completion::after_tool` checked input and response but not the tool. A new core test failed on the unfixed code at `refused`, because the report was recorded. The wire test was also red against a stale build of the unfixed core. | `after_tool` refuses an empty tool right after the unknown-slot check, before `record_after` and the tick. The unknown-slot and refusal policies are unchanged. Empty input or response strings are still refused, and raw JSON `null` is still accepted. |
| `--hook-event PostToolUse extra` exited 2, because arity was checked before the hint. | Yes (old `cli::parse` matched `[flag, value]` first). | `cli::parse` reads the value after `--hook-event` first. A recognized report event (PostToolUse, PostToolUseFailure, Stop, SessionEnd) makes every later command-line error nonblocking (exit 1). `PreToolUse extra`, unknown and malformed hints still exit 2. The hint is trusted. A registration that names the wrong event is not protected against. |
| The report deadline is 10 s, so every report registration needs more. | PostToolUse was already documented at 30 s. The SessionEnd note left its 1.5 s default unaddressed. No draft dogfood driver with hook settings is in the repo, so no harness file needed a change. | `docs/hook-registration.md` requires at least 20 s for every report registration, and SessionEnd's per-hook timeout must be at least 20 s when its PR registers it. |

**New checks.**
- Core: `an_orphan_with_no_tool_is_refused_and_its_corrected_retry_is_recorded` covers both an orphan and a known call. Nothing is claimed or bound. The corrected orphan gets id 2 and stamp `[0, 1]` and is recorded as `Edit`. The corrected known-call report pairs with its BeforeTool at id 3, `[2, 0]`.
- Wire: `an_orphan_with_no_tool_claims_nothing_and_the_corrected_retry_is_recorded` gets `FailedPrecondition`, and no row or blob is written. The retry on the other slot is event 1, `[1, 0]`, an orphan.
- Shim unit: two parser tests for trailing arguments. The binary test `trailing_arguments_keep_the_registered_events_policy` needs no socket, because the shim refuses the command line before it connects.
- Journey `after-tool`: an empty-tool orphan (exit 1, `no tool`), `PostToolUse extra` (exit 1) and `PreToolUse extra` (exit 2).

**Formal model.** The model was not changed. Its `Tools` never contain the `NoTool` sentinel, so it already assumed every recorded call has a nonempty tool. The repair makes the code enforce that assumption. Only a model comment, the skill's coverage notes, the pinned `after_tool_durability` count (5 to 6) in `check.sh`, and the `completion.rs` hash in `SYNC.sha256` changed. The hash was refreshed after the alignment was checked.

**Results (2026-11-09, this checkout, inside the Claude Code sandbox).**

| Check | Result |
| --- | --- |
| `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`; `scripts/check-file-lines.sh`; `git diff --check`; trailing whitespace in untracked files | pass |
| `cargo test --workspace --locked --offline --no-fail-fast` | 164 tests: 139 pass. The 25 failures are the same socket-bind `Operation not permitted` set as above (coordinator lib 3, `coordinator/tests/adapter.rs` 1, `shim/tests/cli.rs` 19, `shim/tests/e2e.rs` 2). **BLOCKED (EPERM), not run with binding allowed.** The new binary CLI test passes in the sandbox. |
| `formal/admission/check.sh` | RESULT: PASS (28 rows): the baseline holds with 2892816 generated, 1883120 distinct and depth 19; 8/8 witnesses are reached (originally recorded as 9/9; corrected in round 2); 15/15 broken variants are caught; sync passes; the code proofs are 2 adapter tests, `admission_durability` 7 and `after_tool_durability` 6. The first run after the fix failed on the pinned count of 5 and showed `completion.rs` drift. Both were fixed before this run. |
| `verify.py build` | OK. Receipt `sources_aggregate_sha256` `dfa668b18dae172257d3d8decfeec58f4368f36f4a6ce82c3566e1e5cbacb534` (30 files). |
| `verify.py doctor` | BLOCKED only on the Unix-socket bind (`EPERM`). |
| `verify.py drive all` | **Not run** (needs sockets). The three new journey checks have not been executed. |

Codex review of this repair: not run by this owner. The chief runs it, along with the full pipeline with binding enabled.

**Residual, not changed in round 1 (fixed in round 2 below).** A BeforeTool with an empty `tool_name` is still admitted and records the call under `""`. Its AfterTool would now be refused: `no tool` for `""`, `OtherTool` for any other name. Claude Code is not known to send an empty tool name. Refusing it in BeforeTool would change gate policy, which is outside this slice.

Hosted CI: not run (nothing pushed). Codex review: not run by this owner; the chief runs it.

## Agent review round 2: verifier freshness, empty BeforeTool tool, build staleness

These are agent findings, not Derek's feedback. The round 1 results above are that author's runs, kept as history. They are not the final result. The chief adds independent results after this round freezes.

| Finding | Verified | Repair |
| --- | --- | --- |
| `verify.py drive` set PASS from journey checks alone. It recorded `sources.unchanged_during_run` without acting on it, and checked the build receipt only at the start (an existing bug that matters more now that journeys run longer). | Yes, by reading `cmd_drive`. | After the last journey, `end_checks` rerun the source fingerprint, compare the receipt with the start, and rerun doctor's receipt and binary checks. Any failure makes the run FAIL (exit 1). `summary.json` gains `end_checks` and `status_reasons`, which the console prints too. Journey checks, the doctor gate and BLOCKED are unchanged. |
| A BeforeTool with `tool = ""` was allowed and claimed `(session_id, tool_use_id)`, so its AfterTool could only be refused. | Yes (round 1 residual). Both new tests failed with the guard disabled and passed once it was restored. The restored file's sha256 matched, and the crate recompiled. | `core::before_tool` denies an empty tool right after the unknown-slot check, before `record_before` and the tick. Nothing is recorded, and no id, stamp or binding is used. This validates an invalid wire field. Gate policy for valid calls is unchanged. |
| The round 1 mutation check reused a stale compiled mutant, because the restored file kept an old mtime. | Yes (round 1 report). | `verify.py build` first runs `cargo clean -p` for every package that `cargo metadata --no-deps --locked --offline` lists, in the build's target dir. Third-party dependencies stay cached. A failed metadata or clean step writes no receipt. The receipt records `cleaned_packages`. |
| The witness count was wrong: `check.sh` runs eight witnesses (three named, five in its loop), not nine. | Yes, counted in `check.sh`. | Reporting fix only, with no missing witness: the counts above now say 8/8 and the formal skill says eight. |

**New checks.**
- Core: `tests/identity.rs` `an_empty_tool_is_refused_and_does_not_claim_the_call`. The deny records nothing, binds no session and claims no call. The corrected retry with the same ids on `slot-2` is event 1, `[0, 1]`, `Edit`.
- Service: `tests/admission_durability.rs` `an_empty_tool_is_denied_without_a_row_and_its_retry_is_event_one`. The deny leaves no row and no blob, and the retry is event 1, `[0, 1]`. The `check.sh` pin moves from 7 to 8.
- Journey `call-identity`: `tool_name: ""` is denied with `no tool`, and its corrected retry is row 6, `[5, 0]`.
- `scripts/freshness_proof.py` runs three real drives of one journey on isolated copies of the sources, both binaries and the receipt. `baseline` must PASS. `source` appends a comment to a source file, and `binary` appends a byte to `art-shim`. Each change is made only after every journey check has passed. Both drift scenarios must FAIL with every journey check passed, and the checkout must be unchanged.

**Formal model.** No change to TLC semantics. `Tools` never holds `NoTool`, and both handlers now enforce that. The model comment, the skill's coverage notes and the `core/mod.rs` hash in `SYNC.sha256` (after checking alignment) changed. The empty-tool boundary is covered by the real tests above, not by a TLC value. TLC was not rerun for this guard: the chief's full `check.sh` run covers the final Rust snapshot.

**Results (2026-11-09, this checkout, inside the Claude Code sandbox).** This author's results, not final.

| Check | Result |
| --- | --- |
| `cargo test -p coordinator --test identity`, `--test admission_durability`, `--test core` | 8, 8 and 8 pass. With the guard disabled, each new test failed (1 failure per file). |
| `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`; `scripts/check-file-lines.sh` (`verify.py` 876 lines, `freshness_proof.py` 179); `git diff --check`; Python `ast` parse | pass |
| `cargo test --workspace --locked --offline --no-fail-fast` | 166 tests: 141 pass. All 25 failures are the socket-bind `Operation not permitted` set (coordinator lib 3, `coordinator/tests/adapter.rs` 1, `shim/tests/cli.rs` 19, `shim/tests/e2e.rs` 2), checked one by one. **BLOCKED, not run with binding allowed.** |
| `verify.py build` | OK. The clean removed only the five workspace packages, and 69 third-party library artifacts stayed. Then the workspace tests and clippy relinked both binaries in `target/debug` with the sources unchanged, and doctor reported both binaries changed. After a second `build`, doctor passes every check except the socket bind. The receipt has sources `5dfb3373...c7d2` (30 files), `art-coordinator` `d07de420...4fd0` and `art-shim` `fb46fa0f...9bf7`. Same-source builds gave different binary hashes, so the chief should run `build` again right before `drive`. |
| `freshness_proof.py` | **BLOCKED (exit 2)** on the Unix-socket bind (EPERM), as expected here. Doctor passed every other check on the isolated copy and the copied receipt. The copy was removed, and the checkout was unchanged. A sandbox-only call of `end_checks` on isolated copies: baseline all pass, source fails the source and receipt checks, binary fails `art-shim unchanged`. That call is not the proof. |
| `verify.py drive all`, the socket tests, `check.sh`, Codex recheck | **Not run** by this author. The chief runs them with binding allowed. |

## Limitations

- No live Claude Code session was run. Hook JSON comes from recorded fixtures with edited fields. The dogfooding rows in `docs/research/dogfooding.md` stay `not run`. The registration in `docs/hook-registration.md` is untested against a live session.
- No coordinator resume: bindings, calls and completions live in memory for one run, and nothing is rebuilt from a trace.
- The model covers two concurrent calls plus one pre-recorded BeforeTool. Longer histories (a third report, a double retry) are not explored. File-system flushes are an assumption, not a proof.
- Failed and interrupted completions, Stop and SessionEnd are not implemented. Their `--hook-event` values only reserve the nonblocking policy.
- The panic-to-exit-1 path is covered by a policy unit test and by reading the code; no test forces a real panic in the binary.
- Orphan blobs from failed appends are never collected.
- `verify.py drive`'s end checks compare two endpoints. A source edit or binary swap undone before the run ends is not detected. Whoever owns the run must keep sources and the target dir frozen.

## Human review

PENDING. No feedback from Derek yet.
