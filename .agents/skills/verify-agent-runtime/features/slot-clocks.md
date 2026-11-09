# Slot clocks

Each admitted call advances only its own slot's counter in that slot's clock and takes the next event id, so the trace orders events by id and shows causal position by clock. Edit and Bash calls are both recorded.

## Sub-features

- `clocks-ids` numbers events 1, 2, 3 in call order.
- `clocks-own-counter` advances only the calling slot's entry.
- `clocks-bash` records a Bash call with its raw `tool_input` in a blob.
- `clocks-blob-dedup` stores the two identical Edit inputs as one blob.

## How to get to it (user POV)

- Two Claude Code sessions, one per slot, making tool calls in a run; the trace shows their stamped events.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Coordinator running for empty run `r1` with slots `slot-1,slot-2`.

- **Three calls.** Run `verify.py drive slot-clocks`. Edit fixture as `slot-1`, Bash fixture as `slot-2`, Edit fixture as `slot-1`. Each exits `0` with the allow decision.
- **Ids and clocks.** Read `trace.db`: `event_id` is `1,2,3`; `agent_id` is `slot-1,slot-2,slot-1`; `vclock` is `[1,0]`, `[0,1]`, `[2,0]`.
- **Bash row.** Row 2 has `tool=Bash`, and the blob its `payload_hash` names is byte-identical to the Bash fixture's raw `tool_input`.
- **Deduplication.** Rows 1 and 3 have the same `payload_hash`, which differs from row 2's. `runs/r1/blobs/` holds exactly those two files and no temporary file.

## Gotchas

- No merging happens between slots yet (no reads, leases or collectives in this slice), so the other slot's entry stays 0. When merge-on-read lands, this journey's expected clocks change and must be updated deliberately.
- The Bash `intent` (the `description` field) is not stored in the trace in this slice; do not assert on it.
- `wall_time` is display-only and is not asserted, including that it is non-decreasing.
