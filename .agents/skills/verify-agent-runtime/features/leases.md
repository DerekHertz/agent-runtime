# Leases: a denied write

A coordinator started with `--leases` gives a file's lease to the first slot that writes it with Edit, Write or NotebookEdit. While that slot holds it, the same write from another slot gets a `deny` decision whose reason names the file, the holding slot, its stated intent and how long it has held the lease. The denied call is recorded as a `Deny` event: its payload is the reason and its input blob is the call's raw tool input. The holder's Stop or SessionEnd releases every lease of its slot. Without `--leases` every call is allowed.

## Sub-features

- `lease-deny` denies the second slot's write and prints the reason through the shim.
- `lease-deny-event` records the Deny with its file, its reason and its input, ticking the denied slot's clock.
- `lease-holder-writes` lets the holder keep writing.
- `lease-release-on-stop` passes the lease to the next slot after the holder's Stop, and then denies the former holder.
- `lease-query` lists the Denies with `art query <run-dir> denies` and leaves them out of the writes.

## Driving it with verify.py

Preconditions: `verify.py doctor` passes. The journey starts a coordinator for empty run `r1` with slots `slot-1,slot-2` and `--leases`.

- **Run.** `verify.py drive leases`. Every call is the Edit fixture on `/work/src/lib.rs`: slot-1 (allowed), slot-2 (denied, naming slot-1), slot-1 again (allowed), slot-1's Stop, slot-2 (allowed), slot-1 (denied, naming slot-2).
- **Rows.** `BeforeTool, Deny, BeforeTool, Stop, BeforeTool, Deny`, the first three stamped `[1,0]`, `[0,1]`, `[2,0]`, each tool row with `path` `/work/src/lib.rs`.
- **Queries.** `art query … denies` prints two lines and `2 denies`. `art query … concurrent-writes` names neither Deny.

## Gotchas

- A Deny merges no clock yet, and a slot is not made to re-read a file changed under it. Both arrive with the knowledge channels (rung 8b), after which this journey's second half needs a Read before slot-2's write.
- Bash is not classified, so a write made through Bash takes no lease and is never denied.
- Leases have no time limit and no Defer. A lease ends only at the holder's Stop or SessionEnd.
- The lease age in the reason is wall time, so the journey checks only that it starts `0m 0`.
