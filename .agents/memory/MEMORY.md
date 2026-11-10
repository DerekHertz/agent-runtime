# Shared project memory

Project contracts live in AGENTS.md, docs/SPEC.md, GLOSSARY.md, and docs/adr/.

- [Unix socket tests need local binding](unix-socket-tests-need-local-binding.md) — sandbox EPERM on bind; run tests with local binding allowed and check exit codes
- [Toy fixture test fails by design](toy-fixture-test-fails-by-design.md) — bench/toy mean_works is the seeded bug; not a regression
- [Shim may exit before reading stdin](shim-may-exit-before-reading-stdin.md) — test helpers piping stdin must tolerate BrokenPipe only
- [Directory existence is not durability](directory-existence-is-not-durability.md) — flush the full canonical chain to the filesystem anchor; concurrent creators may not have flushed
- [Proto build uses protox](proto-build-uses-protox.md) — no system protoc; extend proto/runtime.proto only
- [Test calls need unique tool_use_ids](test-calls-need-unique-tool-use-ids.md) — a repeated (session, tool_use_id) or a session on a second slot is denied; give each test call its own id
- [Restored sources need touch](restored-sources-need-touch.md) — a file moved back from a backup keeps an old mtime; cargo may test the stale build
- [Raw JSON null is not absent](raw-json-null-is-not-absent.md) — serde reads null into Option as None; use a `present` deserialize_with when null is a valid value
- [Verification PASS needs frozen inputs](verification-pass-needs-frozen-inputs.md) — drive's end checks gate PASS; endpoint hashes miss mid-run edit+restore, so freeze while driving
