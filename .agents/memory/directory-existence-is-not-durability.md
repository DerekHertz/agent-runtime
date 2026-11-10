---
name: directory-existence-is-not-durability
description: "Making a directory crash-durable means flushing its whole canonical ancestor chain to the filesystem anchor, not just the parents this call created"
metadata:
  node_type: memory
  type: project
  originSessionId: 208366b7-65c4-4c39-b15b-5529a0be2d90
  modified: 2026-11-09T22:59:33.516Z
---

A directory that exists may not be durable: a concurrent creator can make it visible and not have flushed its parent yet. `trace-store`'s `durable_dir::create` therefore flushes every directory on the canonical path, from the run directory up to its filesystem anchor (the topmost directory on the same device, or `/`), whether or not it already existed.

**Why:** issue #3 PR 2 review (P2) found that taking existing ancestors as durable let a second concurrent `TraceWriter::create` win `trace.lock`, append and allow before the first creator's ancestor flush landed. A crash could then lose the run directory.

**How to apply:** for any new "make this path durable" code, never infer durability from `exists()`, and never add a global lock to order creators. Flush the full chain and return flush errors with the path. Tests can check which directories were flushed, but they do not simulate power loss; say so. See [[unix-socket-tests-need-local-binding]] for running the rest of the suite.
