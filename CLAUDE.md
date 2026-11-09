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
