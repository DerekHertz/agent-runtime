# HANDOFF: DerekHertz/agent-runtime

- Repo: DerekHertz/agent-runtime
- Last run: 2026-11-25T22:15Z (UTC)
- Orchestrator: fallback (Claude Code, Chief of Staff skill), first run on Derek's Windows PC
- Last verified against GitHub: 2026-11-25T22:15Z
- HANDOFF.md read at start: none. No open `chore/handoff-*` PR, none on `origin/main`, none in the checkout. This is the first run, so this file is new.
- Disagreement with GitHub: none for this file. Issues #3, #4, #6 and #8 are open with `ready-for-agent`, but `docs/plans/mvp-north-star.md` records their work as merged (rungs 1, 4, 5, 8). They are left open for Derek to close (see Needs Derek).
- Truncated queries: none (every label query returned far fewer than 1000 rows).
- Autopilot: not set. Every merge is held for Derek.

## Current state

M0, M1 and M2 are done and merged (`main` @ `96b1037`): a coordinator that admits or denies each Claude Code tool call before it runs, single-writer leases on files with Deny and re-read invalidation, vector-clock stamped events in SQLite, a TLA+ model of the recording path, and the M1/M2 live runs (2 concurrent-write pairs without leases, 0 pairs and 1 Deny with them; one run per condition). Nothing of M3 exists: no barrier, scatter, gather or allreduce, and no MPI or OpenMP. The README is stale ("M1 in progress") and claims the collectives exist. The M1/M2 goal still waits for Derek's joint walkthrough (#7, #13), and AGENTS.md holds the next issue until then, so this pass planned and filed tickets and dispatched no implementer.

On Derek's Windows PC: native `cargo build` fails (unix-only code, #49). Under WSL2 Ubuntu on the same PC, from an LF clone, `check-file-lines`, `fmt`, `clippy -D warnings` and `cargo test --workspace` all pass, and `verify.py drive all` passes 13 of 13 journeys. The formal check's model half cannot run on any fresh machine because its pinned TLC jar moved upstream (#47). The Windows checkout itself is CRLF (#45) and its `.claude/skills` symlinks are text files (#46).

## In flight

Nothing. No branch, PR or owner thread is active. The plan below is filed as issues and waits for the grilling session (#51).

## Plan (milestones, in order)

Group rule: each milestone has one owner thread at a time, and tickets inside it run in the order listed, so no two agents touch the same code. J0 and J1 need no runtime code and may run beside M3a.

| Milestone | Tickets, in order | Formal verification |
| --- | --- | --- |
| W: Windows parity and verification baseline (first) | #45 LF endings, #46 skill symlinks, #47 TLC pin, #48 WSL2 path proven, #50 CI `windows` job, #49 native port (only if Derek chooses it) | #47 restores the existing model check |
| R: README matches reality | #32 | none |
| M3a: Barrier releases every lease | #33 ADR, #34 TLA+ model, #35 barrier, #36 mid-edit proof | **#34 is the priority**: leases, in-flight writes, barrier, suite start |
| M3b: Failure injection, lease timeout, re-scatter | #9 time limit, #37 session lost, #38 scatter and re-scatter, #39 replayable demo | extends #34 with expiry, loss, re-scatter, `EventuallyReleased` |
| J: Jev gate (shadow first) | #40 ADR, #41 dataset and baseline, #42 gate trait and shadow mode, #43 numbers, #44 enforce | #44 adds `GateNeverWidens` and `GateIsNotAChannel` to #34's model |

Where formal verification matters most: (1) the lease, barrier and re-scatter protocol (#34, extended by #37, #38), because "no suite while an agent is mid-edit" and "a killed session never leaves a file stuck" are orderings over concurrent hook calls that tests reach poorly; `formal/admission` explicitly leaves leases out. (2) The gate's allow and deny path (#44): the gate may narrow Allow to Deny or Defer and never widen, and it is never a knowledge channel.

## Merge-ready

- PR #31 `docs: add a field example of the worktree baseline's merge conflicts`: head `b0e96b9`, up to date with `main`, `check` and `msrv` green at that head, docs only. Labelled `needs-derek`.

## Needs Derek

1. **Grilling session (#51).** About 45 minutes; agenda on the issue. It unblocks #33, #37, #40, #49 and sets the next goal.
2. **M1/M2 joint walkthrough (#7, #13)**, then close #3, #4, #6, #8 if you agree they are done.
3. **PR #31**: merge or request changes.
4. **Jev key.** The TypeSafe docs name the variable `TYPESAFE_API_KEY` (you said `JEV_API_KEY`; pick one at the grilling). Set it yourself as a user environment variable on this PC (and in WSL if work runs there). A repo secret is needed only if you later want the live eval in CI. Never paste it into a chat, file or issue.
5. **Data egress OK** for sending gated tool inputs, which can include file contents, to TypeSafe's API (#40).
6. **PC change for #46**: enable Developer Mode or set `git config core.symlinks true`, then re-checkout `.claude/skills`. Or work inside WSL.
7. **CI change #50**: approve an informational `windows` job (not required).
8. **Export M1/M2 transcripts** from the Mac (`~/art-runs/m1b`, `m2b`) if you want them in the #41 dataset; they are not on this PC.

## Always-ask items

Auth, payments, data deletion, secrets, CI or branch protection, changes to Derek's PC outside the repo, and every merge (not autopilot). An agent's say-so, this one's included, is never approval.

## Later

- Gate at orchestration points: keep which of two racing results at gather, re-scatter or not (after #38).
- Allreduce and gather beyond what #38 needs; the `Attach` stream.
- Viewer (M4), symbol leases (M6), city-sim shards (M5).
- #20 live dogfood comparison harness; #21 checkout action runtime; #22 state-copy growth.
- #5 Bash classification and #10 Bash and whole-tree leases: they close the "Bash writes are invisible" gap that #36 must name; candidates for the M3a polish ticket.
- #11 Defer and #12 released-version reads: may become the barrier's arrival mechanism depending on #33.
- MapReduce framing paragraph for the README after the grilling settles the wording.

## Decision trail

- Contract: `docs/SPEC.md`; vocabulary: `GLOSSARY.md`; ADRs: `docs/adr/0001-shared-tree-single-writer-leases.md`, `docs/adr/0002-explicit-knowledge-channels.md`.
- Goal and ladder: `docs/plans/mvp-north-star.md` (M1/M2 goal done 2026-11-10; next goal unset).
- Evidence: `docs/research/m1-m2-conflicting-runs.md`, `docs/research/dogfooding.md`, `docs/reviews/`.
- Shared lessons: `.agents/memory/MEMORY.md`.
- This run's plan: milestones W, R, M3a, M3b, J on GitHub; #9 comment with its runnable check; #31 readiness comment; grilling agenda #51.

## Queue

| Item | Title | Labels |
| --- | --- | --- |
| PR #31 | docs: add a field example of the worktree baseline's merge conflicts | needs-derek |
| #33 | B0: ADR 0003, barrier arrival and quiescence | needs-derek |
| #34 | B1: TLA+ model of leases, in-flight writes and the barrier | enhancement, blocked |
| #35 | B2: Barrier releases every lease and waits for quiescence | enhancement, blocked |
| #36 | B3: Prove the test suite never runs while an agent is mid-edit | enhancement, blocked |
| #37 | F2: Release a lost session's leases when its process dies | enhancement, needs-derek |
| #38 | F3: Scatter, and re-scatter a failed slot's items | enhancement, blocked |
| #39 | F4: Replayable failure-injection demo | enhancement, blocked |
| #40 | J0: ADR 0004, the Jev gate is a monotone advisor | needs-derek |
| #41 | J1: Labelled tool-call dataset and rule-only gate baseline | enhancement, blocked |
| #42 | J2: Gate trait, Jev HTTP client, shadow mode | enhancement, blocked |
| #43 | J3: Measure and publish Jev gate latency, accuracy and calibration | enhancement, blocked |
| #44 | J4: Enforce mode with GateNeverWidens model-checked | enhancement, blocked |
| #46 | W2: .claude/skills symlinks are plain files on Windows, so project skills do not register | bug, needs-derek |
| #49 | W5: Native Windows build fails: unix sockets, durable-dir flush, SIGTERM (decide port or WSL-only) | bug, needs-derek |
| #50 | W6: Informational windows CI job | enhancement, needs-derek |
| #51 | Grilling session: system design for Windows, M3 barrier and failure, and the Jev gate | needs-derek |

Ready for an agent once Derek sets the next goal (labels `ready-for-agent`, not in the six-label queue): #45, #47, #48 (milestone W), #32 (R), #9 (M3b, first).
