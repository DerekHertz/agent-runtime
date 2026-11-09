# Run reservation

A run has one coordinator: a second coordinator is refused while the first holds the run directory or the socket, and a run that already has events cannot be restarted, so history is never restamped.

## Sub-features

- `reserve-other-socket` refuses a second coordinator on the same run dir with a different socket.
- `reserve-same-socket` refuses a coordinator that wants a live coordinator's socket.
- `reserve-same-both` refuses the same run dir and the same socket.
- `reserve-first-works` keeps the first coordinator serving, unmodified.
- `reserve-populated` refuses a run dir that already has events.

## How to get to it (user POV)

- Run `art-coordinator --run-id <id> --slots slot-1,slot-2 --runs-dir <dir> --socket <path>` while another coordinator for the same run or socket is alive, or for a run id that already has a trace.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Coordinator A running for run `r1` on `<scratch>/c.sock`.

- **Other socket.** Run `verify.py drive run-reservation`. B with `--run-id r1 --socket <scratch>/o.sock`: exits non-zero, stderr contains `already held`, `o.sock` does not exist.
- **Same socket.** B with `--run-id r2 --socket <scratch>/c.sock`: exits non-zero, stderr contains `in use`, `runs/r2` is not created.
- **Same both.** B with `r1` and `c.sock`: exits non-zero.
- **First unharmed.** A's socket inode is unchanged and A is still running; a shim call as `slot-1` is allowed and `trace.db` holds exactly event `1` with `vclock=[1,0]`.
- **Populated.** Stop A with SIGTERM. A new coordinator for `r1` is refused: non-zero exit, stderr contains `resuming a run is not supported`, no socket created, the trace still has one row.

## Gotchas

- Refusals are by exit code of the second process; each is run with a 15 s timeout so a refusal that hangs shows as a failed check, not a stuck run.
- The `already held` and `in use` wording is product error text; if it is reworded the journey fails and the checks must be updated with the new text deliberately.
- A stale socket file (dead coordinator) is replaced by design; only a live one is refused. That path is covered by the unit test, not here.
