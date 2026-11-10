---
name: formal-verify-admission
description: Model-check and test the coordinator's recording path, `CoordinatorService::step_and_record` in crates/coordinator/src/adapter.rs (used by the BeforeTool, AfterTool and Lifecycle RPCs) with `core::step`, the session, call identity and completion checks, and the trace append (payload blobs, then row). Use when a change touches adapter.rs step_and_record, before_tool or after_tool, core/mod.rs step, core/completion.rs, core/lifecycle.rs, core/clocks.rs, core/identity.rs, clock tick, trace-store append or `BlobStore::put`, or when anything changes the order lock, step, blobs, append, commit, reply, or how a failed blob write or append is handled.
---

# Formal check of the recording path: admissions and completion reports

## What is verified

Two calls may run at the same time, from one slot or two. Each is a BeforeTool (an admission), an AfterTool completion report of a success (PostToolUse) or a failure (PostToolUseFailure), or a lifecycle report (Stop or SessionEnd). Each blob write and the trace INSERT may succeed or fail. The run may start empty or with one BeforeTool already recorded. For every order of their steps, these hold:

- **(a) Durable answer.** Every Allow or Ack a shim gets has its event already in the trace. Harm: a tool call is admitted, or a report is acknowledged, that the trace never records, so "what did the agent know" is wrong.
- **(b) Ordered ids.** The event ids in the trace, in commit order, are 1, 2, 3, ...: unique, increasing, none skipped. Harm: causal replay and storage order disagree.
- **(c) Failed write changes nothing.** A failed blob write or append leaves `next_event_id`, the slot's clock, the session bindings, the recorded calls and their completions as they were (`CoreMatchesTrace`). The retry reuses the same id and stamp, still pairs with its BeforeTool, and is not refused as a repeat of itself; and the first completion of a call stays its completion. Harm: a gap in the ids, a stamp that jumps, a retry refused, or a completion that names an event that does not exist.
- **(d) Increasing stamps.** Each slot's stamps strictly increase. Every recorded event ticks its slot, AfterTool included. Harm: two events of one slot that cannot be ordered.
- **(e) No collisions.** Two concurrent calls never make each other fail; a call gets an Err only when one of its own writes failed. This is what the mutex buys. The primary key on `event_id` (not the mutex) is what keeps ids unique.
- **(f) Rows name stored blobs.** Every row's `payload_hash` names a blob that is already stored and flushed, and so does an AfterTool's `input_payload_hash` (`RowsNameStoredBlobs`). `TraceWriter::append` puts the payload blob, then an AfterTool's input blob, then inserts the row. A failed blob write leaves no row, so (c) covers it like any failed append. A failed INSERT or second blob leaves orphan blobs that no row names, which is allowed. Harm: a row whose payload cannot be read back.
- **(g) Session and call identity.** A session stays on the slot of its first recorded event (`SessionStaysOnSlot`). A `(session_id, tool_use_id)` has at most one BeforeTool row, and none after a row of its own, so a late BeforeTool for an orphan completion is a repeat (`CallRecordedOnce`). Harm: a session's events split across two slot clocks, or a call recorded twice.
- **(h) Pairing.** Every row of a call names the call's tool (`CallKeepsItsTool`). An AfterTool row's `before_event_id` names its own call's BeforeTool row when one exists, by identity alone, and is none (0 in the model, NULL in SQLite) when none exists: never a guess from the latest event (`AfterPairsWithItsBefore`). Harm: a completion attached to the wrong call, so the trace says a call returned something it did not.

- **(i) Outcome kept.** Every row carries its report's outcome: a failure is never recorded as a success, nor a success as a failure (`RowKeepsItsOutcome`). A success and a failure of one call pair the same way, and whichever came first stays the call's completion (rule (c) across outcomes). Harm: the trace says a call succeeded that failed, or a later report rewrites which report completed the call.

- **(j) Lifecycle reports change no call.** A Stop or SessionEnd is one event of its slot with a tick. It keeps every binding and every recorded call, so a completion that arrives after it still pairs (rule (h)) and its session stays on its slot (rule (g)). No new invariant: the broken variants `lifecycle_forgets_calls` and `lifecycle_unbinds` fail (h) and (g). Harm: a late completion recorded as an orphan, or an ended session moving to another slot's clock.

Also checked: a Deny or a refused report records nothing (`DenyRecordsNothing`). In the model a Deny is a refusal (an unknown slot, a bad identity). A write denied for a lease is different: the real code records it as a `Deny` event and answers after the append, so it follows rule (a) like an Allow. Leases are not in the model; see "What it leaves out".

Not covered: see "What it leaves out". In particular the two-coordinator socket and run-reservation lifecycle (`bind`, `remove_stale_socket`, `SocketGuard`, the `trace.lock` flock) is **not** modeled.

## Run it

```sh
formal/admission/check.sh            # TLC model + real-code tests
formal/admission/check.sh --model    # TLC only (about 6 minutes on 12 cores)
formal/admission/check.sh --code     # cargo tests only
```

Exit codes: 0 all pass; 1 a rule, a self-test, or a test failed; 2 a tool is missing (the script prints the exact command to fetch it); 3 only the source-sync section failed (see "Sync record"). Logs go to `target/formal/admission/` (gitignored), never into `formal/`. Set `FORMAL_OUT` to put them elsewhere, `CARGO_TARGET_DIR` for a private cargo target, `JAVA` and `TLA2TOOLS` to point at a Java 11+ binary and the jar. TLC runs with `-workers auto`; the state counts do not depend on the number of workers.

The check runs, in order: TLC on the baseline model and on the lifecycle baseline (`AdmissionLifecycle.cfg`); thirteen witnesses that must be reached (three named in `check.sh`, ten in its witness loop), so a pass is not vacuous; nineteen deliberately broken variants of the model that TLC must fail; the source-sync check; `cargo test -p coordinator --lib adapter::tests::failed_` (4 tests); `cargo test -p coordinator --test admission_durability` (8 tests), `--test after_tool_durability` (6 tests), `--test failure_durability` (5 tests) and `--test lifecycle_durability` (5 tests). The cargo tests bind no socket and run in the sandbox.

The witnesses: "a call failed and the retry reused its id and stamp"; "a blob was stored, a later write failed, and no row names it"; "a known slot's call was refused for its identity or tool"; "a failed call was retried with its session and tool_use_id and recorded"; "a failed completion report was retried with the failed report's id and still paired"; "two AfterTool rows of one call name the same BeforeTool"; "an orphan AfterTool recorded after another event"; "an AfterTool paired with a BeforeTool that is not the latest one before it"; "a failure report failed to persist and its retry was recorded with the failed report's id, as a failure, still paired"; "a call has a success and a failure report, in either order, naming the same BeforeTool, with every call answered"; "a completion recorded after its session's lifecycle report still pairs with its BeforeTool"; "a lifecycle report failed to persist and its retry was recorded with the failed report's id and stamp"; and "after a lifecycle report, another session's event on the same slot takes the slot's next stamp".

Tool, pinned: TLC from tla2tools.jar v1.8.0 (TLC2 2026.11.06.222357), sha256 `e1f11b75c4b5bc4478ba0ac5cb6ae28e40a01b0016bc33e0aa97da22dfdbcd7f`, kept in `target/tla/tla2tools.jar`, fetched by the script from the GitHub release if absent and refused if the hash differs. Java 11 or later (run with 13.0.2).

TLA+ was chosen because the question is whether steps of two calls can interleave badly, and the model is small enough to check every order.

Read every TLC result with the pstack `/statechart` skill: it shows each rule TLC finds broken as its shortest path in plain words, and on a pass it draws how the calls move between states. The view and the small chart scope for it are `formal/admission/AdmissionView.mjs` and `formal/admission/AdmissionChart.cfg`. Use that skill's own instructions for its commands. When it ran on 2026-11-09 it used its own pinned TLC, because it could not read the output of the v1.8.0 jar, and it needed a sandbox that allows local port binding. The statechart was not redrawn for the AfterTool change; only the view's labels were updated. It was not redrawn for failed reports either: `AdmissionChart.cfg` keeps `Kinds = {"B", "A"}`.

## Bounds and baseline result

Two calls (`k1`, `k2`), each a BeforeTool, a successful AfterTool or a failed AfterTool (`Kinds = {"B", "A", "F"}` in the baseline; the older witness and variant configs keep `{"B", "A"}`, the two failure witnesses and two failure variants use all three); two slots (`s1`, `s2`; the calls may name the same slot), one of the calls may name a slot not in the run; two payloads (`p1`, `p2`; equal payloads share one blob; each AfterTool carries its own response or error blob); two sessions, two tool_use_ids and two tools, which the calls may share. The run starts empty, or with one BeforeTool of `(x1, u1)` on `s1` with tool `t1` already recorded (the "seed"), so two calls can show a completion with its failure and retry, a repeated completion, or a completion out of order. Each blob write and each INSERT may fail or succeed. Every call runs once, so a "retry" is the second call.

Baseline (2026-11-09, with failed AfterTool reports): 6935600 states generated, 4492736 distinct, depth 19, all rules hold; the baseline takes 44 s and the whole model section about 6 minutes of wall time on 12 cores. History: with successful AfterTool only (PR 4) 2892816 generated, 1883120 distinct, depth 19; with identity and no AfterTool it was 75424 generated, 48960 distinct, depth 15; with the blob step and no identity 4888 generated, 3156 distinct; before the blob step 806 generated, 545 distinct. The chart scope (one slot, payload, session, tool_use_id and tool, no unknown slot, the seed allowed) has 1164 distinct states, depth 19, and all rules hold.

Lifecycle baseline (2026-11-10, `Kinds = {"B", "A", "L"}`; a failed AfterTool takes the same transition as a successful one, so `F` stays in the main baseline): 3242783 states generated, 2112292 distinct, depth 19, all rules hold.

Baseline finding: **none**. The model and the real code agree that (a) to (i) hold under these bounds.

## Coverage table

Model file: `formal/admission/Admission.tla`. Each action is one indivisible step of `CoordinatorService::step_and_record`; the mutex `self.inner` is held from `lock()` until the guard drops on return. `before_tool`, `after_tool` and `lifecycle` (adapter.rs) only call it and map its answer. The wire mapping in `impl Coordinator::after_tool` (adapter.rs) turns `success` into `ToolResult::Success` and `failure` into `ToolResult::Failure` (error text and the optional `is_interrupt` as sent) before `step_and_record`; it adds no step.

| Model action | Code | What it models |
| --- | --- | --- |
| `Lock` | adapter.rs `step_and_record`, `self.inner.lock()` | waits for the other call |
| `Step` | adapter.rs `step_and_record`: `core::step(inner.state.clone(), input, now)`; core/mod.rs `step`. BeforeTool: `before_tool`, unknown slot, empty tool, then `Identities::record_before` (core/identity.rs), then `Clocks::tick` (core/clocks.rs). AfterTool, success or failure: core/completion.rs `after_tool`, unknown slot, empty tool, empty input, then `ToolResult::into_parts` picks the payload (response or raw error) and the outcome and refuses an empty one, then `Identities::record_after` (core/identity.rs: other slot, other tool, first completion kept, whatever the outcome), then the tick. Lifecycle (Stop or SessionEnd): core/lifecycle.rs `lifecycle`, unknown slot, empty fields, then `Identities::record_lifecycle` (core/identity.rs: empty session, other slot), then the tick | decides on a copy: a refusal, or the next id, the tick, the session bound, and the call recorded or completed, all in the copy; an AfterTool's `before_event_id` comes from the recorded call only; a lifecycle report binds its session if new and changes no call |
| `StartWrites` | adapter.rs `inner.trace.append(&event)` enters trace-store `TraceWriter::append` (lib.rs) | the writes begin |
| `BlobOk` | lib.rs `append`: `self.put(event.payload)` (a response, a raw error or lifecycle fields), then `self.put(event.input_payload)`; each is blobs.rs `BlobStore::put`, which writes and flushes a temporary file, renames it, flushes `blobs/` | one blob stored and flushed; equal contents share one |
| `BlobFails` | that `put` returned Err: `?` leaves `append` before the INSERT, `step_and_record` returns Err, guard drops, state not committed | no row, state unchanged; a payload blob stored before an input blob failure stays an orphan |
| `AppendOk` | the INSERT in `TraceWriter::append` returned Ok, `event_id INTEGER PRIMARY KEY`; the row names its blobs, its session, `tool_use_id`, tool, `before_event_id`, and its `outcome` with `is_interrupt` | one durable row |
| `AppendFails` | that INSERT returned Err: `?` returns, guard drops, state not committed | no row, orphan blobs, state unchanged |
| `AppendIdClash` | same INSERT refused by the primary key | only reachable without the mutex |
| `NoRecord` | adapter.rs `step_and_record`: no `Output::Record` (a Deny or a refused Ack) | nothing to append |
| `Commit` | adapter.rs `step_and_record`: `inner.state = next` | the new state replaces the old, with its clocks, bindings, calls and completions |
| `Unlock` | adapter.rs `step_and_record`: returns `Ok(answer)`, guard drops | lock freed |
| `Reply` | adapter.rs `impl Coordinator`: `Ok(Response::new(to_wire(admission)))` or `Ok(Response::new(WireAck {}))`; an Err becomes the tonic status, a refused report `Status::failed_precondition` | what the shim sees |

The wire refusal of a report with no `result` (`Status::invalid_argument`, adapter.rs) happens before `step_and_record` and is not modeled; `after_tool_durability.rs` covers it. So does the refusal of a lifecycle report naming no known event, which `lifecycle_durability.rs` covers.

Real-code tests for each rule (`crates/coordinator/tests/admission_durability.rs` unless noted):

| Rule | Test |
| --- | --- |
| (a) | `an_allow_has_its_event_in_the_trace_when_the_reply_returns` reads the trace on a second connection right after each Allow |
| (b), (c), (d) | `a_failed_append_leaves_the_state_alone_and_the_retry_reuses_id_and_stamp`; `adapter::tests::failed_append_keeps_the_old_state` (adapter.rs) checks the core state directly |
| `DenyRecordsNothing` | `an_unknown_slot_is_denied_without_a_row_or_an_id`; for reports, `reports_without_a_result_or_with_another_tool_are_refused_without_a_row` and `an_orphan_with_no_tool_claims_nothing_and_the_corrected_retry_is_recorded` (after_tool_durability.rs) |
| (b), (d), (e) | `concurrent_calls_get_consecutive_ids_and_increasing_stamps_and_never_collide`: 8 tasks, 80 calls, two slots |
| (c), (f) | `a_failed_blob_write_leaves_no_row_and_the_retry_reuses_id_and_stamp`; for AfterTool (after_tool_durability.rs): `a_failed_response_blob_records_nothing_and_the_retry_still_pairs`, and `a_failed_input_blob_after_the_response_blob_records_nothing`, where the response blob already exists and the new input blob fails, so the second put fails before the row |
| (f) | `a_failed_row_after_its_blob_leaves_a_harmless_orphan_that_the_retry_reuses`; for AfterTool `a_failed_row_leaves_orphan_blobs_and_the_retry_pairs_with_the_same_id_and_stamp`. Every test reads payloads back through `read_payload` or `read_input_payload`, so a row naming a missing blob fails it |
| (g), (c) | `a_failed_append_binds_no_session_and_uses_no_call_identity`; for AfterTool `a_failed_orphan_report_binds_no_session`. Pure-core cases are in `crates/coordinator/tests/identity.rs` |
| (c) completion | `adapter::tests::failed_after_tool_append_completes_nothing` (adapter.rs): after a failed AfterTool INSERT the committed call has no completion and the id and clock are unchanged; the retry becomes the completion and a repeat does not replace it |
| (h) | `a_failed_row_leaves_orphan_blobs_...` checks reverse-order pairing; the pure-core cases (reverse order, equal ids in two sessions, changed input, orphan never guessed, repeated completion, refusals) are in `crates/coordinator/tests/completion.rs` |
| (c), (f) failures | `crates/coordinator/tests/failure_durability.rs`: `a_failed_error_blob_records_nothing_and_the_retry_pairs_with_the_same_id`, `a_failed_failure_row_leaves_orphan_blobs_and_the_retry_pairs`, `a_failed_orphan_failure_binds_no_session`; `adapter::tests::failed_failure_report_append_completes_nothing` checks that the retried failure is the committed completion and a later success does not replace it |
| (i), (c) across outcomes | `mixed_outcomes_are_all_recorded_and_pair_with_their_own_before` (failure_durability.rs); pure core in `crates/coordinator/tests/failure.rs` (`success_after_failure_and_failure_after_success_keep_the_first_completion`, the interruption flag kept as `Some(true)`, `Some(false)` or `None`, refusals) |
| (j), (c), (f) lifecycle | `crates/coordinator/tests/lifecycle_durability.rs`: `stop_and_session_end_map_to_their_own_kinds_with_hashed_fields`, `a_failed_fields_blob_records_nothing_binds_nothing_and_the_retry_reuses_id_and_stamp`, `a_failed_lifecycle_row_leaves_an_orphan_blob_and_the_retry_reuses_id_and_stamp`, `lifecycle_reports_keep_calls_bindings_and_the_slot_clock`, `malformed_lifecycle_reports_are_refused_without_a_row`; `adapter::tests::failed_lifecycle_append_binds_nothing_and_keeps_the_calls` checks the committed state; pure core in `crates/coordinator/tests/lifecycle.rs` |
| `DenyRecordsNothing` for failures | `a_failure_without_an_error_or_with_another_tool_is_refused_without_a_row` (failure_durability.rs) |

Broken variants (model, `formal/admission/mutants/`), each failing the named rule, and the real-code change that corresponds to it:

| Variant | TLC fails on | Real-code change it stands for |
| --- | --- | --- |
| `commit_before_append` | `CoreMatchesTrace`; `IdsConsecutive` | move `inner.state = next` above the append |
| `ignore_append_error` | `AllowIsDurable` | drop a blob or append error instead of returning it |
| `no_tick` | `StampsIncrease` | `Clocks::tick` stops incrementing |
| `no_mutex` | `CallsDoNotCollide` | clone the state under the lock, then step and append without holding it |
| `record_unknown_slot` | `DenyRecordsNothing` | the core records an event for an unknown slot |
| `row_before_blob` | `RowsNameStoredBlobs` | `TraceWriter::append` inserts the row, then puts the blobs |
| `input_blob_after_row` | `RowsNameStoredBlobs` | `TraceWriter::append` puts an AfterTool's input blob after the INSERT |
| `no_binding_check` | `SessionStaysOnSlot` | the session-to-slot check is skipped |
| `no_repeat_check` | `CallRecordedOnce` | `Identities::record_before` skips the repeated-call check |
| `identity_before_append` | `CoreMatchesTrace` | the binding and the recorded call are written into the committed state before the append |
| `completion_before_append` | `CoreMatchesTrace` | an AfterTool's completion is written into the committed state before the append |
| `no_tool_check` | `CallKeepsItsTool` | `Identities::record_after` skips the matching-tool check |
| `guess_before` | `AfterPairsWithItsBefore` | an orphan AfterTool is paired with the latest event |
| `last_completion` | `CoreMatchesTrace` | a repeated AfterTool replaces the first completion |
| `failure_as_success` | `RowKeepsItsOutcome` | a failed report is recorded with outcome `success` (the wire or core mapping drops the failure) |
| `success_only_completion` | `CoreMatchesTrace` | only a successful AfterTool becomes the call's completion; a failure leaves it open, so a later success takes it |
| `lifecycle_forgets_calls` | `AfterPairsWithItsBefore` | a lifecycle report drops its session's recorded calls, so a late completion is an orphan |
| `lifecycle_unbinds` | `SessionStaysOnSlot` | a lifecycle report unbinds its session, so the session can report from another slot |

Proof that the real-code tests catch real-code breaks: on 2026-11-09 five scratch mutations were applied one at a time to the real files, the named tests run, and each file restored with a matching sha256: `last_completion` (identity.rs: `completed_event_id = Some(event_id)`), `no_tool_check`, `guess_before` (an orphan takes `event_id - 1`), `completion_before_append` (adapter.rs commits an AfterTool's state before the append) and `input_blob_after_row` (lib.rs inserts the row, then puts the input blob). Every one made at least one test fail. Evidence: `.claude/reviews/evidence/after-tool-mutation/summary.json` (gitignored). For failed reports (issue #3 PR 5, 2026-11-09) four more scratch mutations were applied and restored the same way, each restored file checked by sha256 and rewritten with a fresh mtime: `failure_as_success` (completion.rs maps a failure to `Outcome::Success`: 3 core and 4 durability tests fail), `success_only_completion` (identity.rs takes a `completes` flag and completion.rs passes it only for a success: 4 core tests and `adapter::tests::failed_failure_report_append_completes_nothing` fail), `omitted_interrupt_read_as_false` (hook.rs reads an omitted `is_interrupt` as `false`: a shim unit test fails) and `store_false_as_null` (lib.rs stores `false` as NULL: a trace-store test and 2 durability tests fail). Evidence: `.claude/reviews/evidence/failed-tools-mutation/summary.json` (gitignored). Earlier proofs for `commit_before_append`, `row_before_blob` and the identity checks are described in `docs/reviews/issue-3-payload-blobs.md` and `docs/reviews/issue-3-call-identity.md`.

## Sync record

Confirmed at git HEAD `adca380e46d5f7137d43b3192544d262952ee73b` plus the CI repair to `Clock::from_bytes`: fixed-size array chunks replace dynamic slice chunks after the same length guard. The covered files and their SHA-256 are in `formal/admission/SYNC.sha256`; the check recomputes them.

Issue #3 PR 2 (payload blobs, on top of `f7efab4`) changed the order inside `TraceWriter::append`, so the model was updated first: `BlobOk` and `BlobFails` before the INSERT, rule (f), the `row_before_blob` variant and the orphan witness. `trace-store/src/blobs.rs` joined the covered files, because `BlobOk` stands for its flushed `put`.

Issue #3 PR 3 (session and call identity, on top of `f2d21d8`) changed what `Step` decides and what `Commit` commits, so the model was updated first: `sessionOf`, `useIdOf`, `bound` and `seen`, the identity refusal in `Step`, rule (g), the identity part of `CoreMatchesTrace`, two witnesses and three broken variants. `core/identity.rs` joined the covered files.

Issue #3 PR 4 (successful AfterTool, on top of `717ab08`) added a second RPC to the recording path and a second blob to the append, so the model was updated first: `kindOf`, `toolOf`, the seed, `calls` (which replaces `seen` and holds each call's BeforeTool id, tool and first completion), the AfterTool branch of `Step`, `StartWrites` and the ordered `Writes` of `append`, rule (h), the completion part of `CoreMatchesTrace`, four witnesses and five broken variants. `before_tool` became `step_and_record`, shared by both RPCs, with the same order of lock, step, append, commit and reply. `core/completion.rs` joined the covered files. The hashes in `SYNC.sha256` were refreshed after the model, the table and the tests above were checked against the code.

Issue #3 PR 5 (failed and interrupted AfterTool, on top of `c2e2a2e`) added a failure result to the same RPC, core path and append. The order of lock, step, blobs, append, commit and reply did not change, so the model kept its actions: the kind `F` (a failed report) goes through the AfterTool branch of `Step` and the same `Writes`, its payload blob is its own raw error, and every row now carries `outcome` (rule (i)). New: two witnesses, two broken variants, `failure_durability.rs` (5 tests) and a third adapter unit test. The baseline now uses `Kinds = {"B", "A", "F"}`. The hashes of `adapter.rs` (wire mapping and a test), `core/completion.rs` (the failure branch), `core/identity.rs` (a comment) and `trace-store/src/lib.rs` (the `is_interrupt` column) were refreshed after the table above was checked against the code. The interruption flag's three states are opaque data to the model, like payload bytes; the real tests check them.

Issue #3 PR 6 (Stop and SessionEnd, on top of `d8ad298`) added a third RPC to the recording path. It goes through the same `step_and_record`, so the order of lock, step, blob, append, commit and reply did not change and the model kept its actions: the kind `L` takes a branch of `Step` that binds its session, ticks, and leaves `calls` alone, and its `Writes` are one blob (the lifecycle fields) and the row. New: `AdmissionLifecycle.cfg`, three witnesses, two broken variants, `lifecycle_durability.rs` (5 tests) and a fourth adapter unit test. `core/lifecycle.rs` joined the covered files. Stop and SessionEnd are one kind in the model, because the core treats them alike except for the event kind it records; the real tests check each kind. The coverage table names functions and statements, not line numbers, so an edit above a statement does not make the table stale.

Ladder rung 3 (2026-11-10) changed no step and no order. `Call` became a `tool` plus a `CallState` (`Open`, or `Completed` with an optional BeforeTool), which is the model's `calls` record with its unreachable combination removed. The core builds each event through `Event::before_tool`, `Event::after_tool`, `Event::stop` and `Event::session_end` (trace-store `body.rs`) with the same columns as before. The model was not rerun for this change; the real-code tests were, and the hashes were refreshed.

Ladder rung 8 (2026-11-10) added leases to `Step`: with `--leases`, a write claims its file's lease on the copy of the state, and a write the lease refuses is recorded as one `Deny` event and answered with a Deny. A Stop or SessionEnd releases its slot's leases. The order of lock, step, blobs, append, commit and reply did not change, and every input still records at most one event. The lease table and the session phases are part of the state that is committed only after the append; `crates/coordinator/tests/lease_durability.rs` (2 tests) checks that on the real trace. `core/leases.rs` joined the covered files. The TLA+ model was not extended with leases and was not rerun for this change.

Ladder rung 8b (2026-11-10) added the two knowledge channels and the re-read rule to `Step`, in `core/coherence.rs` and `core/files.rs`, which joined the covered files. A Read or a lease Deny merges a clock into the slot's own before the tick, so a stamp still strictly increases (rule (d)); the merge is on the copy of the state and is committed only after the append. No step or order changed. The TLA+ model has no merges and was not rerun.

A review repair in PR 4 made `completion::after_tool` refuse an empty tool before `Identities::record_after`. The model already assumed that (its `Tools` never hold `NoTool`), so only a model comment, the coverage notes above, the `after_tool_durability` count (now 6) and the `completion.rs` hash changed.

Repair round 2 made `core::before_tool` refuse an empty tool the same way, before `Identities::record_before`: a BeforeTool recorded under `""` claimed its call, and every AfterTool for it was then refused as another tool. Same model assumption, so only a model comment, the coverage notes, the `admission_durability` count (now 8) and the `core/mod.rs` hash changed. The empty-tool boundary is covered by real tests (`tests/identity.rs`, `tests/admission_durability.rs`), not by a separate TLC value.

When the check ends in `DRIFT` (exit 3), one of those files changed after this record. Read the diff against the coverage table above. If the order of lock, step, blobs, append, commit, reply, or any guard on them changed, update `Admission.tla` and the table first. Only after you have confirmed that the model and the table describe the code as it now is, run `shasum -a 256` on the covered files into `formal/admission/SYNC.sha256` and update this record. Never refresh the record to make the check go green.

## Telling a real bug from a modeling error

- A rule fails in the **baseline** run: read the path. Replay it against the real code with a test in `admission_durability.rs` or `after_tool_durability.rs`, one step for each action. If the real code does not break, think that the model is wrong until you show it: compare the failing action with the code line in the coverage table. If the real code breaks, it is a finding: report it with the path in the code's words; do not weaken the rule or the model.
- A **broken variant is no longer caught**, or a **witness** fails: the model no longer reaches the case the rule is about. Fix the model, not the variant.
- A **real-code test fails** but TLC passes: the code left what the model says. Treat the test as right, then check whether the change was intended.

## When the change is intended

Edit `formal/admission/Admission.tla` so each action still equals one step of the code, keep the coverage table and the tests in step, rerun the check, then refresh the sync record as above. A new Record output per step (a second event for one call) breaks an assumption below: a failed append after the first row leaves that row in the trace with the state not committed; model it before merging.

## What it leaves out

- The two-coordinator socket and run-reservation lifecycle: `bind`, `remove_stale_socket`, `SocketGuard`, `TraceWriter::create`'s flock on `trace.lock`. Uncovered here.
- Leases. The model has no lease table, no Deny event and no release, so it does not explore a write that races a release, or two slots claiming one file. The pure-core tests in `crates/coordinator/tests/leases.rs` and the two durability tests cover them without interleavings; the mutex makes each step indivisible, which is the property the model does check.
- Only two concurrent calls, plus at most one BeforeTool recorded before them; two slots, sessions, tool_use_ids, tools and payloads. Longer histories (for example a third report of one call, or a BeforeTool, a failed completion and two retries) are not explored. No clock merges: the real code merges on a Read of a released file and on a lease Deny, and the model does not. A lifecycle report is modeled as one kind for both Stop and SessionEnd, and only in the lifecycle baseline (two calls from BeforeTool, successful AfterTool and lifecycle report), so a failed AfterTool next to a lifecycle report is not explored. Release and other RPCs are not modeled; they arrive with later PRs. Failed completions are modeled as a third kind; whether a failure was an interruption (true, false or omitted) is not, and the real tests cover it.
- The tool response's and error's content is not modeled: each AfterTool has its own response or error blob, so response deduplication across calls is not explored (the trace-store tests cover it). The input the AfterTool reports is drawn from the same payloads as BeforeTool inputs, so a report whose input differs from its BeforeTool's is reached; pairing never looks at it.
- Empty `session_id`, `tool_use_id`, tool, tool input, tool response or error field, and a report with no `result`, are refused before or by the same checks as the modeled refusals and are not given their own values; unit and durability tests cover them. The model's `Tools` never contain the `NoTool` sentinel, so it assumes every recorded call has a nonempty tool. `before_tool` and `after_tool` refuse an empty tool before `record_before` and `record_after` to keep that assumption: a call recorded under `""` would have claimed its identity and made its corrected retry, or its AfterTool, an `OtherTool` refusal.
- Session bindings, calls and completions live only in the coordinator's memory for one run. A coordinator restart on the same run is refused (`RunNotEmpty`), so rebuilding them from a trace is not modeled and not supported.
- SQLite is taken as atomic and durable: one INSERT that returns Ok survives a crash, and one that returns Err wrote nothing. Power loss, a commit that fails after writing, and file corruption are not modeled.
- The file system is taken on trust. `BlobOk` means the blob survives a crash. That holds only if the OS and drive honour `fsync` (`F_FULLFSYNC` on macOS) and the rename and directory flush in `BlobStore::put`, and if the run directory was durable before `open` (see `durable_dir.rs`). None of this is model-checked. A `put` that fails part-way is modeled as storing nothing, which matches its temporary-file cleanup. A blob deleted or corrupted after it is stored is not modeled, though `read_payload` reports it as an error.
- Orphan blobs are never collected. The model allows them and does not bound how many pile up.
- `core::step` is taken to emit at most one Record and exactly one answer; the "no answer" and "wrong answer" branches of `step_and_record`, `before_tool` and `after_tool` are unreachable today and not modeled.
- Mutex poisoning (a panic while holding the lock) is not modeled; it makes every later call return Err, which is safe for these rules.
- There is no await between `lock()` and the guard drop, so tonic cannot cancel a call midway; if an await is added there, the steps are no longer indivisible.
- The shim's side (exit codes, the report timeout) is not modeled; the shim tests and the verify journeys cover it. Wall time (`now`) is display only and not modeled.
