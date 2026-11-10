---
name: restored-sources-need-touch
description: "after restoring a source file from a backup copy (mutation checks), touch it or cargo may test a stale build"
metadata:
  node_type: memory
  type: project
  originSessionId: c6ab0948-e4e5-4846-8569-baa8b0a08a26
  modified: 2026-11-10T00:36:59.676Z
---

Cargo decides a crate is fresh by comparing source mtimes with the last build. A real-code mutation that copies a file aside, edits it, builds, then moves the copy back restores an mtime older than that build, so the next `cargo test` silently reuses the mutated binary. In the issue #3 AfterTool repair (2026-11-09) a new test failed against a fixed source this way.

**Why:** a stale build makes a correct fix look broken, or a broken one look fixed, and nothing in cargo's output says so.

**How to apply:** after restoring any source by `mv`/`cp`, `touch` it (or restore with `git checkout`, which writes a fresh mtime) before building again. Verify a surprising result by touching the file and rerunning. `verify.py build` now runs `cargo clean -p` on every workspace package before building, so its receipt can't vouch for a stale artifact; a plain `cargo test` still can, so touch before testing. Related: [[toy-fixture-test-fails-by-design]], [[verification-pass-needs-frozen-inputs]].
