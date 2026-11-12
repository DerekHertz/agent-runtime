# Dogfooding records

This file records runs of real coding work with and without the runtime, so their outcomes can be compared. The rule is in [AGENTS.md](../../AGENTS.md#dogfooding-records). The protocol is below.

**Status (2026-11-10): one pair of runtime-enabled live runs is recorded, tracing against leases, one run each. No runtime-disabled control has been run, so there is still no comparison against working without the runtime.** M0 is the earlier live hook experiment, from before the runtime existed.

## Terms

- **Condition.**
  - `control`: the same launcher and hooks, with the runtime disabled.
  - `enabled-tracing`: the coordinator is running, and each session's shim is bound to a slot. The coordinator records tool calls without enforcing leases.
  - `enabled-leases`: as `enabled-tracing`, with lease enforcement (`--leases`).
  - `none`: the runtime did not exist or was not installed.
- **Kind.**
  - `live`: real Claude Code sessions doing the work.
  - `fixture`: scripted binaries or recorded hook JSON. A fixture checks behavior but is not dogfooding.
- **`not run`** means the run was not executed. Never put a guessed or expected value in a result column.

## Runs

| ID | Condition | Kind | Scenario | Revision | Invocation | Agents and settings | Outcome | Evidence and limitations |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| M0 | none (probe hook only) | live | Hook probe: six single-session experiments on a fresh copy of `bench/toy` | `ef47214` | `python3 -I bench/m0/run_m0.py` | One slot. `claude -p --model haiku` on Claude Code 2.1.318, subscription login, clean environment | Results of six hook experiments | [m0-hook-probe.md](m0-hook-probe.md). Raw logs in `bench/m0/runs/` are gitignored. This is baseline evidence about hook behavior only. It is not a control for any comparison workload. |
| D1-control | control | live | M1 conflicting run: two agents prompted to edit the same function in `bench/toy` (SPEC section 9) | — | — | — | not run | A runtime-disabled control has not been run. D1-tracing is the leases-off condition |
| D1-tracing | enabled-tracing | live | Same workload, `bench/m1/conflict.json` | `2e88429` | `art run bench/m1/conflict.json --run-id m1b --runs-dir ~/art-runs` | Two slots. `claude -p --model haiku` on Claude Code 2.1.319, subscription login, clean environment | 2 concurrent write pairs on `src/lib.rs`, 0 Denies; both sessions exit 0; final tree passes 5 tests | [Write-up](m1-m2-conflicting-runs.md). One run. Run directory not committed. Tick-only clocks make the pair count an upper bound |
| D1-leases | enabled-leases | live | Same workload | `2e88429` | The same with `--run-id m2b --leases` | As D1-tracing | 0 concurrent write pairs, 1 Deny ("re-read first"); both sessions exit 0; final tree passes 5 tests | [Write-up](m1-m2-conflicting-runs.md). One run. Differs from D1-tracing in leases and in the Read and Deny channels |
| F1 | none (worktree baseline) | live | Field observation, not a planned comparison: 24 Sonnet builders, one git worktree each, built 26 PRs on another repo (Systems to Streets) in one night | — | — | Up to four slots at once, usually two. Sonnet builders, one Claude Code lead, Codex review | 6 rebases with textual conflicts in 26 PRs; 4 of them also broke typecheck or tests after git resolved the text, all caught before push | [Write-up](worktree-baseline-systems-to-streets.md). One run, parsed from transcripts after the fact. The workload was not fixed in advance, so it is not a control for D1 |

## Not dogfooding

The following checks and planned benchmark are not live coding dogfooding:

- **Issue #2 real-binary journeys** (7 journeys, 57 checks, see [issue #2 review](../reviews/issue-2.md)). These are `fixture` runs. They drive `art-coordinator` and `art-shim` with recorded hook JSON, and no agent does any coding.
- **SPEC section 8 benchmark.** Not run. The harness is an M4 deliverable and does not exist yet.

## Protocol for a comparison

1. **Fix the workload.** Before either run, write down the repository and revision, the task prompt for each slot, the number of slots, the model, the Claude Code version and the hook settings. Both conditions use exactly this workload.
2. **Control.** Use the same launcher and hook registration, with the runtime off. For example, leave `AGENT_RUNTIME_SLOT` and `AGENT_RUNTIME_SOCKET` unset: outside a run the shim exits 0 without contacting a coordinator.
3. **Enabled.** Start `art-coordinator` for a new run and bind each session to its slot.
4. **Repeat and alternate.** Run the same number of repetitions for each condition, alternating between them. Start each run from a fresh copy of the tree.
5. **Record outcomes.** Record only what was observed: whether the test suite went green, wall time, merge conflicts, and edits that were later discarded. When the enabled run has a trace, also record concurrent writes to one path. Use the SPEC section 8 metrics where a tool can compute them. Mark any metric that wasn't collected `not measured`.
6. **Record evidence.** Say where the logs and traces are and whether they are committed. Name the limitations: sample size, model nondeterminism, and any difference between the two conditions besides the runtime.

Run comparisons by hand, as above. This protocol does not add any CI jobs.
