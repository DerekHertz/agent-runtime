# MVP north star and execution boundary

The product promise is coherent parallel coding on one shared repository: Claude Code sessions can work without silently overwriting each other, and the trace explains what each agent knew. Follow the guarantees and milestones in [SPEC](../SPEC.md); this document does not expand them.

The first product proof is the M1/M2 pair: detect a same-path conflict in a deliberately conflicting run, then repeat that workload with leases and show zero such conflicts plus the Deny that prevented them. Collectives, the viewer and benchmarks remain their later spec milestones.

## Current goal

Finish [issue #3](https://github.com/DerekHertz/agent-runtime/issues/3), the tracing foundation on the M1 path. Deliver its five acceptance criteria through six focused runtime PRs, with independent execution verification, required CI and review reports ready for Derek's joint review. Keep the issue open and every PR unmerged until that review authorizes merging.

See the [issue plan](issue-3.md) for the active scope. The live-comparison harness is [follow-up #20](https://github.com/DerekHertz/agent-runtime/issues/20), not a seventh tracing PR. Existing launcher, M1 demonstration and lease-comparison work remains in #6, #7 and #13.

## Triage before adding work

- **Blocking:** a reproduced failure prevents the current issue's acceptance, breaks its correctness or durability, invalidates required verification, or fails required CI. Fix the smallest cause now with Claude, independently recheck it, and record the failed check and repair.
- **Nonblocking:** an improvement, maintenance warning, unmeasured scaling concern, or later-milestone work. Create or reuse a GitHub issue, record why it is nonblocking, link it from the scope record, and defer implementation.
- **Uncertain:** run the smallest check that determines whether it blocks acceptance. Do not turn the investigation into another feature.

Each PR's Scope section names the acceptance criterion it advances and links deferred findings. No unrelated cleanup or convenience tooling is bundled because it was discovered during review. A human status question or workflow comment is not code approval.

## Deferred findings

- [#20: Matched dogfooding comparison](https://github.com/DerekHertz/agent-runtime/issues/20). No comparison has run; preserve explicit `not run` records.
- [#21: Checkout-action runtime maintenance](https://github.com/DerekHertz/agent-runtime/issues/21). CI currently passes; the annotation is maintenance work.
- [#22: Measure state-copy growth](https://github.com/DerekHertz/agent-runtime/issues/22). The cost is unmeasured; profile before optimizing or claiming a regression.
