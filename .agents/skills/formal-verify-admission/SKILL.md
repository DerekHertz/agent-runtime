---
name: formal-verify-admission
description: Model-check and test the coordinator's admission path, `CoordinatorService::before_tool` in crates/coordinator/src/adapter.rs with `core::step` and the trace append (payload blob, then row). Use when a change touches adapter.rs before_tool, core/mod.rs step, core/clocks.rs, clock tick, trace-store append or `BlobStore::put`, or when anything changes the order lock, step, blob, append, commit, reply, or how a failed blob write or append is handled.
---

# Formal check of admission durability and ordering

## What is verified

Two calls of `before_tool` may run at the same time, from one slot or two, and each call's trace append may succeed or fail. For every order of their steps, these hold:

- **(a) Durable Allow.** Every Allow a shim gets has its BeforeTool event already in the trace. Harm: a tool call is admitted that the trace never records, so "what did the agent know" is wrong.
- **(b) Ordered ids.** The event ids in the trace, in commit order, are 1, 2, 3, ...: unique, increasing, none skipped. Harm: causal replay and storage order disagree.
- **(c) Failed append changes nothing.** A failed append leaves `next_event_id` and the slot's clock as they were, so the retry reuses the same id and stamp. Harm: a gap in the ids and a stamp that jumps.
- **(d) Increasing stamps.** Each slot's stamps strictly increase. Harm: two events of one slot that cannot be ordered.
- **(e) No collisions.** Two concurrent calls never make each other fail; a call gets an Err only when its own append failed. This is what the mutex buys. The primary key on `event_id` (not the mutex) is what keeps ids unique.
- **(f) Rows name stored blobs.** Every row's `payload_hash` names a blob that is already stored and flushed (`RowsNameStoredBlobs`). `TraceWriter::append` puts the blob before it inserts the row. A failed blob write leaves no row, so (c) covers it like any failed append. A failed INSERT leaves an orphan blob that no row names, which is allowed. Harm: a row whose payload cannot be read back, so the run cannot be re-classified or replayed.

Also checked: an unknown slot is denied and records nothing (`DenyRecordsNothing`).

Not covered: see "What it leaves out". In particular the two-coordinator socket and run-reservation lifecycle (`bind`, `remove_stale_socket`, `SocketGuard`, the `trace.lock` flock) is **not** modeled; it is an uncovered area.

## Run it

```sh
formal/admission/check.sh            # TLC model + real-code tests
formal/admission/check.sh --model    # TLC only (about 10 s)
formal/admission/check.sh --code     # cargo tests only
```

Exit codes: 0 all pass; 1 a rule, a self-test, or a test failed; 2 a tool is missing (the script prints the exact command to fetch it); 3 only the source-sync section failed (see "Sync record"). Logs go to `target/formal/admission/` (gitignored), never into `formal/`. Set `FORMAL_OUT` to put them elsewhere, `CARGO_TARGET_DIR` for a private cargo target, `JAVA` and `TLA2TOOLS` to point at a Java 11+ binary and the jar.

The check runs, in order: TLC on the baseline model; a witness that must reach "a call failed and the retry reused its id and stamp" and an orphan witness that must reach "a blob was stored, its row failed, and no row names it" (so a pass is not vacuous); seven deliberately broken variants of the model that TLC must fail; the source-sync check; `cargo test -p coordinator --lib failed_append_keeps_the_old_state`; `cargo test -p coordinator --test admission_durability` (6 tests, no sockets, runs in the sandbox).

Tool, pinned: TLC from tla2tools.jar v1.8.0 (TLC2 2026.11.06.222357), sha256 `e1f11b75c4b5bc4478ba0ac5cb6ae28e40a01b0016bc33e0aa97da22dfdbcd7f`, kept in `target/tla/tla2tools.jar`, fetched by the script from the GitHub release if absent and refused if the hash differs. Java 11 or later (run with 13.0.2).

TLA+ was chosen because the question is whether steps of two calls can interleave badly, and the model is small enough to check every order.

Read every TLC result with the pstack `/statechart` skill: it shows each rule TLC finds broken as its shortest path in plain words, and on a pass it draws how the calls move between states. The view and the small chart scope for it are `formal/admission/AdmissionView.mjs` and `formal/admission/AdmissionChart.cfg`. Use that skill's own instructions for its commands; this skill does not repeat them. When it ran on 2026-11-09 it used its own pinned TLC, because it could not read the output of the v1.8.0 jar, and it needed a sandbox that allows local port binding.

## Bounds and baseline result

Two calls (`k1`, `k2`), two slots (`s1`, `s2`; the calls may name the same slot), two payloads (`p1`, `p2`; the calls may carry the same one, which then shares one blob), one of the calls may name a slot that is not in the run, and each blob write and each INSERT may fail or succeed. Every call runs `before_tool` once, so a "retry" is the second call. Baseline (2026-11-09, with the blob step): 4888 states generated, 3156 distinct, depth 15, all rules hold, a few seconds. Before the blob step it was 806 generated, 545 distinct, depth 13. The chart scope (one slot, one payload, no unknown slot) has 103 distinct states and all rules hold (77 before the blob step). The statechart itself was not redrawn for this change.

Baseline finding: **none**. The model and the real code agree that (a) to (e) hold under these bounds.

## Coverage table

Model file: `formal/admission/Admission.tla`. Each action is one indivisible step of `CoordinatorService::before_tool`; the mutex `self.inner` is held from `lock()` until the guard drops on return.

| Model action | Code | What it models |
| --- | --- | --- |
| `Lock` | adapter.rs `CoordinatorService::before_tool`, `self.inner.lock()` (lines 62-65) | waits for the other call |
| `Step` | adapter.rs `before_tool`: `core::step(inner.state.clone(), ...)` (line 66); core/mod.rs `step` (line 146); core/clocks.rs `Clocks::tick` (line 36) | decides on a copy: next id, clock tick, or Deny for an unknown slot |
| `BlobOk` | adapter.rs `before_tool`: `inner.trace.append(&event)` (lines 70-73) enters trace-store `TraceWriter::append` (line 227), which runs `self.blobs.put(tool_input)` (line 235); blobs.rs `BlobStore::put` (line 136) writes and flushes a temporary file, renames it, flushes `blobs/` | the payload's blob is stored and flushed; equal payloads share one |
| `BlobFails` | that `put` returned Err: `?` leaves `append` before the INSERT, then `before_tool` returns Err, guard drops, state not committed | no blob counted, no row, state unchanged |
| `AppendOk` | the INSERT in `TraceWriter::append` (line 238) returned Ok, `event_id INTEGER PRIMARY KEY` (line 140); the row's `payload_hash` is the blob's hash and `tool_input` is NULL | one durable row naming the stored blob |
| `AppendFails` | that INSERT returned Err: `?` returns, guard drops, state not committed | no row, an orphan blob, state unchanged |
| `AppendIdClash` | same INSERT refused by the primary key | only reachable without the mutex |
| `NoRecord` | adapter.rs `before_tool`: no `Output::Record` in the outputs (unknown slot) | nothing to append |
| `Commit` | adapter.rs `before_tool`: `inner.state = next` (line 79) | the new state replaces the old |
| `Unlock` | adapter.rs `before_tool`: returns `Ok(admission)`, guard drops (line 80) | lock freed |
| `Reply` | adapter.rs `impl Coordinator::before_tool`: `Ok(Response::new(to_wire(admission)))` (line 99), `Err` becomes the tonic status | what the shim sees |

Real-code tests for each rule (`crates/coordinator/tests/admission_durability.rs` unless noted):

| Rule | Test |
| --- | --- |
| (a) | `an_allow_has_its_event_in_the_trace_when_the_reply_returns` reads the trace on a second connection right after each Allow |
| (b), (c), (d) | `a_failed_append_leaves_the_state_alone_and_the_retry_reuses_id_and_stamp` plants a conflicting row, shows the Err, removes the row, and requires the retry to take the failed call's id and stamp; the older `adapter::tests::failed_append_keeps_the_old_state` (adapter.rs) checks the core state directly |
| `DenyRecordsNothing` | `an_unknown_slot_is_denied_without_a_row_or_an_id` |
| (b), (d), (e) | `concurrent_calls_get_consecutive_ids_and_increasing_stamps_and_never_collide`: 8 tasks, 80 calls, two slots |
| (c), (f) | `a_failed_blob_write_leaves_no_row_and_the_retry_reuses_id_and_stamp` makes the real `blobs/` read-only, so `BlobStore::put` fails; it requires an Err, no row, no new file, and a retry with the failed call's id and stamp |
| (f) | `a_failed_row_after_its_blob_leaves_a_harmless_orphan_that_the_retry_reuses` plants a conflicting row so the INSERT fails after the blob is stored; it requires the orphan blob and no row for the call, then a retry that reuses the one blob file. Every test reads payloads back through `read_payload`, so a row naming a missing blob fails it |

Broken variants (model, `formal/admission/mutants/`), each failing the named rule, and the real-code change that corresponds to it:

| Variant | TLC fails on | Real-code change it stands for |
| --- | --- | --- |
| `commit_before_append` | `CoreMatchesTrace`; `IdsConsecutive` | move `inner.state = next` above the append loop |
| `ignore_append_error` | `AllowIsDurable` | drop the append error instead of returning it |
| `no_tick` | `StampsIncrease` | `Clocks::tick` stops incrementing |
| `no_mutex` | `CallsDoNotCollide` | clone the state under the lock, then step and append without holding it |
| `record_unknown_slot` | `DenyRecordsNothing` | the core records an event for an unknown slot |
| `row_before_blob` | `RowsNameStoredBlobs` | `TraceWriter::append` inserts the row, then puts the blob |

Proof that the real-code check catches a real-code break: the scratch mutation "move `inner.state = next` before the append loop" is run by another worker in a private copy; its evidence is in `.claude/reviews/evidence/admission-mutation/`. For (f), on 2026-11-09 the scratch mutation "insert the row with the payload's hash, then put the blob" in `TraceWriter::append` made both blob tests above fail (4 passed, 2 failed). The original file was then restored, with a matching sha256. See `docs/reviews/issue-3-payload-blobs.md`. The model runs, logs, and the statechart output of this skill's own proof are in `.claude/reviews/evidence/formal-admission/` (gitignored).

## Sync record

Confirmed at git HEAD `adca380e46d5f7137d43b3192544d262952ee73b` plus the CI repair to `Clock::from_bytes`: fixed-size array chunks replace dynamic slice chunks after the same length guard. `Clock::tick` and all modeled admission actions are unchanged. The covered files and their SHA-256 are in `formal/admission/SYNC.sha256`; the check recomputes them. `adapter.rs` `455494fdbac05cfa3d47c866f9ad5969184d44d9f2b902c4451aef3f02e19501` (refreshed after the socket-path lock repair, which changed only `bind`, `Listening` and tests; `before_tool` was confirmed unchanged), `core/mod.rs` `129ebcbbb71fbea95dd8697a25e06a4f114aa6cedd31a038f02dcefee65763b2`, and `core/clocks.rs`, `clock/src/lib.rs`, `trace-store/src/lib.rs` as listed in that file. `trace-store/src/lib.rs` was refreshed to `3622922c8f7556c1b6ca47c460ff90ed2c9e0af93d085ac90101d120f0d2e5ae` on top of `6169b5f` when issue #3 added the blob store: the change is the module doc comment plus `mod blobs;` and its `pub use`. The schema and `TraceWriter::append` are byte-identical and only moved down three lines, so the model and the table above still describe the code.

Issue #3 PR 2 (payload blobs, on top of `f7efab4`) changed the order inside `TraceWriter::append`, so the model was updated first: `BlobOk` and `BlobFails` before the INSERT, rule (f), the `row_before_blob` variant and the orphan witness. Then the record was refreshed. `trace-store/src/lib.rs` is now `9055036415edb8f72a29535904a98a8cda47bc1b9c4f4cb4b229c55c7688edbf`. `trace-store/src/blobs.rs` (`cdf5c7613a514e6a901dc5b00f2f3dce5ee70cf5c88dee2b77f59817fbc2d969`) joined the covered files, because `BlobOk` stands for its flushed `put`. `adapter.rs`, `core/mod.rs`, `core/clocks.rs` and `clock/src/lib.rs` are unchanged since `f7efab4`. The durable-directory repair then changed only two doc comments in `trace-store/src/lib.rs` (module docs and `TraceWriter::create`), now `669d3aba57ced99cb66c84f29ae00e128c0bd85439b32d7bfec04e4a3d848786`; no code line and no modeled action changed.

When the check ends in `DRIFT` (exit 3), one of those files changed after this record. Read the diff against the coverage table above. If the order of lock, step, append, commit, reply, or any guard on them changed, update `Admission.tla` and the table first. Only after you have confirmed that the model and the table describe the code as it now is, run `shasum -a 256` on the five files into `formal/admission/SYNC.sha256` and update HEAD and the hashes above. Never refresh the record to make the check go green.

## Telling a real bug from a modeling error

- A rule fails in the **baseline** run: read the path. Replay it against the real code with a test in `admission_durability.rs`, one step for each action. If the real code does not break, think that the model is wrong until you show it: compare the failing action with the code line in the coverage table. If the real code breaks, it is a finding: report it with the path in the code's words; do not weaken the rule or the model.
- A **broken variant is no longer caught**, or the **witness** fails: the model no longer reaches the case the rule is about. Fix the model, not the variant.
- A **real-code test fails** but TLC passes: the code left what the model says. Treat the test as right, then check whether the change was intended.

## When the change is intended

Edit `formal/admission/Admission.tla` so each action still equals one step of the code, keep the coverage table and the tests in step, rerun the check, then refresh the sync record as above. A new Record output per step (a second event for one call) breaks an assumption below: a failed append after the first row leaves that row in the trace with the state not committed; model it before merging.

## What it leaves out

- The two-coordinator socket and run-reservation lifecycle: `bind`, `remove_stale_socket`, `SocketGuard`, `TraceWriter::create`'s flock on `trace.lock`. Uncovered here.
- Only two calls and two slots; no longer histories, no clock merges (ADR 0002: none exist yet), no AfterTool, Release, or other RPCs.
- SQLite is taken as atomic and durable: one INSERT that returns Ok survives a crash, and one that returns Err wrote nothing. Power loss, a commit that fails after writing, and file corruption are not modeled.
- The file system is taken on trust. `BlobOk` means the blob survives a crash. That holds only if the OS and drive honour `fsync` (`F_FULLFSYNC` on macOS) and the rename and directory flush in `BlobStore::put`, and if the run directory was durable before `open` (`TraceWriter::create` flushes every directory on the run directory's canonical path up to its filesystem anchor, existing or new; see `durable_dir.rs`). None of this is model-checked. A `put` that fails part-way is modeled as storing nothing, which matches its temporary-file cleanup. A blob deleted or corrupted after it is stored is not modeled, though `read_payload` reports it as an error.
- Orphan blobs are never collected. The model allows them and does not bound how many pile up.
- `core::step` is taken to emit exactly one Record for a known slot and none for an unknown slot, and always an Admit; the `ok_or_else` "no admission" branch in `before_tool` is unreachable today and not modeled.
- Mutex poisoning (a panic while holding the lock) is not modeled; it makes every later call return Err, which is safe for these rules.
- There is no await between `lock()` and the guard drop, so tonic cannot cancel a call midway; if an await is added there, the steps are no longer indivisible.
- Wall time (`now`) is display only and not modeled.
