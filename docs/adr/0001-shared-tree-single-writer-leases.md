# Shared tree with single-writer leases, not release consistency

All agents in a run edit one shared working tree. The coordinator only admits or refuses writes, and agents perform them with their own tools. A lease guarantees one writer per file at a time, and release invalidates other agents' views. It does **not** hide in-progress edits: other agents can read them and compile them the moment they're on disk. We accept this in v1 because hooks can only gate tool calls, not redirect them. Real release consistency would need each agent to have a private tree that the coordinator publishes from, which is a much larger build that converges on the worktree baseline.

## Considered options

- **Private tree per agent, published at release.** This gives true release consistency. Rejected for v1 because of its size and because it blurs the comparison with the worktree baseline.
- **Shared tree, with Reads of a file someone else owns redirected to its last released snapshot.** Rejected because Bash reads and builds would still see in-progress state, so it buys partial isolation at real complexity. *(Later adopted for the `Read` tool only; see the amendment below.)*

## Consequences

Builds and tests can fail on another agent's half-finished edit. That's a cost to measure in the M2 trace, not a bug. If it turns out to be frequent, revisit with the private-tree option.

## Amendment (grill round 3): reads of a leased file see the released version

A `Read` of a file another agent holds the lease on is redirected (via the PreToolUse `updatedInput`) to the coordinator's snapshot of the file's last released version. We chose this over blocking the read because Anthropic's multi-agent write-ups favour isolation and short locks over long blocking, and lockouts showed up as a failure mode (anthropic.com/research/multiagent-systems, /building-c-compiler). Builds and tests still see the shared tree. If M0 shows `updatedInput` cannot redirect a Read, fall back to Defer, then Deny.
