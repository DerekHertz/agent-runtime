---
name: unix-socket-tests-need-local-binding
description: Coordinator/shim tests bind Unix sockets; the Claude Code sandbox refuses this with EPERM
metadata:
  type: project
---

Tests in `crates/coordinator/tests/adapter.rs`, `crates/coordinator/src/adapter.rs`, `crates/shim/tests/cli.rs` and `crates/shim/tests/e2e.rs` bind Unix domain sockets. Inside the Claude Code sandbox `UnixListener::bind` fails with `Operation not permitted (os error 1)`; there only `$TMPDIR` and `/tmp/claude` are writable. Codex's review sandbox hit the same failure.

**Why:** a sandboxed `cargo test --workspace` therefore fails without any product defect (seen 2026-11-09, issue #2).

**How to apply:** run the workspace tests outside the sandbox (verified). `sandbox.network.allowLocalBinding: true` is documented for TCP ports and is unverified for Unix-socket bind. Keep socket paths short (macOS limit ~104 bytes): tempdirs from `$TMPDIR` with a short prefix and `c.sock`. Check exit status directly or with `set -o pipefail`; don't infer success from grepped output. See [[toy-fixture-test-fails-by-design]].
