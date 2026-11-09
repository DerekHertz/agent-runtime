# Shared project memory

Project contracts live in AGENTS.md, docs/SPEC.md, GLOSSARY.md, and docs/adr/.

- [Unix socket tests need local binding](unix-socket-tests-need-local-binding.md) — sandbox EPERM on bind; run tests with local binding allowed and check exit codes
- [Toy fixture test fails by design](toy-fixture-test-fails-by-design.md) — bench/toy mean_works is the seeded bug; not a regression
- [Shim may exit before reading stdin](shim-may-exit-before-reading-stdin.md) — test helpers piping stdin must tolerate BrokenPipe only
- [Proto build uses protox](proto-build-uses-protox.md) — no system protoc; extend proto/runtime.proto only
