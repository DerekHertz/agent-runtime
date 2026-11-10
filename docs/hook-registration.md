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
    ],
    "PostToolUseFailure": [
      {
        "matcher": "Edit|Write|NotebookEdit|Read|Bash",
        "hooks": [
          { "type": "command", "command": "/abs/path/to/art-shim --hook-event PostToolUseFailure", "timeout": 30 }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "/abs/path/to/art-shim --hook-event Stop", "timeout": 30 }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          { "type": "command", "command": "/abs/path/to/art-shim --hook-event SessionEnd", "timeout": 30 }
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
| `art-shim --hook-event PostToolUseFailure` (a report of a failed or interrupted call) | nothing printed, exit 0 | exit 1, a nonblocking error, with the reason on stderr |
| `art-shim --hook-event Stop` (a lifecycle report) | nothing printed, exit 0 | exit 1, a nonblocking error, with the reason on stderr; never exit 2 and never a `decision` on stdout, either of which would keep the session from stopping |
| `art-shim --hook-event SessionEnd` (a lifecycle report) | nothing printed, exit 0 | exit 1, a nonblocking error, with the reason on stderr |
| `art-shim` with no arguments (the issue #2 registration) | as `--hook-event PreToolUse`; any other event passes through silently | exit 2 |

A report never exits 2: on PostToolUse that only feeds an error to the model, and on Stop it would keep the session from stopping. The stdin JSON must be the registered event, or the shim fails with that registration's policy. Outside a run (neither variable set, or both empty) every registration exits 0 silently and never contacts the coordinator, whatever its arguments. A command line the shim cannot parse fails closed inside a run, because it may belong to a gate, unless it starts `--hook-event <report event>`: then even stray arguments after the event fail with exit 1. Event names are exact and case-sensitive (`--hook-event stop` is unknown and fails closed). The event named on the command line is trusted. A registration that names the wrong event gets that event's policy, and the shim cannot detect it. A PostToolUseFailure report needs the hook's `error` as a string; `is_interrupt` may be omitted, but when present must be a boolean. A Stop's `stop_hook_active` and `last_assistant_message` and a SessionEnd's `reason` may each be omitted; when present they must be a boolean, a string and a string. Any other type, `null` included, is a report the shim cannot make (exit 1).

Stop and SessionEnd have no matcher: Claude Code runs them once per stop or session end. The shim keeps only the fields named above (SPEC section 5); `transcript_path`, `cwd` and the rest of the hook input are not recorded.

## Timeouts

Claude Code kills a hook at its `timeout` (seconds), and a killed hook lets the call through (M0 experiment 3). So each registration's timeout must exceed the shim's own bound, and the shim gives up first and reports why:

- **PreToolUse: 90 s.** SPEC sets the hook timeout to the Defer timeout (60 s by default) plus 30 s. The shim gives up after 80 s and exits 2.
- **PostToolUse, PostToolUseFailure, Stop and SessionEnd: 30 s each.** A report never waits on a lease, so the shim bounds the whole report (connect plus RPC) at 10 s (`REPORT_TIMEOUT` in `crates/shim/src/transport.rs`) and exits 1. A report the shim gave up on may still have been recorded if the coordinator answered late; one cut short by the hook timeout instead would leave no reason on stderr, which is why the margin is wide.
- **Every report registration: at least 20 s.** While the report bound is 10 s, no report hook may be registered with a timeout or overall budget at or below it. 20 s is the minimum; 30 s is used above.
- **SessionEnd** defaults to a 1.5 s budget in Claude Code, up to 60 s ([research notes](research/claude-code-hooks.md)), which would kill the shim long before its 10 s bound. Its registration above therefore sets a per-hook `timeout` of 30 s, inside that 60 s maximum. That a per-hook timeout raises the SessionEnd budget comes from those notes and the hooks reference; it has not been observed in a live session here. If the budget stays at 1.5 s, a slow coordinator's SessionEnd may be cut short with no reason on stderr; it can never block, because SessionEnd cannot block.

## Status

This registration has been exercised only through recorded and synthetic hook JSON piped to the real binaries (the verify skill's journeys and the shim's tests). It has not been run under a live Claude Code session; the dogfooding rows in `docs/research/dogfooding.md` stay `not run` until one is observed. No hooks are installed in any global settings by this repository.
