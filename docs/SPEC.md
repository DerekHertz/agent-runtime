# Agent runtime spec: coherent parallel coding agents

As of 2026-11-08. Author: Derek Hertzell. Solo project in Rust, planned by Derek, implemented with Claude coding agents.

> For the coding agent reading this: every section is a contract. Build to the contracts, verify with the "Done when" lines under Milestones, and raise anything under Open questions as a question rather than a guess.

---

## 1. Problem and goals

Running N coding agents on one repository in parallel fails in three predictable ways, and no shipped tool addresses the three together.

1. **Agents overwrite each other.** Two agents edit the same file or the same symbol; the last write wins silently, or a merge conflict surfaces after both have moved on. Today's mitigation is one git worktree per agent, which is the distributed-systems equivalent of no cache coherence: correctness is deferred to a human at merge time.
2. **Nobody can reconstruct what an agent knew.** Agents run in separate processes with no shared clock. When agent B rewrites a function after agent A's test run failed, the logs cannot answer whether B saw that failure before deciding. That is a happens-before question, and wall-clock timestamps cannot answer it.
3. **Fan-out and fan-in are hand-rolled every time.** "Split these 20 files across agents, wait for all of them, merge the findings" is written as ad-hoc loops in every orchestrator, with no barrier semantics and no guarantee that the merge step sees every result.

The runtime guarantees:

- **Coherence:** at most one agent holds write ownership of a given file (later: symbol) at a time; every other agent's view of that file is invalidated when the owner releases it with changes.
- **Causality:** every tool call and tool result carries a vector clock; for any two events the system can say happened-before, happened-after, or concurrent, and can replay a run in a causally consistent order.
- **Collectives:** scatter, barrier, gather and all-reduce over a set of agents are first-class operations with defined completion semantics.
- **Transparency:** the first two guarantees hold with zero changes to the agent's prompt or model. The agent only sees that an edit was refused or deferred.

---

## 2. Scope and non-goals

**In scope**

- Agents are Claude Code sessions, intercepted through its PreToolUse and PostToolUse hooks. An optional own agent loop against the Claude API is a second adapter behind the same interface, not a requirement for v1.
- One repository, one machine, N agent processes. Cross-machine operation is a stretch goal; gRPC is chosen so the stretch costs nothing architecturally.
- File-level ownership in v1; symbol-level ownership (functions, structs) in v2 once tree-sitter parsing is in.
- Vector-clock tracing of every hook event, persisted and queryable.
- Scatter, barrier, gather, all-reduce.
- A web dashboard for the causal timeline and a benchmark harness.

**Out of scope**

- Replacing git. The runtime sits above the working tree; git remains the durable store and the merge tool.
- Any opinion about what the agents do. The runtime does not plan, decompose tasks, or write prompts; the orchestration layer that calls scatter/gather does.
- Sandboxing or security isolation between agents. Agents are trusted processes on one machine.
- OpenMP and GSM from the coursework. Neither maps onto this problem without contortion; the README should say so rather than fake it.

---

## 3. System overview

Four processes:

- a **coordinator** that owns the ownership table, the clock, and the collective state;
- N **agent sandboxes**, each a Claude Code session plus a thin hook shim;
- a **trace store** that persists every stamped event;
- a **viewer** that renders the causal timeline.

The repository working tree is shared state that only the coordinator arbitrates.

```
 +---------------------+   BeforeTool / AfterTool    +---------------------------+
 | Agent sandbox 1     | <---- gRPC (UDS) --------> |                           |
 | Claude Code session |                             |       Coordinator         |
 | + hook shim         |                             |                           |
 +---------------------+                             |  Ownership table (DSM)    |
 +---------------------+                             |  Vector clock merge       |      +------------------+
 | Agent sandbox 2     | <------------------------> |  Collectives: scatter,    | ---> |   Trace store    |
 | same shim, own VC   |                             |   barrier, gather,        |      | every stamped evt|
 +---------------------+                             |   allreduce               |      | happens-before q |
 +---------------------+                             |  Admission: allow/deny/   |      | SQLite, append   |
 | Agent sandbox N     | <------------------------> |   defer                   |      +---------+--------+
 | or own Rust loop    |                             |  gRPC server (tonic)      |                |
 +---------------------+                             +-------------+-------------+                v
                                                                   | gates agent writes     +------------------+
                                                                   v                        |      Viewer      |
                                                     +---------------------------+          | causal timeline  |
                                                     | Shared working tree (git) |          | ownership heatmap|
                                                     +---------------------------+          | reads trace only |
                                                                                            +------------------+
```

The coordinator never writes to the tree itself. It admits, denies or defers each edit before the agent's own tool performs it, and it stamps every event. The viewer reads only the trace store. See [ADR 0001](adr/0001-shared-tree-single-writer-leases.md).

The **hook shim** is the only agent-facing code. On every PreToolUse for Edit, Write, NotebookEdit, Read or Bash, the shim sends the intended operation to the coordinator over gRPC and blocks on the answer: proceed, deny with reason, or defer. On every PostToolUse and PostToolUseFailure it reports the result. The shim is stateless: the coordinator holds each slot's clock (see section 5). Claude Code's hook protocol allows a hook to block a tool call and return a message the model sees, so denial is surfaced to the agent as ordinary feedback.

**The shim fails closed.** Claude Code lets a tool call proceed when a hook times out or exits with an ordinary error. So if the shim cannot reach the coordinator, or fails internally, it denies the call (exit 2). A Defer must always finish inside the hook's configured timeout.

**Reports never block.** A report (PostToolUse and PostToolUseFailure now; Stop and SessionEnd later) comes after the fact, and exit 2 would only feed an error to the model or, for Stop, keep the session from stopping. So a report the shim cannot make, for any reason, exits 1 with the reason on stderr, and a recorded report exits 0 silently. The hook registration names its event (`art-shim --hook-event PostToolUse`), so the shim knows which policy applies before it reads its configuration or stdin; see `docs/hook-registration.md`. A report pairs with its call's BeforeTool by `(session_id, tool_use_id)` alone, never by its input, which a hook may have rewritten. A report for a call with no recorded BeforeTool (an orphan, for example after a PreToolUse timeout) is still recorded, with no BeforeTool, never paired with a guess. A PostToolUseFailure report (`art-shim --hook-event PostToolUseFailure`) pairs the same way and is recorded as an AfterTool with outcome `failure`: its payload is the raw JSON text of the hook's `error` string, and its interruption flag is kept as the hook sent it (`true`, `false`, or omitted). The hook's `error` must be a string and `is_interrupt`, when present, a boolean; the shim refuses any other type, `null` included, with exit 1. A call's first report, success or failure, is its completion; later reports of either outcome are recorded as events of their own.

**Slots and launching.** A launcher starts each session with environment variables naming its slot and the coordinator's socket, plus a settings file that registers the hooks. The coordinator maps each `session_id` to its slot, so a restarted session keeps its slot's clock. A session's first recorded event binds it to its slot for the rest of the run; a call that names the session under a different slot is refused and recorded nowhere. Within a session, Claude Code's `tool_use_id` names one tool call, so the coordinator refuses a second BeforeTool for the same `(session_id, tool_use_id)`. Without those variables the shim does nothing, so it never affects ordinary Claude Code sessions. The number of slots is fixed when a run starts.

The **own-loop adapter** implements the same two calls (`BeforeTool`, `AfterTool`) from inside a Rust agent loop that calls the Claude API directly. Both adapters produce identical trace events, so the coordinator and viewer never know which one is running. The city simulator is built using this runtime, which is the dogfood path described under Milestones.

---

## 4. Workspace coherence protocol

The working tree is treated as distributed shared memory with files as the unit of coherence; the protocol is single-writer, multiple-reader with invalidation on release, which is the simplest protocol that eliminates silent overwrites.

**States.** Every file is in one of three states per agent, tracked by the coordinator:

| State   | Meaning                                                   | Agent may                                        |
| ------- | --------------------------------------------------------- | ------------------------------------------------ |
| Invalid | Agent has not read the file since it last changed         | Read (triggers a fresh read and moves to Shared) |
| Shared  | Agent has a current view; zero or more agents share it    | Read; request Owned                              |
| Owned   | Agent holds the exclusive write lease                     | Read, write; must release                        |

The coordinator enforces Invalid itself. The model's context can't be cleared, and Claude Code doesn't refuse an edit to a file that changed since it was read. So while a file is Invalid for an agent, that agent's request to take the lease is denied with "re-read first." Only the `Read` tool moves a file from Invalid to Shared; seeing it through Bash `cat` or `grep` doesn't count. A file the agent has never read starts Invalid. Creating a brand-new file needs no prior read.

**Acquire.** On PreToolUse for any write tool, the shim sends `Acquire(path, agent, clock)`. The coordinator grants if no other agent holds Owned, otherwise returns one of:

- `Deny(owner, reason)`: the agent is told who owns the file and what they are doing (the owner's declared intent string, if any). Claude Code surfaces this as the tool's error, and the model chooses another task or waits.
- `Defer(ticket)`: the shim blocks the tool call until the lease is released, up to a configurable timeout (default 60 s). A Defer that times out becomes a Deny.

Until M3, Deny or Defer is a single setting for the whole run, with Deny as the default. M3 adds choosing it per phase. A Deny reason names the slot holding the lease, that agent's stated intent, and how long it has held the lease.

**Release.** A lease is released on any of six triggers:
1. the owning agent's Stop (it finished its turn);
2. its SessionEnd;
3. the lease's time limit running out (default 10 min, renewed on each write);
4. a barrier (section 6);
5. an explicit `art release`, kept for testing and never prompted, to keep the transparency goal;
6. a Bash command on the run's configured release list (e.g. `cargo test`, `cargo build`) exiting 0.

A failing release command keeps the lease. Release with changes invalidates every other agent's Shared copy of that file. The next read by any of them is served fresh and stamped with the writer's clock, which is what makes the causal trace meaningful.

**Reads of a leased file.** When an agent calls `Read` on a file another agent holds the lease on, the shim rewrites the call with `updatedInput` to point at the coordinator's snapshot of the file's last released version. The reader never sees half-done work and never waits. M0 confirmed that `updatedInput` redirects a Read invisibly to the model.

**Consistency model.** v1 guarantees one writer per file at a time, with invalidation at release. It does *not* provide release consistency: all agents share one tree, so builds and tests by other agents see an owner's in-progress edits as soon as they are on disk. The `Read` tool does not, because it is served the last released version. That cost is measured in the trace, for example builds failing on another agent's half-finished file. If it turns out to be large, private trees per agent with publication at release are the next step. See [ADR 0001](adr/0001-shared-tree-single-writer-leases.md).

**Granularity escalation (v2).** When two agents repeatedly contend for the same large file, the coordinator can split the lease to symbol level using tree-sitter: ownership of `fn parse_header` rather than `src/parser.rs`. Writes outside the owned span are rejected. Symbol-level leases coexist with file-level ones under a simple rule: a file lease excludes all symbol leases in that file and vice versa.

**Baseline for comparison.** The git-worktree approach is modeled as the no-coherence case: every agent is always Owned on everything, conflicts are discovered at merge. The benchmark in section 8 measures both.

---

## 5. Causal tracing

Every hook event carries a vector clock, so the trace store can answer "did agent B know about X when it did Y" exactly, not approximately.

**Clock rules.** Each slot has a vector `VC[slot] -> u64` with one entry per slot in the run. The coordinator holds every slot's clock, because shims are stateless. It applies these rules:

1. On each event from a slot, increment that slot's own entry and stamp the event with the slot's full vector.
2. Merge another vector into a slot's clock (element-wise max) **only** through a knowledge channel:
   - **Read:** the reader absorbs the file's **release clock**. A `Read` of a file someone else holds the lease on is served the last released version (section 4), so the reader learns only what was released.
   - **Whole-tree read:** a read-only Bash command that reads the whole tree (section 7) absorbs every file's latest-write clock. It does not change any file's Invalid/Shared standing, because only the `Read` tool does that.
   - **Deny:** the denied agent absorbs the lease holder's clock from when it took the lease.
   - **Barrier:** every participant absorbs every other participant's clock.
3. Nothing else merges. The coordinator is not a clock participant, and it never sends a global merged view back to an agent.

Each file carries two clocks: the **release clock**, set at each release with changes and picked up by `Read`, and the **latest-write clock**, set on every write and picked up by builds and tests.

This is what ties coherence and causality together: reading a file that agent A changed makes A's write happen-before everything you do next, and nothing else does. See [ADR 0002](adr/0002-explicit-knowledge-channels.md).

**Event schema.** One row per event in the trace store:

| Field        | Type   | Notes                                                                                  |
| ------------ | ------ | -------------------------------------------------------------------------------------- |
| event_id     | u64    | monotonic per coordinator                                                              |
| agent_id     | string | session or loop id                                                                     |
| vclock       | bytes  | serialized vector, one u64 per agent                                                   |
| kind         | enum   | BeforeTool, AfterTool, Acquire, Release, Invalidate, Barrier, Scatter, Gather          |
| tool         | string | Edit, Write, Bash, Read, etc.                                                          |
| path         | string | file or symbol, if any                                                                 |
| payload_hash | bytes  | hash of the tool input or output; the full content lives in a blob store keyed by hash. A BeforeTool's payload is its raw tool input, a successful AfterTool's its raw tool response (any JSON value, `null` included), a failed AfterTool's the raw JSON text of the hook's `error` string (`""` for an empty message) |
| tool_use_id  | string | Claude Code's `tool_use_id` for tool events; with the session it pairs a BeforeTool with its AfterTool. Null in traces written before it existed |
| before_event_id | u64 | AfterTool only: the `event_id` of the call's BeforeTool, null when none was recorded (an orphan). A repeated report names the same BeforeTool in a row of its own; the first one is the call's completion |
| outcome      | enum   | AfterTool only: how the call ended, `success` (PostToolUse) or `failure` (PostToolUseFailure, a failed or interrupted call) |
| is_interrupt | bool   | failed AfterTool only: the hook's `is_interrupt`, null when the hook omitted it (never read as false); null for every other row |
| input_payload_hash | bytes | AfterTool only: hash of the raw tool input the report carried, which may differ from the BeforeTool's; kept so an orphan's input is not lost |
| wall_time    | i64    | for display only, never for ordering                                                   |

**Layout.** Each run has its own directory, `runs/<run-id>/`, containing `trace.db` (SQLite, append-only, written only by the coordinator) and `blobs/` (content-addressed by blake3). Queries do not run in SQL. A run is loaded into memory (`Run::load(dir)`) and queried with pure methods. A later live-tail adapter will serve the viewer.

**Queries the store must answer.**

- `happens_before(e1, e2)`: VC(e1) < VC(e2) element-wise with at least one strict.
- `concurrent(e1, e2)`: neither happens-before the other. Two concurrent writes to the same path are by definition a conflict the coherence layer should have prevented; the benchmark counts these as failures.
- `knew(agent, event, at)`: did agent's clock at `at` dominate `event`'s clock. This is the "what did it know" question.
- `causal_past(event)`: the set of all events that happen-before it; this is what the replay view renders.

**Replay.** Replay walks events in any linear extension of the happens-before partial order and re-applies file writes from the blob store into a scratch tree. Because only causal order is enforced, the replay can run faster than real time and can be paused at any event to show every agent's view of the tree at that moment.

**Cost.** With N agents the clock is 8N bytes per event. At N = 20 and a busy run of 50,000 events that is 8 MB, which is trivial. Clocks are not pruned in v1.

---

## 6. Orchestration collectives

The orchestrator talks to groups of agents with four MPI-shaped operations instead of ad-hoc loops, and each one has defined completion semantics the trace can verify.

| Operation                           | Semantics                                                                                                                          | Completes when                                                                  |
| ----------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `scatter(items, group, prompt_fn)`  | Assign each item to one agent in the group and start it with `prompt_fn(item)`                                                     | Every agent has acknowledged its item                                           |
| `barrier(group)`                    | Each agent, when it reaches the barrier (signals via a tool call the shim intercepts), blocks                                      | Every agent in the group has arrived; all are released with a merged clock      |
| `gather(group)`                     | Collect each agent's declared result (a file path or a structured blob)                                                            | Every agent has reported or timed out                                           |
| `allreduce(group, reduce_fn)`       | Gather, apply `reduce_fn` (deduplicate findings, merge diffs, vote), broadcast the result to every agent as a new tool result       | The broadcast has been acknowledged                                             |

**Phases.** A typical run is a sequence of phases separated by barriers: scatter edits, barrier, run the test suite once, allreduce the failures, scatter fixes, barrier. The barrier is what makes "run tests once for everyone" correct: no agent is mid-edit when the suite runs, because all leases are released at the barrier by rule.

**Barrier and leases.** Arriving at a barrier releases all of the agent's leases. This is the one place the runtime imposes policy on agents, and it is what makes the phase model safe.

**Failure.** An agent that crashes or exceeds the barrier timeout is marked failed; the barrier completes without it and the orchestrator is told which items are orphaned. Orphaned items can be re-scattered. There is no attempt at exactly-once semantics in v1.

**Why MPI shape and not a workflow engine.** The collectives are small, composable, and their completion is observable in the trace. A workflow DAG would hide the synchronization points this project exists to make visible.

---

## 7. Transport and interfaces

All inter-process communication is gRPC over tonic, with two services: one the shim calls into the coordinator, one the orchestrator uses to drive collectives.

```protobuf
service Coordinator {
  // Hook path, called by every agent's shim
  rpc BeforeTool(BeforeToolRequest) returns (Admission);
  rpc AfterTool(AfterToolReport) returns (Ack);
  rpc Release(ReleaseRequest) returns (Ack);
  // Bidirectional stream so the coordinator can push invalidations and barrier releases
  rpc Attach(stream AgentEvent) returns (stream CoordinatorEvent);
}

service Orchestration {
  rpc Scatter(ScatterRequest) returns (ScatterAck);
  rpc Barrier(BarrierRequest) returns (BarrierResult);
  rpc Gather(GatherRequest) returns (GatherResult);
  rpc AllReduce(AllReduceRequest) returns (AllReduceResult);
}

// No clocks in shim-facing messages: the coordinator holds every slot's clock (section 5).
// An empty slot, session_id, tool or tool_use_id is denied and records nothing, so the call's
// (session_id, tool_use_id) stays free for a corrected retry.
message BeforeToolRequest {
  string slot = 1;
  string session_id = 2;
  string tool = 3;           // Edit, Write, NotebookEdit, Read, Bash
  string tool_input_json = 4; // raw hook tool_input; the coordinator core classifies it (paths, Bash class)
  string intent = 5;          // Bash `description` when present, else empty; agents are never prompted for it
  string tool_use_id = 6;     // hook tool_use_id; with session_id it names the call (pairs it with its AfterTool)
}

// Every field is required: an empty string or an unset `result` is refused, never read as a default.
message AfterToolReport {
  string slot = 1;
  string session_id = 2;
  string tool = 3;
  string tool_input_json = 4; // raw hook tool_input reported with the completion
  string tool_use_id = 5;
  oneof result {
    ToolSuccess success = 6;  // PostToolUse
    ToolFailure failure = 7;  // PostToolUseFailure: the call failed or was interrupted
  }
}

message ToolSuccess {
  string tool_response_json = 1; // raw hook tool_response, any JSON value including `null`
}

message ToolFailure {
  string error_json = 1;          // raw hook `error`: the JSON text of a string, `""` for an empty message
  optional bool is_interrupt = 2; // hook `is_interrupt`; unset when the hook omitted it
}

message Ack {} // a refused report is an error status with the reason

message Admission {
  enum Verdict { ALLOW = 0; DENY = 1; DEFER = 2; }
  Verdict verdict = 1;
  string reason = 2;
  string owner = 3;
  string updated_input_json = 4; // set when a Read is redirected to a released snapshot
}
```

**Why gRPC here specifically.** The shim runs as a short-lived process per hook invocation under Claude Code, so each call must be cheap to set up: a unary gRPC over a Unix domain socket is sub-millisecond. The own-loop adapter keeps one `Attach` stream open instead. Both are the same proto, so the coordinator does not distinguish them.

**Remote tool invocation.** Tools that agents call but that live outside the agent process (the test runner, a shared build cache, the city sim's headless shards later) are exposed as gRPC services with generated stubs. The shim rewrites a local tool call into a remote call transparently. This is the RMI-shaped part: a stub, a skeleton, serialization, and a registry, with the registry being the coordinator. It is also the natural seam if the runtime ever spans machines.

**Path extraction.** The shim forwards the raw tool input. All classification happens in the coordinator core, driven by run config, so the trace keeps the raw input and old runs can be re-classified offline. For Edit, Write and NotebookEdit the paths are explicit in the tool input. Bash commands fall into three classes, with the lists set per run in config:

- **Read-only** (e.g. `cargo test/build/check/clippy`, `git status/diff/log`, `ls`, `cat`, `grep`, `rg`, `find`): no lease needed. Build and test commands count as whole-tree reads for clocks (section 5).
- **Writes to known paths** (e.g. `mv`, `cp`, `rm`, `sed -i`, `>` redirects to a file): a lease on each path.
- **Unknown:** a whole-tree lease, which conflicts with every other lease. This is deliberately strict, and should be loosened only with evidence from the trace.

`target/` and `.git/` are not covered by leases.

---

## 8. Observability and the demo

The demo is a single screen: a scrubbable timeline of a 10-agent run on a real repository, where clicking any edit shows exactly what that agent knew when it made it.

**Viewer panels**

- **Causal graph.** Agents as lanes, events as dots, happens-before edges as arrows. Concurrent events are visually level; causally ordered ones are offset. The scrubber moves a cut through the partial order, not through wall time.
- **Ownership heatmap.** Files on the y axis, time on x, color by owning agent. Contention shows up as rapid color changes on one row, which is the cue to escalate to symbol-level leases.
- **"What did it know" inspector.** Select an edit; the panel lists the other agents' writes in its causal past and, separately, the concurrent writes it could not have seen.
- **Replay.** Step the tree forward one causal event at a time with a diff view.

**Benchmark.** The same task set is run three ways on the same repository with the same agents, and the trace store computes the metrics; nothing is hand-counted.

| Metric                                  | No coherence (worktrees) | File leases | Symbol leases (v2) |
| --------------------------------------- | ------------------------ | ----------- | ------------------ |
| Concurrent writes to the same path      | measured                 | measured    | measured           |
| Merge conflicts at integration          | measured                 | measured    | measured           |
| Wasted tokens (edits later discarded)   | measured                 | measured    | measured           |
| Wall time to green test suite           | measured                 | measured    | measured           |
| Agent idle time on Defer                | n/a                      | measured    | measured           |

The honest expectation is that leases trade some idle time for far fewer conflicts and discarded edits, and that symbol-level leases claw back most of the idle time. If the numbers say otherwise, that result goes in the README too.

**Implementation.** The viewer is a small web app reading the SQLite trace store over a read-only gRPC-web endpoint, so it can be pointed at a finished run or a live one. It is deliberately not Bevy; the city sim's viewer is.

---

## 9. Milestones

An M0 spike followed by six milestones, each ending in something a coding agent can verify by running it; from M3 onward the runtime builds itself, and from M5 it builds the city simulator.

0. **M0: Hook probe (no Rust).** Two `claude -p --model haiku` sessions run on a toy repo, using the subscription (no `--bare`, no `ANTHROPIC_API_KEY`). A shell hook logs its stdin and can be told to sleep or deny. It measures six things:
   1. whether launcher-set environment variables reach the hook;
   2. whether a 60 s hook block degrades the session;
   3. whether a hook timeout lets the call through;
   4. whether parallel tool calls run their PreToolUse hooks at the same time;
   5. whether the cap of 8 consecutive Stop-hook continuations holds;
   6. how the model behaves after a Deny.

   *Done when:* `docs/research/m0-hook-probe.md` answers all six with logs. **Done 2026-11-08.**
1. **M1: Hook shim and trace (no coherence yet).** A Rust shim binary registered as Claude Code hooks, plus a minimal coordinator that allows every call, holds the clocks and is the only process that writes the SQLite trace. Shims keep no state. Two agents run on the toy crate (`bench/toy/`: one source file of about 3 functions plus a test, copied to a temp directory per run), prompted so that both must edit the same function. The store can answer `happens_before` for any pair.
   *Done when:* a concurrent-write pair is detected in a deliberately conflicting run.
2. **M2: File leases.** Coordinator with Acquire/Release/Deny/Defer and invalidation.
   *Done when:* the same conflicting run from M1 produces zero concurrent writes to one path, and the trace shows the Deny.
3. **M3: Collectives.** Scatter, barrier, gather, allreduce over the `Orchestration` service. First dogfood: use the runtime to scatter the implementation of M4's viewer across three agents.
   *Done when:* the barrier releases all leases and the test suite runs once for the group.
4. **M4: Viewer and benchmark.** Causal graph, ownership heatmap, inspector, replay; the three-way benchmark table filled on one real repository.
   *Done when:* the README has the numbers.
5. **M5: City sim shards via remote tool invocation.** The headless simulation shards from the city sim spec register as gRPC tools; agents building the sim can call them.
   *Done when:* a scatter across shards is visible in the same causal graph as the code edits that produced them.
6. **M6: Symbol-level leases.** Tree-sitter spans, lease splitting under contention, the fourth benchmark column.
   *Done when:* the idle-time metric drops against M2 on the same task set.

Milestones 1 through 4 are the resume deliverable on their own. M5 and M6 are where the two projects become one story.

---

## 10. Open questions and risks

- [x] **Hook latency budget.** Resolved by M0: a 60 s block is safe, and a hook timeout fails open (docs/research/m0-hook-probe.md). Original question: Claude Code runs hooks synchronously; if a Defer blocks for 60 s, does the session time out or degrade? Measured in M0. The hook timeout is set to the Defer timeout plus 30 s (section 3).
- [ ] **Bash path extraction.** The conservative "touch everything" fallback may make Bash-heavy agents hold global leases constantly. Need trace data from M2 to decide how much static analysis is worth.
- [ ] **Agent behavior on Deny.** Does the model retry the same edit, pick another task, or stall? M0: with nothing else to do it retries once, then stops and reports; it never works around the lease. Switching to other work is still untested. If it stalls, the orchestrator needs a nudge mechanism, which is scope creep worth resisting.
- [x] **Clock identity across sessions.** Resolved: clocks are keyed by a run-scoped slot that the launcher assigns through environment variables. The coordinator maps `session_id` to slot (section 3).
- [ ] **Own-loop adapter priority.** It is cleaner but delays dogfooding. Current call: hooks first, own loop only if hooks prove too constraining by M2.
- [ ] **Benchmark repository.** Needs enough parallelizable work for 10 agents and a fast test suite. The city sim itself is the candidate once M5 exists; before that, a mid-size open-source Rust crate.
- [ ] **Vector clock growth with agent churn.** Pruning is deferred to v2; confirm that 8N bytes per event stays trivial at the agent counts actually used.

**Biggest risk:** the coherence layer makes agents slower without making them meaningfully more correct, because conflicts in practice are rarer than assumed. The M1 trace on an uncoordinated run is the first thing to look at; if concurrent writes to one path are rare, the project pivots toward the tracing and collectives story, which stand on their own.

---

## Suggested repo layout (for the first coding session)

```
agent-runtime/
├── Cargo.toml                 # workspace
├── proto/
│   └── runtime.proto          # section 7, source of truth
├── crates/
│   ├── clock/                 # vector clock type + tests (property-based)
│   ├── trace-store/           # SQLite schema, blob store, happens_before / knew / causal_past
│   ├── coordinator/           # pure core: step(state, input, now) -> (state', outputs); tonic + SQLite + timers are adapters
│   ├── shim/                  # Claude Code hook binary: parse hook JSON, call coordinator, emit verdict
│   └── orchestrator-sdk/      # scatter/barrier/gather/allreduce client
├── viewer/                    # web app, reads trace-store over gRPC-web (M4)
├── bench/                     # task sets + harness, three-way comparison (M4)
└── docs/
    └── SPEC.md                # this file
```

First session target: M1. Start with `crates/clock` (pure, testable) and `crates/shim` reading Claude Code's hook stdin JSON, then wire the trace store.
