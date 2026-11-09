---
name: shim-may-exit-before-reading-stdin
description: art-shim exits before reading stdin outside a run or on bad config; test helpers piping stdin must tolerate BrokenPipe
metadata:
  node_type: memory
  type: project
  originSessionId: dcc1fcf1-9e3c-499b-989a-00c754919663
  modified: 2026-11-09T19:06:00.655Z
---

`art-shim` returns before reading stdin when it is outside a run (both variables unset) or when exactly one of `AGENT_RUNTIME_SLOT`/`AGENT_RUNTIME_SOCKET` is set. A test that pipes hook JSON to it can get `EPIPE` on `write_all`, depending on timing. The `run_shim` helper in `crates/shim/tests/cli.rs` ignores only `ErrorKind::BrokenPipe` and still checks exit code and output.

**Why:** Hosted CI on PR #14 failed intermittently in `exactly_one_variable_set_fails_closed` because the helper panicked on the broken pipe. The product behavior was correct.

**How to apply:** Any new helper that feeds stdin to `art-shim` and can hit an early-exit path should handle `BrokenPipe` the same way. Do not make the shim drain stdin just to please tests. See [[unix-socket-tests-need-local-binding]].
