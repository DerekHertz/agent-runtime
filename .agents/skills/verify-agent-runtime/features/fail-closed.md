# Fail closed

Inside a run the shim never lets a call through unchecked: if it cannot get an answer, or its configuration is half-set, it exits 2 with a reason, which is the only failure code Claude Code treats as a block.

## Sub-features

- `closed-no-coordinator` blocks when the socket path does not exist.
- `closed-stale-socket` blocks when a socket file exists but nobody listens.
- `closed-half-env` blocks when exactly one of the two variables is set, even with a live coordinator.
- `closed-coordinator-gone` blocks after the coordinator stops.

## How to get to it (user POV)

- Claude Code runs `art-shim` in a run whose coordinator crashed, was never started, or whose launcher exported only one variable.
- By hand: `AGENT_RUNTIME_SLOT=slot-1 AGENT_RUNTIME_SOCKET=/tmp/none.sock art-shim < crates/shim/tests/fixtures/pre_tool_use_edit.json; echo $?`.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Scratch dir with no coordinator at first.

- **Missing socket.** Run `verify.py drive fail-closed`. Slot `slot-1`, socket path that does not exist: exit `2`, stdout `""`, stderr starts `agent-runtime:`.
- **Stale socket.** A socket file bound then closed: exit `2`, no allow on stdout.
- **Only slot.** `AGENT_RUNTIME_SLOT` set, socket unset: exit `2`, stderr names `AGENT_RUNTIME_SOCKET`.
- **Only socket.** Socket set, slot unset: exit `2`, stderr names `AGENT_RUNTIME_SLOT`.
- **Live coordinator, half env.** With a coordinator running, only-slot still exits `2`.
- **Control and teardown.** The full-env call is allowed while the coordinator runs (event `1`); after SIGTERM the same call exits `2` with no stdout. The trace holds only event `1`.

## Gotchas

- Blocking means exit 2 with an empty stdout; a deny decision on stdout with exit 0 is a different mechanism and does not satisfy this journey.
- The shim waits up to 5 s to connect and 80 s overall; the nonexistent-path cases return immediately, so a slow result means something else is wrong.
- A timeout kill by Claude Code fails open (M0 experiment 3); this journey cannot test that, see `docs/research/m0-hook-probe.md`.
