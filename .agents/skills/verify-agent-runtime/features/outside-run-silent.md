# Outside a run the shim is silent

An ordinary Claude Code session has no slot variables; the shim must then do nothing and never reach a coordinator, so it cannot affect sessions that are not part of a run.

## Sub-features

- `silent-no-env` exits 0 with no stdout and no stderr when neither variable is set.
- `silent-any-input` stays silent even for garbage or empty stdin.
- `silent-empty-values` treats `AGENT_RUNTIME_SLOT=` and `AGENT_RUNTIME_SOCKET=` (empty) as unset.
- `silent-no-contact` leaves the trace of a running coordinator empty.

## How to get to it (user POV)

- Claude Code with the shim registered as a hook but launched without the run variables.
- By hand: `art-shim < crates/shim/tests/fixtures/pre_tool_use_edit.json; echo $?` with both variables unset.

## Driving it with verify.py

Preconditions:

- `verify.py doctor` passes.
- A coordinator for empty run `r1` is running (started by the journey), so contact would be visible as a row.

- **No env, valid input.** Run `verify.py drive outside-run-silent`. Shim with the Edit fixture and no `AGENT_RUNTIME_*`: exit `0`, stdout `""`, stderr `""`.
- **No env, garbage and empty input.** Stdin `this is not JSON`, then `""`: exit `0`, no output (the shim never parses outside a run).
- **Empty env values.** Both variables set to the empty string: exit `0`, no output.
- **No contact.** Read `trace.db`: zero rows.
- **Control.** A real call as `slot-1` is then allowed and becomes event `1`, so the empty trace was not a dead coordinator.

## Gotchas

- Setting only one variable is not "outside a run"; see fail-closed.
- An empty string counts as unset by design.
- The journey runs the shim with all `AGENT_RUNTIME_*` stripped from the harness's own environment; a developer shell that exports them would otherwise invalidate the proof.
