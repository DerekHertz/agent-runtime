---
name: toy-fixture-test-fails-by-design
description: bench/toy mean_works fails on purpose; it is the seeded bug agents fix in M1/M2 runs
metadata:
  type: project
---

`cargo test --manifest-path bench/toy/Cargo.toml` fails `tests::mean_works` (left 1.5, right 2.0) because of the deliberate off-by-one marked `BUG (deliberate)` in `bench/toy/src/lib.rs`.

**Why:** the toy crate is the conflicting-edit fixture for M1/M2; agents are prompted to fix it in temp copies.

**How to apply:** treat that one failure as expected baseline, not a regression; never "fix" it in the repo. Running it in place leaves `bench/toy/Cargo.lock` and `bench/toy/target/` untracked; delete them afterwards.
