# agent-runtime

Coherent parallel coding agents: file-level write leases, vector-clock causal tracing, and MPI-shaped collectives (scatter, barrier, gather, allreduce) for N Claude Code sessions on one repository.

Status: M1 in progress. A PreToolUse call reaches the coordinator through the shim and is recorded as a stamped event in `runs/<id>/trace.db`.

## Checks

CI (`.github/workflows/ci.yml`) runs all of these:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`. The coordinator and shim tests bind Unix sockets, so run them where that is allowed.
- `scripts/check-file-lines.sh`, which fails if any authored file has more than 1000 lines. `Cargo.lock` is exempt because it is generated.

The minimum supported Rust version is 1.89, the version that stabilised `File::try_lock` and the highest `rust-version` among the locked dependencies. A separate CI job checks it. `bench/toy` is outside the workspace and CI skips it, because its `mean_works` test fails by design.

## Verification skills

- [`.agents/skills/verify-agent-runtime/`](.agents/skills/verify-agent-runtime/SKILL.md) drives the real `art-coordinator` and `art-shim` binaries through each mapped feature and keeps the evidence.
- [`.agents/skills/formal-verify-admission/`](.agents/skills/formal-verify-admission/SKILL.md) model-checks how admissions commit around trace appends and replays the property against the real code.

- [docs/SPEC.md](docs/SPEC.md): the spec, every section is a contract
- [GLOSSARY.md](GLOSSARY.md): canonical domain language (created as terms are settled)
- [docs/adr/](docs/adr/): architecture decision records (created as decisions are made)
