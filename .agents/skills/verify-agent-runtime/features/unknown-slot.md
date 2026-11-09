# Unknown slot is denied

A shim whose slot is not part of the run is refused with a deny decision the model can read, and the coordinator records nothing for it.

## Sub-features

- `unknown-deny` prints a deny decision naming the slot, exit 0.
- `unknown-no-event` adds no row to the trace.
- `unknown-no-side-effect` consumes no event id and no clock tick.

## How to get to it (user POV)

- A launcher starts a session with `AGENT_RUNTIME_SLOT=slot-9` for a run whose slots are `slot-1,slot-2`.
- By hand: `AGENT_RUNTIME_SLOT=slot-9 AGENT_RUNTIME_SOCKET=<sock> art-shim < crates/shim/tests/fixtures/pre_tool_use_edit.json`.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- Coordinator running for empty run `r1` with slots `slot-1,slot-2`.

- **Unknown slot.** Run `verify.py drive unknown-slot`. Shim as `slot-9` with the Edit fixture: exit `0`; stdout has `permissionDecision` `deny` and a `permissionDecisionReason` containing `unknown slot "slot-9"`.
- **No trace.** `trace.db` has zero rows.
- **No side effect.** A call as `slot-1` is then allowed as `event_id=1` with `vclock=[1,0]`.

## Gotchas

- Deny here is exit 0 plus a decision on stdout, unlike fail-closed (exit 2).
- Slot names are exact strings; `slot-9` is chosen because the run only has `slot-1` and `slot-2`.
