# Issue #3 PR 2 walkthrough: BeforeTool payloads in blobs

**Status: draft. HUMAN FEEDBACK PENDING.** Derek has not reviewed this PR. Nothing below is his feedback. When he comments, record it in `docs/reviews/pr-<number>.md` once the PR number exists, and keep this draft as the walkthrough presented.

- **Branch:** `codex/issue-3-payload-blobs`, PR 2 of the [issue #3 plan](../plans/issue-3.md).
- **Base:** `f7efab44bc7245809d16e57f2470a2463a0238ce` on `codex/issue-3-blob-store` (PR #15 plus its review record). The chief published the review-record commit on the parent branch after the owner's sandbox blocked its push.
- **Verified source snapshot:** after the durable-directory repair below, its Rust sources fingerprint to `bc029ac654e2ea4df23b1e178fdc8d1c9574c51b1cacfb2481cbf88f99d84fb4` (was `fa51625d…` before the repair; the `verify.py build` receipt: sha256 over `Cargo.*`, `proto/**`, `crates/**/src/**`). The attached PR identifies the committed head; the source fingerprint ties these results to its implementation without a self-referential report commit.
- **Merge hold:** this PR stays unmerged until Derek has reviewed all issue #3 reports together.

## What it does

Before this PR, a BeforeTool row kept the agent's raw `tool_input` inline in the trace. Now the exact bytes go to the run's content-addressed blob store (`runs/<run-id>/blobs/<blake3 hex>`, from PR #15). The row's `payload_hash` holds the 32-byte hash, and `tool_input` is NULL. SPEC section 5 ("Event schema", "Layout") asks for this.

## Core modules

- **`TraceWriter`** ([`crates/trace-store/src/lib.rs`](../../crates/trace-store/src/lib.rs)) now owns the run's `BlobStore`. The persistence layer publishes blobs, so `core::step` and `CoordinatorService::before_tool` are byte-identical to the base. The core still emits an `Event` whose `tool_input` is the raw input.
  - `create` makes the run directory durable, then takes `trace.lock` and refuses a non-empty trace, and only then opens `blobs/`. A refused run gets no `blobs/` directory.
  - `append` refuses an event that already carries a `payload_hash`, so the writer is the only source of hashes in the trace. It then runs `blobs.put(tool_input)`, which flushes the blob, and only after that inserts the row with the hash and a NULL `tool_input`.
- **`durable_dir::create`** ([`durable_dir.rs`](../../crates/trace-store/src/durable_dir.rs)) creates the run directory and its missing ancestors, resolves the canonical path, and flushes every directory from the run directory up to its filesystem anchor, whether or not they already existed. The anchor is the topmost directory on the run directory's device: the next one up is on another device, or there is none (`/`). The anchor is flushed, which records its child's entry. Its own entry is a mount point in the parent filesystem, which existed before the mount. This meets the PR #15 contract that the caller makes the run directory durable before `BlobStore::open`, including under concurrent starts.
- **`read_payload(run_dir, &Event)`** returns the exact bytes. It reads the blob for a new row, rehashing it so a missing or corrupt blob is an error and never wrong bytes. For an issue #2 row it reads the inline `tool_input` column, and it returns `None` for a row with neither. It refuses a hash that is not 32 bytes and a row with both forms. It creates nothing, because it goes through the new `read_blob`, not `BlobStore::open`.

## Runtime flow (one admitted call)

1. `before_tool` takes the mutex and steps the core on a copy of the state.
2. `trace.append(event)` runs `blobs.put(raw)`: temporary file, flush, atomic rename, `blobs/` flush. Equal content is stored once.
3. The same `append` runs the INSERT with `payload_hash = blake3(raw)` and `tool_input = NULL`.
4. Only after both succeed does `before_tool` commit the state and reply Allow.

Failure cases:

- **Blob write fails:** `?` returns before the INSERT. There is no row, the state is unchanged, and the shim gets an error (exit 2). The retry takes the same event id and clock stamp.
- **INSERT fails:** the blob stays as an orphan that no row names. The state is unchanged and the shim gets an error. A retry with the same input reuses the orphan file.

One row per transition and append-before-commit-before-Allow are unchanged.

## Tradeoffs and decisions

- **Blob publication sits in the persistence layer.** The alternative was a new core output (`Output::StoreBlob`) or core-side hashing. That would have changed the pure core and the adapter loop for no behavior gain. The cost is that `Event.tool_input` means "payload to store" on write and "legacy inline body" on read. Both meanings are documented on the field.
- **Legacy compatibility is read-only.** Issue #2 traces load through `read_events` plus `read_payload`'s inline fallback. A writer still refuses any non-empty run (`RunNotEmpty`), legacy or new, and never adds `blobs/` to it. Restart recovery does not exist and this PR does not pretend otherwise.
- **Orphan blobs are never collected.** They are harmless (no row names them) but take disk space. Collection would need its own design.
- **File-system assumptions** are written in the module docs and the formal skill:
  - blob durability relies on the OS and drive honouring `fsync` (`F_FULLFSYNC` on macOS);
  - no directory is taken as durable just because it exists. The chain is flushed up to the filesystem anchor, and the anchor's own entry (a mount point, or `/`) is the system's. Symlinks on the given path are resolved and the physical directories flushed; the link entries themselves are taken as given. Every directory up to the anchor must be readable, because flushing opens it. An unreadable one (for example an execute-only `0711` ancestor) fails the start with an error naming it; it is never skipped silently. On this Mac, flushing `/`, `/Users`, `/private/tmp` and `/System/Volumes/Data` succeeded, including on the sealed system volume. Linux was not exercised;
  - row durability is SQLite's default (no `PRAGMA fullfsync`). The blob is fully flushed before the INSERT starts, so a weaker row flush cannot leave a row whose blob was lost.
- **Not measured:** the latency an extra fsync and rename add to every admitted call, and the cost of flushing the whole ancestor chain once per writer start.

## Agent findings and repairs

These are findings from agent review, not Derek's feedback.

- **P2, confirmed and repaired: `durable_dir::create` took existence as durability.** Found by the provisional Codex review (`.claude/reviews/issue3-pr2-review-findings.md`). Writer A creates `/base/new-runs/r1` and pauses before flushing `/base`. Writer B sees `r1` already exists, so it flushes only `r1` and `new-runs`. B wins `trace.lock`, appends, and replies Allow before A flushes `/base`. A crash could then lose `new-runs` even though B reported success. Repair: `create` no longer looks at what existed. It flushes the full canonical chain up to the filesystem anchor (above). It takes no global lock, and concurrent starts are still allowed; `trace.lock` still picks one writer. Flush errors are returned with the failing path. The `TraceWriter::create` order (directory durable, then lock, then empty check, then `blobs/`) is unchanged. So are at-most-one-row and blob-before-row-before-state-commit. `lib.rs` changed only in two doc comments, so `SYNC.sha256` was refreshed for it and the formal check passes.
- Tests added (real file system, threads with a barrier). They check the set of directories flushed and the visible outcome; they **do not simulate power loss**:
  - `durable_dir` unit tests: nested missing directories flush up to the anchor; fully and partly pre-existing components (a racing creator that has not flushed) are still flushed; 8 concurrent initializers each succeed and each flushes the full chain; a symlinked ancestor flushes the physical directories.
  - `trace-store/tests/run_dir.rs`: 8 concurrent `TraceWriter::create` on a new nested run admit exactly one writer, and the rest get `RunInUse`.
  - Mutation: limiting the flush to the run directory and its parent made 4 of the 5 `durable_dir` tests fail. The edit was then reverted exactly.

## Formal model

[`formal/admission/Admission.tla`](../../formal/admission/Admission.tla) now splits the append into `BlobOk`/`BlobFails` followed by `AppendOk`/`AppendFails`/`AppendIdClash`. It adds rule (f) `RowsNameStoredBlobs`, payloads that calls may share, an orphan witness and a `row_before_blob` variant. No existing rule was weakened. `SYNC.sha256` was refreshed only after the model and the coverage table matched the code, and it now also covers `blobs.rs`. Flushing stays an OS assumption the model does not check. See [the skill](../../.agents/skills/formal-verify-admission/SKILL.md), "What it leaves out".

## Initial owner verification (2026-11-09, this checkout)

| Check | Result |
| --- | --- |
| `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`, `scripts/check-file-lines.sh` | pass |
| `cargo test --workspace --locked --offline --no-fail-fast`, inside the sandbox, after the durable-directory repair | 110 tests: 94 pass. The 16 that fail all panic at a Unix-socket bind with `Operation not permitted` (coordinator lib 3, `coordinator/tests/adapter.rs` 1, `shim/tests/cli.rs` 11, `shim/tests/e2e.rs` 1). **Not yet run with socket binding allowed.** |
| New behavior tests (all pass in the sandbox) | `trace-store/tests/payloads.rs` 5; `trace-store/tests/run_dir.rs` 1; `trace-store` unit tests +7 (`durable_dir` 5, forged hash refused, refused run gets no `blobs/`); `admission_durability.rs` +2 (blob failure with a real read-only `blobs/`; orphan after a real INSERT conflict) |
| `formal/admission/check.sh` (rerun after the repair and the `lib.rs` sync refresh) | RESULT: PASS. Baseline 4888 generated / 3156 distinct / depth 15; both witnesses reached; 7/7 variants caught (`row_before_blob` on `RowsNameStoredBlobs`); sync PASS; `failed_append_keeps_the_old_state` and `admission_durability` (6 tests) pass. Chart scope: 103 distinct, no error. |
| Real-code mutation | Inserting the row before putting the blob in `TraceWriter::append` failed both new durability tests (4 passed, 2 failed). The file was then restored, with a matching sha256. |
| `verify.py build` / `doctor` | Build and receipt OK (build rerun after the repair; doctor not rerun). Doctor is BLOCKED only on "a Unix socket can be bound" (EPERM in the sandbox); every other check passes. |
| `verify.py drive all` | **Not run.** It needs socket binding. Journeys updated: `admitted-tool-call` (NULL inline column, hash names the only blob, blob bytes equal the fixture's raw `tool_input`), `slot-clocks` (Bash blob bytes, two Edit calls share one blob, no temporary files), `unknown-slot` and `hook-input` (no blob stored). |

Commands still to run where binding a Unix socket is allowed, in this order (cargo tests replace the binaries, so build again right before doctor):

```sh
cargo test --workspace --locked --offline
.agents/skills/verify-agent-runtime/scripts/verify.py build
.agents/skills/verify-agent-runtime/scripts/verify.py doctor
.agents/skills/verify-agent-runtime/scripts/verify.py drive all
.agents/skills/verify-agent-runtime/scripts/verify.py cleanup
```

Hosted CI: not run (nothing pushed). Codex review: not run by this owner; the chief runs it.

## Final chief verification

All commands below ran with real dependencies and Unix-socket binding allowed after the directory-race repair. The implementation and its source fingerprint did not change during verification.

- `cargo test --workspace --locked --offline`: 110 tests passed.
- Formatting, workspace Clippy with warnings denied, the 1,000-line cap, and diff whitespace checks passed.
- `formal/admission/check.sh`: PASS, 3156 distinct states; both witnesses reached; all seven deliberately broken variants rejected. This includes the new blob-before-row invariant. Filesystem flush behavior remains an assumption, not a power-loss proof.
- `verify.py build`, `doctor`, `drive all`, and `cleanup`: all passed: seven journeys and 62 checks.
- A separate verifier archived and built exact commit `6169b5f`, then used its real shim and coordinator to produce an inline-payload trace. The new read APIs returned the exact payload while preserving the database hash and recursive directory inventory. No blob directory was created. The original legacy trace and commands are retained locally in `.claude/reviews/real-legacy-trace/`.
- Independent Codex recheck: PASS. The ancestor-durability race is repaired; no remaining actionable implementation findings. The separate CLI review also reports no actionable regressions. Its output and the numbered command logs are retained in `.claude/reviews/issue3-pr2-*`.

Hosted CI status is published on the attached PR. Human feedback remains pending; automated verification does not authorize merging.

## Not covered

- The original owner used a reconstructed legacy SQL fixture. The final chief verification below also checks a real trace produced by the `6169b5f` binaries.
- The verifier does not rehash blobs, because python's stdlib has no blake3. Hash correctness rests on the official-vector Rust tests from PR #15.
- Power loss, crash recovery, orphan collection and fsync cost.

## Dogfooding

None run. `docs/research/dogfooding.md` is unchanged, and its proposed rows stay `not run`. The tests above are tests, not dogfooding runs.

## Human feedback

PENDING. Derek has not reviewed this PR.
