# Leases: a denied write

A coordinator started with `--leases` gives a file's lease to the first slot that writes it with Edit, Write or NotebookEdit. While that slot holds it, the same write from another slot gets a `deny` decision whose reason names the file, the holding slot, its stated intent and how long it has held the lease. The denied call is recorded as a `Deny` event: its payload is the reason and its input blob is the call's raw tool input. The holder's Stop or SessionEnd releases every lease of its slot. Without `--leases` every call is allowed.

## Sub-features

- `lease-deny` denies the second slot's write and prints the reason through the shim.
- `lease-deny-event` records the Deny with its file, its reason and its input, ticking the denied slot's clock.
- `lease-deny-channel` stamps the Deny with the holder's clock from when it took the lease.
- `lease-holder-writes` lets the holder keep writing.
- `lease-reread-first` denies a write to a file released with changes until the slot reads it again.
- `lease-read-channel` stamps the Read, and everything the reader does after it, with the releasing slot's clock at its release.
- `lease-release-on-stop` passes the lease to the next slot after the holder's Stop, and then denies the former holder.
- `lease-query` lists the Denies with `art query <run-dir> denies` and reports no concurrent write pair.

## Driving it with verify.py

Preconditions: `verify.py doctor` passes. The journey starts a coordinator for empty run `r1` with slots `slot-1,slot-2` and `--leases`.

- **Run.** `verify.py drive leases`. Every write is the Edit fixture on `/work/src/lib.rs`: slot-1 (allowed), slot-2 (denied, naming slot-1), slot-1 again (allowed), slot-1's Stop, slot-2 (denied, "changed since you last read it"), slot-2's Read of the file (allowed), slot-2 (allowed), slot-1 (denied, naming slot-2).
- **Rows.** `BeforeTool [1,0]`, `Deny [1,1]`, `BeforeTool [2,0]`, `Stop [3,0]`, `Deny [1,2]`, `BeforeTool Read [3,3]`, `BeforeTool Edit [3,4]`, `Deny [4,4]`. Each tool row has `path` `/work/src/lib.rs`.
- **Queries.** `art query … denies` prints three lines and `3 denies`. `art query … concurrent-writes` prints `0 concurrent write pairs`.

## Gotchas

- A file no slot has released is never stale, so a first write needs no Read as far as the coordinator is concerned. The spec's stricter rule (a file a slot never read starts Invalid) is not built; Claude Code's Edit requires a prior Read on its own.
- A Read of a file another slot holds is allowed and reads the tree as it is. Serving the last released version is not built, so the reader may see unfinished work that its clock does not account for.
- Bash is not classified, so a write made through Bash takes no lease and is never denied.
- Leases have no time limit and no Defer. A lease ends only at the holder's Stop or SessionEnd.
- The lease age in the reason is wall time, so the journey checks only that it starts `0m 0`.
