# M1 and M2: the conflicting run, without and with leases

Run on 2026-11-10 by Claude at Derek's go-ahead. Two live `claude -p --model haiku` sessions (Claude Code 2.1.319, subscription login) worked at once on one copy of `bench/toy`, both told to change `fn mean` in `src/lib.rs`. The same binaries (built from `rung-8b-channels`, merged as `2e88429`) and the same run file ran twice: once with the coordinator allowing everything, once with `--leases`.

| | M1: leases off (`m1b`) | M2: leases on (`m2b`) |
| --- | --- | --- |
| Concurrent write pairs on one path | **2** | **0** |
| Denies | 0 | **1** |
| Events in the trace | 28 | 35 |
| Sessions | both exit 0 | both exit 0 |
| `cargo test` in the final tree | 5 passed | 5 passed |
| Wall time, slot-1 / slot-2 | 19.7 s / 9.4 s | 12.9 s / 22.8 s |
| Cost reported by the sessions | $0.017 | $0.019 |

Both milestone checks pass. M1 (SPEC section 9): a concurrent-write pair is detected in a deliberately conflicting run. M2: the same run produces zero concurrent writes to one path, and the trace shows the Deny.

## How to run it

```
cargo build --workspace
target/debug/art run bench/m1/conflict.json --run-id m1 --runs-dir ~/art-runs
target/debug/art run bench/m1/conflict.json --run-id m2 --runs-dir ~/art-runs --leases
target/debug/art query ~/art-runs/m1 concurrent-writes
target/debug/art query ~/art-runs/m2 concurrent-writes
target/debug/art query ~/art-runs/m2 denies
```

Keep the runs directory outside this repository. A run inside it makes the sessions inherit the project's `CLAUDE.md` and the parent cargo workspace; see "Earlier runs".

## What happened in M2

Writes and the events around them, with each stamp as `[slot-1, slot-2]`:

| Event | Slot | What | Stamp |
| --- | --- | --- | --- |
| 5 | slot-1 | Edit, takes the lease on `src/lib.rs` | [3, 0] |
| 11 | slot-1 | Edit | [7, 0] |
| 15 | slot-2 | Read of the file, while slot-1 still holds it | [0, 5] |
| 18 | slot-1 | Edit | [11, 0] |
| 23 | slot-1 | Stop: the lease is released, slot-2's view goes stale | [15, 0] |
| 25 | slot-2 | Edit, **denied**: "changed since you last read it … Re-read first" | [0, 9] |
| 28 | slot-2 | Read: absorbs the release clock | [15, 12] |
| 30 | slot-2 | Edit, allowed | [15, 14] |

Slot-2 read the file at event 15, before slot-1's third edit, and tried to write from that view at event 25. The coordinator refused it. Slot-2 read the file again and then made its edit, which the trace orders after all of slot-1's work. The model needed no change to its prompt: it followed the Deny's reason.

## Limits of this result

- **One run per condition.** Model timing varies. A different interleaving could give more Denies, or none.
- **The Deny seen is the re-read Deny, not the lease-held Deny.** Slot-2 did not try to write while slot-1 held the lease, so the "leased by slot-1" Deny did not occur in a live run. It is covered by the pure-core tests and the `leases` verify journey.
- **M1's pairs are an upper bound.** With leases off no knowledge channel runs, so every cross-slot pair of writes is concurrent by the clocks. In `m1b` slot-1 did read the file after slot-2's edit, so it knew of that edit; the tick-only clocks cannot show it. The two conditions therefore differ in two things: the leases, and the Read and Deny channels that come with them.
- **No write was lost in either run.** Both final trees pass their tests and contain both agents' changes. These runs show detection and ordering, not a prevented corruption.
- **Bash writes are outside the result.** Bash is not classified, so a write made through Bash takes no lease and is not counted as a write. The sessions were allowed only `cargo test` through Bash.
- **A reader may see unfinished work.** Slot-2's Read at event 15 read the tree as it was while slot-1 held the lease. Serving the last released version is not built.
- **Evidence is local.** The run directories (`~/art-runs/m1b`, `~/art-runs/m2b`: trace, blobs, session output, transcripts) are not committed.

## Earlier runs

| Run | Condition | Result | Note |
| --- | --- | --- | --- |
| `runs/m1` (inside the repo) | leases off, first prompts | 1 pair | The sessions inherited this project's `CLAUDE.md` and tried to run its review steps; `cargo test` failed inside the parent workspace. Not a clean workload. |
| `~/art-runs/m1` | leases off, first prompts | 1 pair | Clean. Slot-1 finished before slot-2 wrote. |
| `~/art-runs/m2` | leases on, first prompts | 0 pairs, 0 Denies | Slot-1 finished before slot-2 read, so nothing was contended and M2's Deny did not appear. The prompts were then lengthened so slot-1 holds the file longer, and both conditions were run again as `m1b` and `m2b`. |
