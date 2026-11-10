# Issue #3 plan: every hook event reaches the trace

Issue #3 extends tracing from PreToolUse alone (issue #2, PR #14) to every hook event the runtime receives. Its parts are: BeforeTool for the five file and shell tools, AfterTool for success and failure, Stop and SessionEnd, session identity across restarts, pairing of overlapping calls by `tool_use_id`, and tool inputs and outputs kept in content-addressed blake3 blobs (SPEC section 5, "Layout").

The work ships as six runtime PRs. Reusable live-comparison tooling is deferred to a separate follow-up. Successful and failed completion reports are separate reviews; this changes the delivery breakdown, not the issue scope. Each one leaves the system usable and tested at its own level. A PR targets the PR it actually depends on; work that depends on nothing pending targets `main`. Issue #3 is implemented only after PR 6's acceptance checks pass; all reports are reviewed before any merge.

## Scope decision (2026-11-09)

Derek requested a status update and explicit scope-creep tracking. This is workflow feedback, not approval of the pending PRs.

- **Required:** the five acceptance criteria in [issue #3](https://github.com/DerekHertz/agent-runtime/issues/3): listed hook kinds; failed AfterTool; continued slot clock across sessions; correctly paired overlapping calls (explicitly a simulated test); and blob deduplication/references. The six runtime PRs deliver these.
- **Supporting repairs already included:** invalid tool-name guards, end-of-run verifier freshness checks, and workspace-only build cleaning. Each addresses a confirmed defect in admission or the evidence used to certify it. PR #18 grew larger than intended; further work stays focused on acceptance.
- **Deferred:** the reusable live-comparison harness and live execution. The uncommitted draft is preserved for a follow-up; it is not a seventh issue #3 PR. Dogfooding records remain `not run` for comparisons that have not executed.

The earlier plan added a tooling PR after the six runtime slices. This decision removes that addition from issue #3 without dropping a runtime acceptance criterion. Remaining delivery is failed-tool verification/publication, Stop/SessionEnd and full acceptance, then the report index and joint human review. Any further addition needs a named acceptance failure and the smallest repair that resolves it. Intent classification, leases, clock merges and benchmark features belong to later work.

## Merge hold

No issue #3 PR merges, and the issue stays open, until Derek has reviewed every PR's report together. Each PR records its walkthrough in `docs/reviews/`. See [PR #15's review](../reviews/pr-15.md).

## PRs

1. **Blob store API** (PR #15). `trace_store::BlobStore` stores exact bytes under their full 32-byte blake3 hash in `runs/<run-id>/blobs/` and returns them only if they still hash to their name. `BlobStore::open` requires an existing run directory and refuses a missing one; creating the run directory and making it durable stays with the caller, which PR 2 adds. That PR introduces the API without wiring the runtime. Tests run on a real file system: round trip, refusal of a missing run directory, official blake3 vectors, deduplication, distinct content, missing and corrupt reads, concurrent identical writers with a racing reader, and removal of the temporary file after a failed write.
2. **BeforeTool payloads in blobs.** The coordinator writes the raw `tool_input` to a blob before the trace append, and `payload_hash` references it. This PR also covers reading traces written before the change (the inline `tool_input` column) and updates the runtime verifier journeys to read the blob. Base: PR 1. As built, `TraceWriter` owns the run's `BlobStore`, and its `append` stores the blob before the INSERT. The adapter and the pure core are unchanged. `read_payload` reads issue #2 rows from the inline column; this is not a resume path. Report: [`docs/reviews/issue-3-payload-blobs.md`](../reviews/issue-3-payload-blobs.md).
3. **Session identity and overlapping BeforeTool calls.** Pairing of overlapping BeforeTool calls by `(session, tool_use_id)`. The first use of a session binds it to its slot, and a later mismatch is refused without recording anything. Base: PR 2, because it extends the trace schema and reader. `BeforeToolRequest.tool_use_id` (tag 6) reaches a nullable `tool_use_id` column. `core::identity` in the pure core refuses an empty id, a rebound session or a repeated `(session_id, tool_use_id)` with a Deny and no event. Traces from before the column read with `None`. Report: [`docs/reviews/issue-3-call-identity.md`](../reviews/issue-3-call-identity.md).
4. **Successful AfterTool reports.** PostToolUse becomes a paired AfterTool with its exact input and output in blobs. Includes nonblocking report error handling. Base: PR 3. As built, the `AfterTool` RPC carries a `oneof result` with only `success`; the shim's `--hook-event` argument fixes the error policy (gate exit 2, report exit 1) before configuration, stdin or a panic; `core::completion` pairs by `(session_id, tool_use_id)` through `Identities::record_after`, which keeps the first completion and records orphans with no BeforeTool; the AfterTool row's `payload_hash` names the raw response and `input_payload_hash` the raw input, both stored before the INSERT. Branch `codex/issue-3-after-tool`. Report: [`docs/reviews/issue-3-after-tool.md`](../reviews/issue-3-after-tool.md).
5. **Failed and interrupted AfterTool reports.** PostToolUseFailure becomes a failed AfterTool without losing its error or interruption metadata. Base: PR 4. As built, the `AfterTool` oneof gains `ToolFailure failure = 7` with `error_json` (the raw JSON text of the hook's `error` string) and `optional bool is_interrupt`, so an omitted flag stays unset; the shim refuses a missing or non-string `error` and a non-boolean `is_interrupt` (exit 1) under `--hook-event PostToolUseFailure`. `core::ToolResult::Failure` pairs exactly like a success through the same `Identities::record_after`, so the first report of either outcome stays the completion. The trace stores outcome `failure` and a new nullable `is_interrupt` column (1, 0 or NULL for omitted); the payload blob is the raw error text, not a synthesized envelope, and the input blob is kept as for a success. No lifecycle events. Branch `codex/issue-3-failed-tools`. Report: [`docs/reviews/issue-3-failed-tools.md`](../reviews/issue-3-failed-tools.md).
6. **Stop and SessionEnd, then the full acceptance run.** Base: PR 5.

## Integration choices for PRs 2 to 6

These constraints govern the stack and are implemented by the corresponding units. PR 1 provides storage only.

- Every recorded event ticks its slot's clock (SPEC clock rule 1).
- A session binds to one slot, and the binding never changes.
- Calls are paired by `(session, tool_use_id)`.
- Issue #3 adds no new clock merges; merges happen only on knowledge channels (ADR 0002).
- The blob is written before its SQLite row, so no row references a missing blob. An orphan blob after a failed append is allowed.
- The trace append happens before the state commit, as in `before_tool` today.
- Each transition records at most one event. A refused call records none.
- A Stop report that fails must not block Stop. A failed report never exits 2.
- New lifecycle event kinds are additive and added by the PR that needs them.

PRs 2 to 6 change the admission path, so each must rerun `formal-verify-admission` and update the model where the order of lock, step, append, commit or reply changes.

## Dogfooding

No live runtime-vs-control run has been executed for issue #3. The pending rows in `docs/research/dogfooding.md` stay `not run` until a run is observed. The tests in these PRs are tests, not dogfooding runs.
