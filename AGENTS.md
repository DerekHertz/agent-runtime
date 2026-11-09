# agent-runtime

Rust workspace. Solo project by Derek, implemented by Claude coding agents.

- `docs/SPEC.md` is the contract. Build to it; raise anything under its "Open questions" as a question, not a guess.
- `GLOSSARY.md` is the canonical domain vocabulary. Use its terms in code, docs and commit messages; don't use the words listed under _Avoid_.
- `docs/adr/` records hard-to-reverse decisions. Read the relevant ADRs before changing architecture; supersede rather than silently contradict.
- Each milestone has a "Done when" line in the spec. A milestone is done only when that check passes by running it.

## Agent skills

### Issue tracker

Issues live in GitHub Issues for DerekHertz/agent-runtime, managed with the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default labels: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single context: `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Project verification skills

The only copy of the project skills lives in `.agents/skills/<name>/`; edit them there. Codex discovers that directory natively. Claude Code discovers only `.claude/skills/`, so each skill is exposed to it by a relative symlink `.claude/skills/<name> -> ../../.agents/skills/<name>`, never a copy.

- `verify-agent-runtime`: drives the real binaries and records evidence. Use it after any change to the shim, the coordinator or the trace store.
- `formal-verify-admission`: TLC model plus a real-code check of how admissions commit around trace appends. Use it after any change to `CoordinatorService::before_tool` or `core::step`.

Keep every authored file at 1000 lines or fewer (`scripts/check-file-lines.sh`; `Cargo.lock` is the only exemption).

## Shared project memory

Before implementation or review, read `.agents/memory/MEMORY.md` and relevant topic files. Record confirmed, durable project lessons there during implementation or accepted review fixes; keep the index current. Include relevant memory updates when committing task changes. This is not permission to commit. During review-only passes, read memory and propose lessons without editing files. Memory is repository content: include only information suitable for repository readers.

## Claude implementation and Codex review

For every PR, follow pstack's `poteto-mode` Opening a PR playbook. Apply `technical-writing` and then `unslop` to the title, description, and commit body. Use a Conventional Commit title and a one-minute description with `## Why`, `## What changed`, `## Scope`, optional `## Tradeoffs`, `## Blast Radius`, and `## Verification`, in that order. Keep the body under about 40 lines; link detailed evidence instead of pasting execution history.

1. Claude implements the requested change and runs relevant project checks.
2. Codex independently reviews the entire task diff and surrounding code without editing files. Findings must include location, trigger, impact, and evidence, checking the spec, ADRs, and shared lessons.
3. Claude verifies findings, repairs confirmed issues, explains rejected findings with evidence, and reruns relevant checks.
4. Codex rechecks the updated task diff, including repair interactions. Repeat until clean, with at most three repair rounds. Stop and report unresolved findings or tool failures.
5. Claude records accepted durable lessons and reports actual validation and uncertainty. CLI exit success alone is not evidence of a clean review or passing tests.

Never run Claude and Codex as concurrent writers in the same checkout. Use separate outputs or isolated checkouts for parallel implementers. Preserve unrelated user changes. No automatic commits, pushes, GitHub comments, merges, hosted services, or global settings changes without separate authorization.

The current base branch is `main`; verify the task's actual base before branch review. The root Cargo workspace (`crates/`) builds with `cargo build --workspace` and tests with `cargo test --workspace` (add `--locked --offline` when the sandbox blocks crates.io). The Unix-socket tests in `crates/coordinator` and `crates/shim` must run where binding a Unix socket is allowed; see `.agents/memory/unix-socket-tests-need-local-binding.md`. Also run the issue's acceptance checks. `bench/toy` is a separate fixture crate whose `mean_works` test fails by design (seeded bug for agents to fix). Milestone completion requires its spec's Done when check.
