# Registering the shim as Claude Code hooks

A launcher starts each session of a run with `AGENT_RUNTIME_SLOT` and `AGENT_RUNTIME_SOCKET` set and a settings file that registers `art-shim` (SPEC section 3). This page is the registration that settings file should contain. Without the two variables the shim does nothing, so the same settings are harmless in an ordinary session.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Edit|Write|NotebookEdit|Read|Bash",
        "hooks": [
          { "type": "command", "command": "/abs/path/to/art-shim --hook-event PreToolUse", "timeout": 90 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write|NotebookEdit|Read|Bash",
        "hooks": [
          { "type": "command", "command": "/abs/path/to/art-shim --hook-event PostToolUse", "timeout": 30 }
        ]
      }
    ]
  }
}
```

## Why each registration names its event

`--hook-event` is read from the command line, which the launcher writes, before the shim reads its environment or stdin, so it fixes how the shim fails even if everything after it goes wrong, panics included:

| Registration | On success | On any failure inside a run |
| --- | --- | --- |
| `art-shim --hook-event PreToolUse` (a gate) | a PreToolUse decision on stdout, exit 0 | exit 2: the call is blocked (fails closed) |
| `art-shim --hook-event PostToolUse` (a report) | nothing printed, exit 0 | exit 1, a nonblocking error, with the reason on stderr |
| `art-shim` with no arguments (the issue #2 registration) | as `--hook-event PreToolUse`; any other event passes through silently | exit 2 |

A report never exits 2: on PostToolUse that only feeds an error to the model, and on Stop it would keep the session from stopping. The stdin JSON must be the registered event, or the shim fails with that registration's policy. Outside a run (neither variable set, or both empty) every registration exits 0 silently and never contacts the coordinator, whatever its arguments. A command line the shim cannot parse fails closed inside a run, because it may belong to a gate, unless it starts `--hook-event <report event>`: then even stray arguments after the event fail with exit 1. The event named on the command line is trusted. A registration that names the wrong event gets that event's policy, and the shim cannot detect it. `--hook-event PostToolUseFailure`, `Stop` and `SessionEnd` are reserved for later issue #3 PRs; this build exits 1 for them, so even an early registration never blocks.

## Timeouts

Claude Code kills a hook at its `timeout` (seconds), and a killed hook lets the call through (M0 experiment 3). So each registration's timeout must exceed the shim's own bound, and the shim gives up first and reports why:

- **PreToolUse: 90 s.** SPEC sets the hook timeout to the Defer timeout (60 s by default) plus 30 s. The shim gives up after 80 s and exits 2.
- **PostToolUse: 30 s.** A report never waits on a lease, so the shim bounds the whole report (connect plus RPC) at 10 s (`REPORT_TIMEOUT` in `crates/shim/src/transport.rs`) and exits 1. A report the shim gave up on may still have been recorded if the coordinator answered late; one cut short by the hook timeout instead would leave no reason on stderr, which is why the margin is wide.
- **Every report registration: at least 20 s.** While the report bound is 10 s, no report hook (PostToolUse now, PostToolUseFailure, Stop and SessionEnd later) may be registered with a timeout or overall budget at or below it. 20 s is the minimum; 30 s is used above.
- **SessionEnd** defaults to a 1.5 s budget in Claude Code (up to 60 s, [research notes](research/claude-code-hooks.md)), which would kill the shim long before its 10 s bound. Its registration lands with the SessionEnd PR and must set a per-hook timeout of at least 20 s, so the overall SessionEnd budget also covers the bound.

## Status

This registration has been exercised only through recorded hook JSON piped to the real binaries (the verify skill's journeys and the shim's tests). It has not yet been run under a live Claude Code session; the dogfooding rows in `docs/research/dogfooding.md` stay `not run` until one is observed.
