# Issue #3 plan: every hook event reaches the trace

Issue #3 extends tracing from PreToolUse alone (issue #2, PR #14) to every hook event the runtime receives. Its parts are: BeforeTool for the five file and shell tools, AfterTool for success and failure, Stop and SessionEnd, session identity across restarts, pairing of overlapping calls by `tool_use_id`, and tool inputs and outputs kept in content-addressed blake3 blobs (SPEC section 5, "Layout").

The work ships as five small PRs. Each one leaves the system usable and tested at its own level. A PR targets the PR it actually depends on; work that depends on nothing pending targets `main`. Issue #3 is complete only after PR 5's acceptance checks pass.

## Merge hold

No issue #3 PR merges, and the issue stays open, until Derek has reviewed every PR's report together. Each PR records its walkthrough in `docs/reviews/`. See [PR #15's review](../reviews/pr-15.md).

## PRs

1. **Blob store API** (this PR). `trace_store::BlobStore` stores exact bytes under their full 32-byte blake3 hash in `runs/<run-id>/blobs/` and returns them only if they still hash to their name. `BlobStore::open` requires an existing run directory and refuses a missing one; creating the run directory and making it durable stays with the caller, which PR 2 adds. Nothing calls the store yet. Tests run on a real file system: round trip, refusal of a missing run directory, official blake3 vectors, deduplication, distinct content, missing and corrupt reads, concurrent identical writers with a racing reader, and removal of the temporary file after a failed write.
2. **BeforeTool payloads in blobs.** The coordinator writes the raw `tool_input` to a blob before the trace append, and `payload_hash` references it. This PR also covers reading traces written before the change (the inline `tool_input` column) and updates the runtime verifier journeys to read the blob. Base: PR 1. As built, `TraceWriter` owns the run's `BlobStore`, and its `append` stores the blob before the INSERT. The adapter and the pure core are unchanged. `read_payload` reads issue #2 rows from the inline column; this is not a resume path. Report: [`docs/reviews/issue-3-payload-blobs.md`](../reviews/issue-3-payload-blobs.md).
3. **Session identity and overlapping BeforeTool calls.** Pairing of overlapping BeforeTool calls by `(session, tool_use_id)`. The first use of a session binds it to its slot, and a later mismatch is refused without recording anything. Base: PR 2, or `main` if it turns out not to touch PR 2's code.
4. **AfterTool for success and failure.** PostToolUse and PostToolUseFailure become AfterTool events whose outputs are stored as blobs. Base: PR 3.
5. **Stop and SessionEnd, then the full acceptance run.** Base: PR 4.

## Integration choices for PRs 2 to 5

These choices are settled but not implemented. PR 1 implements none of them.

- Every recorded event ticks its slot's clock (SPEC clock rule 1).
- A session binds to one slot, and the binding never changes.
- Calls are paired by `(session, tool_use_id)`.
- Issue #3 adds no new clock merges; merges happen only on knowledge channels (ADR 0002).
- The blob is written before its SQLite row, so no row references a missing blob. An orphan blob after a failed append is allowed.
- The trace append happens before the state commit, as in `before_tool` today.
- Each transition records at most one event. A refused call records none.
- A Stop report that fails must not block Stop. A failed report never exits 2.
- New lifecycle event kinds are additive and added by the PR that needs them.

PRs 2 to 5 change the admission path, so each must rerun `formal-verify-admission` and update the model where the order of lock, step, append, commit or reply changes.

## Dogfooding

No live runtime-vs-control run has been executed for issue #3. The pending rows in `docs/research/dogfooding.md` stay `not run` until a run is observed. The tests in these PRs are tests, not dogfooding runs.
