# M0 hook probe: results

Run 2026-11-08 on Claude Code 2.1.318. Each experiment was one `claude -p --model haiku` session on a fresh copy of `bench/toy`, launched with a clean environment, so it ran on the logged-in subscription. `bench/m0/probe_hook.py` was registered on every hook event, and `bench/m0/run_m0.py` reproduces the whole probe. Raw logs (hook log, transcript, stdout) land in `bench/m0/runs/<experiment>/` and are gitignored. The excerpts below come from them.

| # | Question | Answer | Evidence |
|---|---|---|---|
| 1 | Do launcher-set environment variables reach the hook? | **Yes.** | Every hook invocation in every run logged `AGENT_RUNTIME_SLOT=slot-1`, which was set only on the `claude` process. |
| 2 | Does a 60 s PreToolUse block degrade the session? | **No.** | e2: the Edit hook slept 60.0 s with a 120 s timeout. The Edit then applied, the session finished normally ("done", 3 turns, 73.6 s wall). |
| 3 | Does a hook timeout let the call through? | **Yes: it fails open.** | e3: the hook slept 20 s and would then have denied, with a 5 s timeout. The hook was killed before it could log or deny, the Edit applied (`/// Adds two integers.`), PostToolUse fired, and the run took 17.3 s. |
| 4 | Do parallel tool calls run their PreToolUse hooks at the same time? | **Yes, overlapping.** | e1: three parallel Reads, each hook sleeping 3 s. Intervals were [0.0, 3.0], [0.8, 3.8] and [1.7, 4.8] s: they overlapped, with starts about 0.9 s apart. |
| 5 | Does the cap of 8 consecutive Stop-hook continuations hold? | **A cap exists. Tool use appears to reset it.** | e5: the Stop hook blocked on every call (limit 20). There were 5 continuations, then the model made tool calls, then 9 more blocks, then the session ended with the 9th block ignored: 14 blocks in total, `stop_hook_active` true after the first. This fits "8 consecutive continuations without intervening tool use". |
| 6 | What does the model do after a Deny? | **Retries once, then stops and reports. No workaround, no stall.** | e6: Edit was denied with a lease message. Haiku diagnosed the bug, tried Edit, was denied, re-ran `cargo test`, tried Edit once more, was denied, then ended its turn. It quoted the holder and intent, gave the unapplied fix, and asked whether to wait or override. It did not try to get around the lease through Bash. |
| 7 | Can `updatedInput` redirect a Read to a released snapshot? | **Yes, invisibly to the model.** | e7: a Read of `src/lib.rs` was rewritten to `.art/snapshots/lib.rs`. The model quoted the snapshot's marker line as the first line of `src/lib.rs`. PostToolUse reports the snapshot path in `tool_response`, so the coordinator can see that the redirect happened. |

## Other observations

- A failed Bash command (`cargo test` with a failing test) fires **PostToolUseFailure**, not PostToolUse. The shim must hook both.
- In `-p` mode, SessionEnd's `reason` is `other` in every run.
- `--session-id` works: the hooks saw the id the launcher chose.
- When the hook's JSON output says deny, the model sees the reason verbatim.

## What this settles in the spec

- **Fail closed (section 3).** Confirmed necessary: a slow or killed shim silently lets the write through. Set the hook timeout well above the Defer timeout, and treat any shim failure as exit 2.
- **Defer (section 4).** A 60 s block is safe. Keep the 60 s default with a 90 s hook timeout.
- **Reads of a leased file (section 4, ADR 0001 amendment).** The snapshot redirect works. No fallback is needed.
- **Parallel calls.** Two shim processes for one slot can be in flight at once, so the coordinator core must order them itself, keyed by `tool_use_id`.
- **Barrier through Stop (M3).** Holding a session at a barrier by blocking Stop is viable, but a session gets about 8 consecutive continuations without tool use. Phases that include tool use appear to reset the count. Confirm this in M3 before relying on it.
- **Agent behavior on Deny (open question).** With nothing else to do, the agent stops and reports rather than stalling. With other work available it might switch tasks, which still needs testing. That's the case for "nudge on release" in M3: the agent ended its turn, so something has to restart it.
