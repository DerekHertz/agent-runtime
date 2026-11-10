# Agent runtime

A runtime that lets several coding agents work on one repository at the same time without overwriting each other. It also records what each agent knew when it acted.

## Participants

**Agent**:
One coding session doing work in a run. An agent is identified by its slot, not by its session.
_Avoid_: worker, session (when you mean the agent)

**Slot**:
The fixed, run-scoped identity of an agent. A slot survives session restarts and is one position in every clock.
_Avoid_: agent id, session id

**Session**:
One Claude Code process attached to a slot. A slot can have several sessions over a run.

**Coordinator**:
The single authority that admits or refuses agents' tool calls and records every event. It never edits the tree itself.
_Avoid_: server, broker, master

**Shim**:
The agent-side hook program that asks the coordinator before a tool call and reports after it. It keeps no state of its own.
_Avoid_: client, hook (when you mean the shim)

**Launcher**:
What starts a run's sessions and binds each one to its slot.

**Run**:
One coordinated episode: a fixed set of slots working on one tree, from launch to finish.

## Coherence

**Tree**:
The single shared working tree that all agents in a run read and edit.
_Avoid_: workspace, repo (when you mean the working files)

**Lease**:
An agent's exclusive right to write a file. At most one agent holds a lease on a given file at a time.
_Avoid_: lock, ownership

**Whole-tree lease**:
A lease on every file at once, required by a command whose writes can't be pinned to paths. It conflicts with every other lease.

**Admission**:
The coordinator's verdict on a proposed tool call: Allow, Deny, or Defer.

**Deny**:
An admission that refuses the call and tells the agent who holds the lease and why.

**Defer**:
An admission that holds the call until the lease frees up, or until a timeout turns it into a Deny.

**Release**:
The moment an agent gives up a lease. Other agents' current views of a file it changed become Invalid.

**Released version**:
A file's content as of its last release. An agent that reads a file someone else holds the lease on sees the released version, not the work in progress.

**Invalid / Shared / Owned**:
An agent's standing toward one file. **Invalid** means it hasn't read the current version. **Shared** means it has read the current version. **Owned** means it holds the lease. An agent must re-read an Invalid file before it may take the lease.
_Avoid_: stale, fresh, locked

## Causality

**Event**:
One recorded happening in a run, such as a tool call, a result, an admission or a release. Every event is stamped with a clock.

**Clock**:
A vector with one counter per slot, showing everything the stamped event could have known about.
_Avoid_: timestamp (wall time is for display only)

**Knowledge channel**:
One of the few ways one agent's clock can absorb another's: reading a file, building or testing the whole tree, being denied by a lease holder, or passing a barrier. Nothing else carries knowledge between agents.

**Completion**:
The AfterTool event recording that a tool call ended, paired with the call's BeforeTool by its session and `tool_use_id`, never by its input. Its outcome is `success` (PostToolUse) or `failure` (PostToolUseFailure: the call failed or was interrupted). A call's first completion, of either outcome, is the authoritative one; a repeated report, of either outcome, is recorded as an event of its own. An **orphan** completion is one whose BeforeTool the coordinator never recorded; it keeps its input and names no BeforeTool.
_Avoid_: result (when you mean the event), response (that is the tool's output, the completion's payload)
