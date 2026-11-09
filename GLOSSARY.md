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

**Admission**:
The coordinator's verdict on a proposed tool call: Allow, Deny, or Defer.

**Deny**:
An admission that refuses the call and tells the agent who holds the lease and why.

**Defer**:
An admission that holds the call until the lease frees up, or until a timeout turns it into a Deny.

**Release**:
The moment an agent gives up a lease. Other agents' current views of a file it changed become Invalid.

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
One of the few ways one agent's clock can absorb another's: reading a file, being denied by a lease holder, or passing a barrier. Nothing else carries knowledge between agents.
