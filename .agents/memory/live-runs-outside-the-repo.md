---
name: live-runs-outside-the-repo
description: art run inside this repo makes sessions inherit CLAUDE.md and the cargo workspace; pass --runs-dir outside it
metadata:
  type: project
---

A live `art run` whose run directory is inside this repository is not a clean workload. The sessions' working tree is `<run-dir>/work`, so they load this project's `CLAUDE.md` and `AGENTS.md` as their own instructions, and `cargo test` fails because the copied crate sits under the parent workspace.

**Why:** observed on 2026-11-10 in `runs/m1`: both Haiku sessions tried to run this project's Codex review step and reported a workspace error. The same run under `~/art-runs` was clean.

**How to apply:** pass `--runs-dir` pointing outside any repository (for example `~/art-runs`) for every live run. The verify harness is unaffected, because its scratch runs live under `/tmp`. See `docs/research/m1-m2-conflicting-runs.md`.
