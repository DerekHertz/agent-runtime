# agent-runtime verification map

This directory is the maintained source for verifying the user-facing behavior of agent-runtime as Claude Code touches it: the `art-shim` hook program and the `art-coordinator` it talks to. Read the index, then use the matching feature file as the recipe. Each feature id is also a `verify.py drive <id>` journey.

## Baseline preconditions

- Binaries built with `verify.py build` (`cargo clean -p` for each workspace package, then `cargo build --workspace --locked --offline`), with a build receipt (written by `verify.py build`) whose source fingerprint matches the current sources and whose binary sha256s match the binaries on disk.
- `verify.py doctor` passes. It needs permission to bind a Unix socket: run outside the Claude Code sandbox.
- Each journey starts its own coordinator in a fresh `/tmp/artv-*` dir with run id `r1` and slots `slot-1,slot-2`, so the trace starts empty.
- Never drive a coordinator this harness did not start.

## Driving conventions

- Start every recipe from the baseline state unless its preconditions say otherwise.
- Hook input is a recorded fixture from `crates/shim/tests/fixtures/` on the shim's stdin; the slot comes from `AGENT_RUNTIME_SLOT`, the socket from `AGENT_RUNTIME_SOCKET`.
- Treat commands as literal. Slot names are `slot-1` and `slot-2`; `slot-9` is deliberately not in the run.
- Read the trace through `trace.db` (read-only) after every shim call. `agent_id` is the slot column; `vclock` is little-endian u64 per slot. A row's payload is the file `blobs/<lowercase hex of payload_hash>` in the run dir; an AfterTool's reported input is `blobs/<hex of input_payload_hash>`.
- Order is `event_id`; causal order is the clock. `wall_time` is display-only and is never asserted.
- Run `verify.py cleanup` after any failed or interrupted run. Do not remove evidence.

## Proof and skip reporting

- Capture the command, exit code, stdout and stderr, and the trace rows read afterwards.
- Record the feature id and the evidence dir with every claim; `summary.json` carries per-check results and binary hashes.
- A drive is PASS only when every journey check passed and every end check in `summary.json` (`end_checks`) passed: the sources, receipt and binaries at the end are those doctor checked at the start. `status_reasons` names what failed. `scripts/freshness_proof.py` proves this guard. Endpoint checks miss a change undone before the run ends, so keep sources and binaries frozen while driving.
- A failed journey stays failed; put the cause in that feature's Gotchas.
- Report an unreachable path with the attempted command and the unmet precondition. Do not report it as verified through a different path.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph describing the user-visible behavior. It then uses exactly four H2 sections in this order: `Sub-features`, `How to get to it (user POV)`, `Driving it with verify.py`, `Gotchas`.

## Features

- [Admitted tool call is traced](./admitted-tool-call.md) covers an Edit hook call: allow decision, one stamped BeforeTool event, raw tool_input kept byte for byte in a blob named by the row's `payload_hash`.
- [Outside a run the shim is silent](./outside-run-silent.md) covers ordinary Claude Code sessions: no output, no coordinator contact.
- [Fail closed](./fail-closed.md) covers a missing coordinator, a stale socket, a half-set environment and a coordinator that goes away.
- [Run reservation](./run-reservation.md) covers refusal of a second coordinator on a held or populated run, or a held socket.
- [Unknown slot is denied](./unknown-slot.md) covers a slot outside the run: deny decision, nothing recorded.
- [Slot clocks](./slot-clocks.md) covers per-slot counters and event ids across Edit and Bash calls from two slots, and one blob for repeated input.
- [Hook input handling](./hook-input.md) covers non-PreToolUse events and malformed hook JSON.
- [Call identity](./call-identity.md) covers session-to-slot binding, refusal of a repeated or rebound call, restarted sessions, equal `tool_use_id`s in two sessions, and overlapping calls.
- [AfterTool reports](./after-tool.md) covers PostToolUse under `--hook-event PostToolUse`: pairing out of order, raw `null` responses, both inputs kept, repeats, orphans, and nonblocking exit 1 for every report failure.
- [Failed and interrupted tool calls](./failed-tool.md) covers PostToolUseFailure under `--hook-event PostToolUseFailure`: outcome `failure`, the raw error, `is_interrupt` as 1, 0 or NULL, mixed success and failure histories, orphans, a failed blob write and its retry, and nonblocking exit 1 for every report failure.
- [Stop and SessionEnd](./lifecycle.md) covers lifecycle reports under `--hook-event Stop` and `--hook-event SessionEnd`: rows on the session's slot, a late completion after SessionEnd, the binding kept, a new session continuing the clock, and nonblocking exit 1 for every report failure.
