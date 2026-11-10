# MVP north star and execution boundary

The product promise is coherent parallel coding on one shared repository: Claude Code sessions can work without silently overwriting each other, and the trace explains what each agent knew. Follow the guarantees and milestones in [SPEC](../SPEC.md); this document does not expand them.

The first product proof is the M1/M2 pair: detect a same-path conflict in a deliberately conflicting run, then repeat that workload with leases and show zero such conflicts plus the Deny that prevented them. Collectives, the viewer and benchmarks remain their later spec milestones.

## Current goal

Set 2026-11-10 by Derek: finish the M1/M2 proof. The goal is done when the same conflicting two-slot run on the toy crate has been run twice and the trace shows both results:

1. **M1:** with the coordinator allowing everything, `art query concurrent-writes` reports at least one same-path concurrent pair.
2. **M2:** with leases on, it reports zero pairs, and the trace shows the Deny that prevented them.

Nothing else is in the goal. Work that does not move a rung below is filed as an issue and deferred.

**Status 2026-11-10: both checks pass on live runs.** Every rung below is climbed. What the result does and does not show is in the [write-up](../research/m1-m2-conflicting-runs.md). The next goal is not set; it waits for the joint walkthrough with Derek.

### The ladder

Each rung is one small PR that ends in a check someone can run. A rung is climbed only when its check has passed. This table is the only record of where the work stands; update it in the PR that climbs the rung.

| Rung | Work | Check that climbs it | State |
| --- | --- | --- | --- |
| 1 | [#3](https://github.com/DerekHertz/agent-runtime/issues/3) PR 6: Stop and SessionEnd | Issue #3's five criteria pass in `e2e_acceptance.rs`; `verify.py drive all`; `formal/admission/check.sh` | climbed 2026-11-10 |
| 2 | Land the issue #3 stack (PRs #15 to #19 and PR 6) | All six merged to `main`, CI green | climbed 2026-11-10 (merge of #23) |
| 3 | Model the domain: `Call` as an enum of its three states; `Event` built by per-kind constructors and read through `Event::body` | Workspace tests and `check.sh --code` pass with no behavior change | climbed 2026-11-10 |
| 4 | [#4](https://github.com/DerekHertz/agent-runtime/issues/4) Causal queries | `art query concurrent-writes` lists a planted pair in a fixture trace | climbed 2026-11-10 |
| 5 | [#6](https://github.com/DerekHertz/agent-runtime/issues/6) Launcher | `art run` with a two-slot run file puts both sessions' hook events in one trace | climbed 2026-11-10 with a stand-in agent; a live session is rung 6 |
| 6 | [#7](https://github.com/DerekHertz/agent-runtime/issues/7) **M1 run** (Claude ran it at Derek's go-ahead) | Goal check 1 | climbed 2026-11-10: 2 pairs in `m1b` ([write-up](../research/m1-m2-conflicting-runs.md)) |
| 7 | Classify the file each Edit, Write, NotebookEdit and Read names, in the core, and record it in `path` (the part of [#5](https://github.com/DerekHertz/agent-runtime/issues/5) the leases need) | A recorded BeforeTool carries its file; `Run` uses it | climbed 2026-11-10 |
| 8 | [#8](https://github.com/DerekHertz/agent-runtime/issues/8) Leases with Deny, with the session as an explicit state machine that releases on Stop and SessionEnd | Two slots write one file: the second is denied and the Deny is in the trace | climbed 2026-11-10 |
| 8b | Invalidation on release and the two knowledge channels of [#5](https://github.com/DerekHertz/agent-runtime/issues/5): Read absorbs the file's release clock, Deny absorbs the holder's lease clock | A Read after another slot's write and release orders that write before the reader's next event | climbed 2026-11-10 |
| 9 | [#13](https://github.com/DerekHertz/agent-runtime/issues/13) **M2 run** (Claude ran it at Derek's go-ahead) | Goal check 2 | climbed 2026-11-10: 0 pairs and 1 Deny in `m2b` ([write-up](../research/m1-m2-conflicting-runs.md)) |

Rungs 4 and 5 do not depend on each other and can be built in parallel. Rung 3 comes first because rungs 4 and 8 read the two types it fixes.

### Cut from this goal

- Bash classification from #5, and [#10](https://github.com/DerekHertz/agent-runtime/issues/10) Bash and whole-tree leases. Leases cover Edit, Write and NotebookEdit only. A write made through Bash is not leased; the M2 query would still report it.
- [#9](https://github.com/DerekHertz/agent-runtime/issues/9) lease time limit and release on a passing build. Leases release on Stop and SessionEnd only.
- [#11](https://github.com/DerekHertz/agent-runtime/issues/11) Defer and [#12](https://github.com/DerekHertz/agent-runtime/issues/12) released-version reads.
- #7's third criterion (a cross-slot happens-before pair through a knowledge channel) moves to rung 7, because M1 runs before any channel exists. Until then every cross-slot pair is concurrent, so the M1 detection is true but weak, and the write-up says so.
- #13 lists #9 and #10 as blockers. Running it without them is Derek's call, recorded here.

### Per-rung process

One writer (Claude Fable, inline, no swarm). Per rung: tests first, the two project verification skills where they apply, then a small PR that merges on green CI. Derek dropped the per-rung Codex review on 2026-11-10; he and Claude walk through every rung together at the end instead. No seal scripts, receipts or evidence audits beyond what those skills already write. The issue #3 review reports in `docs/reviews/` stay as they are; new rungs get a PR description and a walkthrough, not a report packet.

Derek granted a standing "merge on PASS" for this ladder on 2026-11-10: Claude may commit, push, open and merge a rung's PR once CI is green and the rung's check has passed. The grant covers the ladder only, and it replaces the issue #3 merge hold. Derek still starts the two live runs.

The earlier issue #3 breakdown is in the [issue plan](issue-3.md). The live-comparison harness stays [follow-up #20](https://github.com/DerekHertz/agent-runtime/issues/20).

## Triage before adding work

- **Blocking:** a reproduced failure prevents the current issue's acceptance, breaks its correctness or durability, invalidates required verification, or fails required CI. Fix the smallest cause now with Claude, independently recheck it, and record the failed check and repair.
- **Nonblocking:** an improvement, maintenance warning, unmeasured scaling concern, or later-milestone work. Create or reuse a GitHub issue, record why it is nonblocking, link it from the scope record, and defer implementation.
- **Uncertain:** run the smallest check that determines whether it blocks acceptance. Do not turn the investigation into another feature.

Each PR's Scope section names the acceptance criterion it advances and links deferred findings. No unrelated cleanup or convenience tooling is bundled because it was discovered during review. A human status question or workflow comment is not code approval.

## Deferred findings

- [#20: Matched dogfooding comparison](https://github.com/DerekHertz/agent-runtime/issues/20). No comparison has run; preserve explicit `not run` records.
- [#21: Checkout-action runtime maintenance](https://github.com/DerekHertz/agent-runtime/issues/21). CI currently passes; the annotation is maintenance work.
- [#22: Measure state-copy growth](https://github.com/DerekHertz/agent-runtime/issues/22). The cost is unmeasured; profile before optimizing or claiming a regression.
