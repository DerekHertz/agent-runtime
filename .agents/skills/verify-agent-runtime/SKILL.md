---
name: verify-agent-runtime
description: Drive the real agent-runtime binaries (art-coordinator, a Unix-socket gRPC server, and art-shim, the Claude Code PreToolUse, PostToolUse, PostToolUseFailure, Stop and SessionEnd hook program) the way Claude Code would, and prove behavior from exit codes, hook JSON and trace.db rows. Use to verify a change to the coordinator, shim, trace store or clock before calling it done, or whenever a unit test passing is not enough proof.
---

# Verify agent-runtime

Surface: three CLI binaries. `art` starts a run (`art run`) and queries a finished one (`art query`); the `launcher` journey drives it. The other two are the hook path. Claude Code runs `art-shim` once per hook event with the event JSON on stdin and the env vars `AGENT_RUNTIME_SLOT` and `AGENT_RUNTIME_SOCKET`. For PreToolUse (no arguments, or `--hook-event PreToolUse`) the shim asks `art-coordinator` over a Unix socket and prints a hook decision; for PostToolUse (`--hook-event PostToolUse`) it reports the completed call, and for PostToolUseFailure (`--hook-event PostToolUseFailure`) the failed or interrupted one, and prints nothing; `--hook-event Stop` and `--hook-event SessionEnd` report those hooks the same silent way. The coordinator records a stamped event per admitted call and per recorded report in `<runs-dir>/<run-id>/trace.db` (SQLite). There is no UI and no persistent server: each drive starts its own coordinator.

Everything is scripted by `scripts/verify.py` (python3 stdlib, no pip). Run it from anywhere; it finds the repo from its own path. Terms follow `GLOSSARY.md`: slot, run, coordinator, shim, clock.

## Run it outside the sandbox

`UnixListener::bind` fails with EPERM (Operation not permitted) inside the Claude Code sandbox, so the coordinator cannot start there. Run every `verify.py` command that starts a coordinator (`doctor`, `drive`) with the sandbox disabled. Doctor reports this case explicitly as `can bind a Unix socket here: FAIL ... EPERM`. Do not "fix" it by mocking the socket. See `.agents/memory/unix-socket-tests-need-local-binding.md`.

## Launch

Build once (Cargo, offline, locked); binaries land in `<target>/debug/`:

```
CARGO_TARGET_DIR=<repo>/target/verify .agents/skills/verify-agent-runtime/scripts/verify.py build
```

Without `CARGO_TARGET_DIR` the default is `<repo>/target`; `--target-dir <dir>` overrides both. Pass the same `--target-dir` (or env) to `doctor` and `drive`.

`build` first runs `cargo clean -p <name>` for every root workspace package (names from `cargo metadata --no-deps --locked --offline`; `cargo clean` has no `--workspace`) in that target dir, and prints the list. Cargo judges freshness by mtime, so a source restored with an older mtime than its artifact would otherwise leave a stale artifact under a receipt that names the current sources. Third-party dependencies stay cached. If metadata or the clean fails, `build` writes no receipt. The receipt lists the cleaned packages under `cleaned_packages`.

There is nothing to keep running. Each journey starts its own `art-coordinator` on a short socket in a fresh scratch dir (`/tmp/artv-XXXXXXXX/c.sock`, run dir `/tmp/artv-XXXXXXXX/runs/r1`), equivalent to:

```
art-coordinator --run-id r1 --slots slot-1,slot-2 --runs-dir <scratch>/runs --socket <scratch>/c.sock
```

Ready means the socket file exists and accepts a connection (the harness polls up to 15 s; `art-coordinator: listening on <path>` appears on stderr). The trace starts empty: no seed data or migrations are needed. Socket paths must stay under about 100 bytes (macOS limit 104), which is why the scratch root is `/tmp`, not `$TMPDIR`.

Teardown is `cleanup` (below); `drive` also runs it after every feature.

## Doctor

```
.agents/skills/verify-agent-runtime/scripts/verify.py [--target-dir D] doctor [--save]
```

Read-only (it only binds and removes a throwaway socket in `/tmp`). Run it first, and again whenever anything looks off. It checks: both binaries exist; the build receipt that `verify.py build` writes to `<target>/debug/verify-agent-runtime-build-receipt.json` exists, its source fingerprint (the `sources.aggregate_sha256` below) equals the current sources, and its sha256 for each binary equals the binary on disk (a stale or swapped binary = wrong evidence: rebuild). Freshness is by content, not mtime, because an incremental build correctly leaves a binary whose inputs did not change untouched; `art-coordinator --help` works; `art-shim` with no env exits 0 silently; the hook fixtures exist; python sqlite3 works; the scratch socket path is short enough; a Unix socket can be bound here (the EPERM case above); no unfinished harness state is left over. Exit 0 ready, 2 blocked. `drive` runs the same checks and stops with status BLOCKED if any fail.

## Drive

```
verify.py list                         # feature ids
verify.py drive all                    # every journey in features/
verify.py drive admitted-tool-call     # one journey
```

Recipes and expected end states live in `features/` (index: `features/README.md`). Each journey invokes the real binaries exactly as Claude Code would:

- Hook event: the JSON from `crates/shim/tests/fixtures/{pre_tool_use_edit,pre_tool_use_bash,post_tool_use_edit,post_tool_use_failure_bash,stop,session_end}.json` goes on `art-shim`'s stdin, with env `AGENT_RUNTIME_SLOT=<slot>` and `AGENT_RUNTIME_SOCKET=<scratch>/c.sock`, and for a report the arguments `--hook-event PostToolUse` or `--hook-event PostToolUseFailure`.
- Allow: stdout is `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}`, exit 0. Deny: the same shape with `"permissionDecision":"deny"` and a reason, exit 0. Gate failure inside a run: exit 2, empty stdout, stderr starting `agent-runtime:`. Recorded report: exit 0, no output. Report failure inside a run: exit 1, empty stdout, stderr starting `agent-runtime:`. Outside a run (neither variable set, or both empty): exit 0, no output.
- Observation: `trace.db` table `events(event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash, tool_input, wall_time, tool_use_id, before_event_id, outcome, input_payload_hash)`. `agent_id` holds the slot. A row's payload lives in `<run-dir>/blobs/<lowercase hex of payload_hash>`, byte for byte: a BeforeTool's raw tool input, an AfterTool's raw tool response. An AfterTool's reported tool input is the blob named by `input_payload_hash`, its `before_event_id` names its BeforeTool (NULL for an orphan), and its `outcome` is `success`. The `tool_input` column is NULL. Only traces written before blobs (issue #2) keep it inline in `tool_input`. `vclock` is little-endian u64 per slot in slot order (slots `slot-1,slot-2`: slot-2's first event is `[0,1]`). The harness reads it read-only; by hand: `sqlite3 -readonly <db> 'select event_id,agent_id,tool,hex(vclock) from events'`.

To drive by hand for a quick look, start the coordinator command above in one terminal, then `AGENT_RUNTIME_SLOT=slot-2 AGENT_RUNTIME_SOCKET=<scratch>/c.sock art-shim < crates/shim/tests/fixtures/pre_tool_use_edit.json; echo $?`. Prefer the harness: it records everything.

## Evidence

Every `drive` writes `<repo>/.claude/reviews/evidence/verify-agent-runtime/<UTC-timestamp>/` (gitignored via `.claude/reviews/`):

- `summary.json`: status (PASS/FAIL/BLOCKED), per-feature and per-check pass/fail with the observed values (exit codes, stdout/stderr), doctor results, UTC start/finish, git HEAD, and path, size and sha256 of both binaries. Source provenance is the `sources` block: the implementation is often uncommitted, so HEAD alone does not identify the code, and the `git status --porcelain` hash (also recorded, with its line count) does not change when an already-modified file is edited again. `sources.aggregate_sha256` is therefore the identifying hash: sha256 over the lines `<repo-relative path>\t<sha256 of file bytes>\n`, sorted by path, for every file matching `Cargo.toml`, `Cargo.lock`, `proto/**`, `crates/**/src/**`, `crates/**/build.rs`, `crates/**/Cargo.toml` (anything under a `target` dir excluded). `sources.file_count` and `sources.per_file_list` (`sources.json` in the same evidence dir, path to sha256) let a reviewer see which file differs between two runs. `end_checks` are rerun after the last journey and gate the verdict: `end: sources unchanged since the run started` (the aggregate again, listing any changed path), `end: build receipt is the one doctor checked at the start`, and doctor's receipt checks again (receipt present, built from the current sources, each binary's sha256 equal to the receipt). Any failed end check makes the status FAIL (exit 1) even when every journey check passed; `status_reasons` lists each failed feature and end check, and the console prints them. `sources.unchanged_during_run` repeats the first end check. A drive BLOCKED by doctor stays BLOCKED. The end checks compare two endpoints only: an edit or a binary swap that is undone before the run ends is invisible to them, so keep the sources and the target dir frozen while a drive runs. Doctor ties the binaries to those sources through the build receipt: `verify.py build` fingerprints the sources before and after `cargo build`, writes the receipt only if the build succeeded and the sources did not change during it, and deletes any older receipt first.
- `<feature>/NN-<label>.json`: one file per command: argv, stdin, which `AGENT_RUNTIME_*` vars were set, exit code, stdout, stderr, UTC start and end.
- `<feature>/trace-<label>.json`: decoded `trace.db` rows read after the action: slot, session, tool, vclock as a list, `payload_hash` as hex, the inline `tool_input` column, `payload_source` (`blob`, `inline`, `none`, or `missing` when the named blob is absent) with the resolved `payload` text, the same three for `input_payload_hash`, `tool_use_id`, `before_event_id`, `outcome`, and wall_time. The harness does not rehash blobs, because python's stdlib has no blake3; the Rust tests cover hash correctness.
- `<feature>/coordinator-<tag>.stdout|stderr`: the coordinator's own output.

Proof standards:

- Exercise the real user path: real binaries, real socket, hook JSON on stdin. Never call library functions or test-only entry points, and never write to `trace.db` yourself.
- Capture the action and the resulting state: the shim's exit code and output AND the rows in the trace. A passing exit code alone proves nothing, and absence of a row is checked by reading the trace, not by assuming.
- Order is `event_id`; causal order is the `vclock`. `wall_time` is display-only (SPEC section 5): record it, never assert on it.
- Nothing is mocked. The only stand-in is that the hook JSON comes from recorded fixtures instead of a live Claude Code session. A claim about the live Claude Code hook protocol needs `docs/research/m0-hook-probe.md` or an M0 probe run, not this harness.
- Never report a skipped journey as verified through another one. A journey the product fails reports FAIL; the reason goes in that feature's Gotchas. Never weaken a journey to make it pass.

## Freshness proof

```
.agents/skills/verify-agent-runtime/scripts/freshness_proof.py [--feature admitted-tool-call] [--scratch-root /tmp] [--target-dir D]
```

Proves the end guard with the real binaries. Run it after `verify.py build`, outside the sandbox like `drive`. It runs three real drives of one journey, each on its own temporary copy of the fingerprinted sources plus copies of both binaries and the receipt: `baseline` (must be PASS, exit 0), `source` (appends a comment to the copy's `crates/coordinator/src/core/mod.rs`) and `binary` (appends a byte to the copied `art-shim`). Both drift scenarios must be FAIL, exit 1, with every journey check passed and the matching end check failed. The change is made by wrapping the journey's `FEATURES` entry: the real journey runs first, and the copy changes only after all its checks pass. The checkout's sources, binaries and receipt are hashed before and after and must match. The copies are removed. Evidence goes to `<repo>/.claude/reviews/evidence/verify-agent-runtime-freshness/<UTC>/proof.json`, with each scenario's drive evidence beside it. Exit 0 proven, 1 failed, 2 blocked (stale receipt, or doctor blocked, for example on socket bind).

## Cleanup

```
verify.py cleanup
```

The harness records every coordinator PID and scratch dir it creates in `<evidence-root>/active-processes.json`. Cleanup sends SIGTERM (then SIGKILL) only to those PIDs, and only while the process command line still contains the harness's own socket path (PID-reuse guard); it never kills by name. It removes only `/tmp/artv-*` scratch dirs it created. It never deletes anything under `.claude/reviews/evidence/`. `drive` refuses to start while that registry is non-empty, so run `cleanup` after any crash or Ctrl-C. Afterwards confirm: `cleanup` prints "nothing to clean up" on a second run, `ls /tmp/artv-*` finds nothing, and the evidence dir is intact.

## Isolation

Each drive uses its own run id dir and socket, so concurrent harness runs on different scratch dirs do not collide, but one checkout's evidence root has a single registry: drive one at a time per checkout. Never point the harness at a coordinator it did not start.

## Helpers

- `scripts/verify.py` (executable): `build`, `doctor [--save]`, `drive <feature|all>`, `cleanup`, `list`. Global options `--target-dir`, `--scratch-root` precede the subcommand. Exit codes: 0 pass, 1 a check failed, 2 blocked.
- `scripts/freshness_proof.py`: the end-guard proof above. It imports verify.py and points its `REPO`, `EVIDENCE_ROOT` and `ACTIVE` at the copy for each scenario, then restores them.
- `scripts/after_tool.py`: the `after-tool` journey, kept apart to hold each file under the line cap. It takes verify.py's helpers as an argument and imports nothing from it; verify.py imports it without writing `__pycache__`.
- `scripts/failed_tool.py`: the `failed-tool` journey (PostToolUseFailure), kept apart the same way.
- `scripts/lifecycle.py`: the `lifecycle` journey (Stop and SessionEnd), kept apart the same way.
- `scripts/launcher.py`: the `launcher` journey (`art run` and `art query`), kept apart the same way. `scripts/fake_agent.py` is the stand-in agent it starts in place of `claude`.
- `scripts/leases.py`: the `leases` journey (a write denied for a lease, and its release), kept apart the same way.
- `features/`: the maintained feature map. Add a feature file and a `FEATURES` entry in `verify.py` together.
