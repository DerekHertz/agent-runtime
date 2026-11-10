# Launcher: `art run` and `art query`

`art run <run-file> --run-id <id> --runs-dir <dir>` starts one run. It copies the run file's target tree to `<run-dir>/work`, writes `<run-dir>/settings.json` registering `art-shim` on PreToolUse, PostToolUse, PostToolUseFailure, Stop and SessionEnd, starts `art-coordinator`, and launches one headless agent per slot in the copied tree with a clean environment plus `AGENT_RUNTIME_SLOT` and `AGENT_RUNTIME_SOCKET`. When every session has ended it stops the coordinator and writes `<run-dir>/run.json`. `art query <run-dir> concurrent-writes` then lists every pair of concurrent writes to one path by two slots.

## Sub-features

- `launcher-one-trace` puts both sessions' hook events in one trace, each slot's in order.
- `launcher-settings` registers the shim on the five hook events, the gate with the run file's `gate_timeout_s` and every report with 30 s.
- `launcher-clean-env` gives a session none of the parent's `ANTHROPIC_API_KEY` or Claude Code variables.
- `launcher-outputs` keeps each session's stdout under `sessions/<slot>/` and a summary in `run.json`.
- `launcher-shutdown` stops the coordinator cleanly after the last session.
- `launcher-query` reports the two slots' edits of one file as a concurrent pair.
- `launcher-run-id-once` refuses a run id whose directory exists: exit 2, nothing changed.

## Driving it with verify.py

Preconditions: `verify.py doctor` passes, which now also needs the `art` binary.

- **Run.** `verify.py drive launcher`. The run file names `bench/toy` as the target, two slots, `gate_timeout_s` 75, and `scripts/fake_agent.py` as the agent. The parent environment carries `ANTHROPIC_API_KEY`, `CLAUDECODE` and `CLAUDE_CODE_ENTRYPOINT`.
- **Expected.** `art run` exits `0` and prints `coordinator shut down cleanly: true`. Each session's `stdout.json` shows its slot, the run's socket, the copied tree as its working directory, none of the three parent variables, and exit `0` from all four hooks, with an allow decision from the gate. The trace holds 8 rows: `BeforeTool, AfterTool, Stop, SessionEnd` per slot. `art query … concurrent-writes` prints one pair on `/work/src/lib.rs` and `1 concurrent write pairs`.
- **Refused.** The same command again exits `2` with `already exists`, and the trace still holds 8 rows.

## Gotchas

- The agent is a stand-in. `fake_agent.py` reads the generated settings and runs each registered hook command once, as Claude Code would for one Edit call, with recorded fixtures on stdin. This proves the launcher, the settings and the trace. It does not prove how a live `claude -p` session behaves; that is the M1 run, which a human starts.
- No transcript is copied in this journey, because the stand-in writes none. With `claude`, the launcher copies `~/.claude/projects/<working directory with separators as dashes>/<session-id>.jsonl`; that path is untested until a live run.
- The path in the query output is `/work/src/lib.rs` because the fixture's tool input names it. A live session names the file in the copied tree.
- The coordinator's socket is `/tmp/art-<pid>.sock`, because a Unix socket path is limited to about 100 bytes.
