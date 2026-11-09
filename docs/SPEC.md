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

**Slots and launching.** A launcher starts each session with environment variables naming its slot and the coordinator's socket, plus a settings file that registers the hooks. The coordinator maps each `session_id` to its slot, so a restarted session keeps its slot's clock. Without those variables the shim does nothing, so it never affects ordinary Claude Code sessions. The number of slots is fixed when a run starts.

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
- `Defer(ticket)`: the shim blocks the tool call until the lease is released, up to a configurable timeout (default 60 s). Used when the orchestrator has marked the agents as cooperating on a phase.

**Release.** A lease is released when the owning agent runs a PostToolUse on a test or build command that succeeds, when it emits an explicit `Release`, when its session ends, or when a lease TTL expires (default 10 min, renewed on each write). Release with changes invalidates every other agent's Shared copy of that file. The next read by any of them is served fresh and stamped with the writer's clock, which is what makes the causal trace meaningful.

**Consistency model.** v1 guarantees one writer per file at a time, with invalidation at release. It does *not* provide release consistency: all agents share one tree, so an owner's in-progress edits can be read and built by other agents as soon as they are on disk. That cost is measured in the trace, for example builds failing on another agent's half-finished file. If it turns out to be large, private trees per agent with publication at release are the next step. See [ADR 0001](adr/0001-shared-tree-single-writer-leases.md).

**Granularity escalation (v2).** When two agents repeatedly contend for the same large file, the coordinator can split the lease to symbol level using tree-sitter: ownership of `fn parse_header` rather than `src/parser.rs`. Writes outside the owned span are rejected. Symbol-level leases coexist with file-level ones under a simple rule: a file lease excludes all symbol leases in that file and vice versa.

**Baseline for comparison.** The git-worktree approach is modeled as the no-coherence case: every agent is always Owned on everything, conflicts are discovered at merge. The benchmark in section 8 measures both.

---

## 5. Causal tracing

Every hook event carries a vector clock, so the trace store can answer "did agent B know about X when it did Y" exactly, not approximately.

**Clock rules.** Each slot has a vector `VC[slot] -> u64` with one entry per slot in the run. The coordinator holds every slot's clock, because shims are stateless. It applies these rules:

1. On each event from a slot, increment that slot's own entry and stamp the event with the slot's full vector.
2. Merge another vector into a slot's clock (element-wise max) **only** through a knowledge channel:
   - **Read:** the reader absorbs the file's clock.
   - **Deny:** the denied agent absorbs the lease holder's clock from when it took the lease.
   - **Barrier:** every participant absorbs every other participant's clock.
3. Nothing else merges. The coordinator is not a clock participant, and it never sends a global merged view back to an agent.

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
| payload_hash | bytes  | hash of the tool input or output; the full content lives in a blob store keyed by hash |
| wall_time    | i64    | for display only, never for ordering                                                   |

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
  rpc AfterTool(AfterToolReport) returns (ClockUpdate);
  rpc Release(ReleaseRequest) returns (ClockUpdate);
  // Bidirectional stream so the coordinator can push invalidations and barrier releases
  rpc Attach(stream AgentEvent) returns (stream CoordinatorEvent);
}

service Orchestration {
  rpc Scatter(ScatterRequest) returns (ScatterAck);
  rpc Barrier(BarrierRequest) returns (BarrierResult);
  rpc Gather(GatherRequest) returns (GatherResult);
  rpc AllReduce(AllReduceRequest) returns (AllReduceResult);
}

message BeforeToolRequest {
  string agent_id = 1;
  bytes vclock = 2;
  string tool = 3;           // Edit, Write, Bash, ...
  repeated string paths = 4; // files the tool will touch, extracted by the shim
  string intent = 5;         // optional, the agent's one-line reason
}

message Admission {
  enum Verdict { ALLOW = 0; DENY = 1; DEFER = 2; }
  Verdict verdict = 1;
  string reason = 2;
  string owner = 3;
  bytes vclock = 4;
}
```

**Why gRPC here specifically.** The shim runs as a short-lived process per hook invocation under Claude Code, so each call must be cheap to set up: a unary gRPC over a Unix domain socket is sub-millisecond. The own-loop adapter keeps one `Attach` stream open instead. Both are the same proto, so the coordinator does not distinguish them.

**Remote tool invocation.** Tools that agents call but that live outside the agent process (the test runner, a shared build cache, the city sim's headless shards later) are exposed as gRPC services with generated stubs. The shim rewrites a local tool call into a remote call transparently. This is the RMI-shaped part: a stub, a skeleton, serialization, and a registry, with the registry being the coordinator. It is also the natural seam if the runtime ever spans machines.

**Path extraction.** For Edit and Write the paths are explicit in the tool input. For Bash the shim runs a conservative static scan of the command for file arguments and redirections; anything it cannot classify is treated as touching the whole tree, which forces the agent to hold a global lease. That is intentionally strict and should be loosened only with evidence from the trace.

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

Six milestones, each ending in something a coding agent can verify by running it; from M3 onward the runtime builds itself, and from M5 it builds the city simulator.

1. **M1: Hook shim and trace (no coherence yet).** A Rust shim binary registered as Claude Code hooks, plus a minimal coordinator that allows every call, holds the clocks and is the only process that writes the SQLite trace. Shims keep no state. Two agents run on a toy repo; the store can answer `happens_before` for any pair.
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

- [ ] **Hook latency budget.** Claude Code runs hooks synchronously; if a Defer blocks for 60 s, does the session time out or degrade? Measure in M1 before committing to Defer semantics.
- [ ] **Bash path extraction.** The conservative "touch everything" fallback may make Bash-heavy agents hold global leases constantly. Need trace data from M2 to decide how much static analysis is worth.
- [ ] **Agent behavior on Deny.** Does the model retry the same edit, pick another task, or stall? If it stalls, the orchestrator needs a nudge mechanism, which is scope creep worth resisting.
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
