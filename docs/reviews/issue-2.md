# Issue #2 review walkthrough

This is the human review of the first runtime slice, saved from the review conversation. It covers what the slice does, where to read each part, what the checks showed, and what they don't cover.

## Revisions

- **Reviewed runtime: `77eecff`.** This is the implementation in `adca380` plus a Clippy-only fix. The review, the counts below and the passing hosted CI run (`check` and `msrv` jobs) all apply to this commit.
- **Docs: `7b44b3e`.** Changed only `AGENTS.md`. Hosted CI on it failed in `exactly_one_variable_set_fails_closed` with a test-harness `BrokenPipe`, not a product fault (see the last section).
- **Final repair: test-only.** The `run_shim` helper in [`crates/shim/tests/cli.rs`](../../crates/shim/tests/cli.rs) now accepts a broken pipe on stdin and still checks exit code and output. No product code changed. At the time of this review, hosted CI for the repair was pending. See [PR #14 checks](https://github.com/DerekHertz/agent-runtime/pull/14/checks) for the final result.

## Purpose

The slice records one admitted tool call end to end. Claude Code fires PreToolUse, the shim forwards the call, the coordinator stamps it with the slot's clock, appends it to the run's trace and answers Allow. Leases, Defer, AfterTool and the query layer come in later issues. The contract is [SPEC](../SPEC.md) sections 3, 5 and 7, and M1 in section 9.

## Walkthrough

**Shim** ([`crates/shim/src/`](../../crates/shim/src/)). This is a stateless hook program.
- With `AGENT_RUNTIME_SLOT` and `AGENT_RUNTIME_SOCKET` both unset (or empty), it is outside a run and exits 0 silently. It never contacts a coordinator in that case.
- With exactly one set, it fails closed (exit 2, `agent-runtime:` reason on stderr) before reading stdin.
- Otherwise it parses the hook JSON, calls `BeforeTool` over the Unix socket and prints the hook decision.
- Any error or panic becomes exit 2, because M0 showed that a hook failure otherwise lets the call through ([M0 results](../research/m0-hook-probe.md), question 3).

**Proto** ([`proto/runtime.proto`](../../proto/runtime.proto)). Only `BeforeTool` exists. Shim messages carry no clocks, since the coordinator holds them all.

**Pure core** ([`crates/coordinator/src/core/mod.rs`](../../crates/coordinator/src/core/mod.rs)). The core is `step(state, input, now) -> (state', outputs)`. It does no I/O, reads no clock and has no proto or SQLite types. The outputs are `Record` events, followed by one `Admit`. An unknown slot gets a Deny and nothing is recorded.

**Clock** ([`crates/clock/src/lib.rs`](../../crates/clock/src/lib.rs), [`core/clocks.rs`](../../crates/coordinator/src/core/clocks.rs)). Each slot has a vector clock. Its tick is clock rule 1 from SPEC section 5. Property tests in `crates/clock/tests/laws.rs` cover the order laws and the byte encoding.

**Adapter** ([`crates/coordinator/src/adapter.rs`](../../crates/coordinator/src/adapter.rs), `CoordinatorService::before_tool`). Under one mutex, it:
1. steps the core on a copy of the state,
2. appends every `Record` to the trace,
3. commits the new state and replies, but only after every append succeeds.

If an append fails, the old state stays and the shim gets an error, which it turns into exit 2. A retry then reuses the same event ID and clock stamp, so the trace never has a gap or a duplicate stamp.

**Trace, run and socket path.**
- [`crates/trace-store`](../../crates/trace-store/src/lib.rs) is the only writer of `runs/<id>/trace.db`. It is append-only, and each event is its own transaction.
- A writer holds `trace.lock` for the run's lifetime. It refuses a trace that already has events, because clocks start at zero and event IDs start at 1.
- The coordinator takes an exclusive `<socket>.lock` before it probes, removes or binds the socket. It refuses a socket that still accepts connections, and it removes only the socket file it bound (same device and inode).

### Independent clocks vs event IDs

Event IDs are one counter per run in append order. They give a total order of what the coordinator recorded, not of causality. Each slot's clock only ticks its own entry. No knowledge channel merges clocks yet, so events from two slots are concurrent by clock even when one has a lower event ID. Use clocks for happens-before and event IDs for trace position.

## Verification at `77eecff`

| Check | Result |
| --- | --- |
| Rust tests (`cargo test --workspace`) | 86 pass; fmt, Clippy (`-D warnings`), 1000-line cap pass |
| Real-binary journeys ([verify skill](../../.agents/skills/verify-agent-runtime/SKILL.md)) | 7 journeys, 57 checks pass |
| TLC admission model ([formal skill](../../.agents/skills/formal-verify-admission/SKILL.md), [`formal/admission/`](../../formal/admission/)) | 545 distinct states, no violation; all 6 model mutants rejected |
| Real-code mutation | Moving the state commit before the append makes the durability tests fail; restored code passes |
| Hosted CI | `check` and `msrv` jobs pass |

The six model mutants are in [`formal/admission/mutants/`](../../formal/admission/mutants/): commit before append (two variants), ignore append error, no mutex, no tick, and record an unknown slot. Run logs and evidence directories are local-only (`.claude/reviews/`, gitignored). Reproduce the results with the two skills above.

## Not covered

- **Bounded formal model.** It covers two calls, two slots and one unknown slot. Larger runs are not model-checked.
- **SQLite assumed.** The model treats each append as atomic: it either succeeds or fails, with nothing in between. It does not model crash recovery or partial writes.
- **Socket and run lifecycle are not modeled.** Lock acquisition, stale-socket removal and the concurrent-start race are covered by tests in `adapter.rs`, not by TLC.
- **Real-binary journeys are scripted fixtures.** They drive real binaries with recorded hook JSON. They are not live Claude Code sessions and are not dogfooding (see [dogfooding](../research/dogfooding.md)).

## Post-review test repair

The shim exits before reading stdin when it is outside a run or one variable is missing. In `exactly_one_variable_set_fails_closed`, the test helper's `write_all` could race that exit and get `EPIPE`, and the helper panicked. The helper now ignores only `ErrorKind::BrokenPipe` and still checks exit code, stdout and stderr. Product behavior is unchanged.
