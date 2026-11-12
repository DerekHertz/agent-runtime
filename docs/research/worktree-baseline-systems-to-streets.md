# The worktree baseline in the field: rebase conflicts in Systems to Streets

This is an observed example of SPEC section 1, problem 1 ("a merge conflict surfaces after both have moved on") and of the baseline in SPEC section 4, where each agent has its own git worktree and conflicts are found at merge. The runtime was not involved. It was a real night of work, not a fixture.

## The work

On 2026-11-12, between 01:38 and 07:34 UTC, one Claude Code session (the lead) ran Sonnet builders on [DerekHertz/systems-to-streets](https://github.com/DerekHertz/systems-to-streets), a TypeScript repo. Each builder had its own git worktree and branch and built one GitHub issue into one PR. The lead sent each PR to Codex for review and merged it when CI was green. Usually about two builders ran at once, at most four. 26 PRs merged in about five hours.

When a PR merged, every open PR behind it had to rebase onto the new `main`. The lead asked each builder to do this with a message, and the builder resolved any conflicts in its own worktree.

The numbers come from the lead's transcript and the 24 builder transcripts, parsed after the run.

## The conflicts

Six builder rebases hit textual conflicts. Five came from builders running in parallel on the page layer (`src/page/main.ts`, `review-setup.ts`, `server.ts`, `cli.ts`).

| # | Time (UTC) | Rebasing PR vs merged PR | Where | What each side did |
| --- | --- | --- | --- | --- |
| 1 | 02:18 | 254 vs 251 | `main.ts` imports | 254 moved a block of setup code into a new `review-setup.ts`; 251 added an import and a field in the code that moved |
| 2 | 02:23 | 252 vs 248, 254 | imports of `judged-panel.ts`, `review-panel.ts`, `server.ts`; `main.ts` | 248 added gate rates; 252 added polling for updates |
| 3 | 03:03 | 260 vs 256 | `review-setup.ts`, the opening-view block | 256 added a headline strip where 260 added an arrival row |
| 4 | 03:18 | 260 vs 257, 259 | `server.ts` `begin()`, `cli.ts` options | three PRs each added a start-up option; the conflict replayed on two commits |
| 5 | 03:25 | 258 vs 260 | `hover-facts.ts` and its call in `main.ts` | one side turned a field into a function, the other added two fields |
| 6 | 05:15 | 275 vs 274 | `review-panel.ts` imports | 274 moved two helpers into a new module; 275 added an import beside them |

Each one cost between 30 seconds and 4.3 minutes, and up to 16k output tokens, from the lead's message to the builder's push.

**Four of the six were broken in ways git did not report.** Git merged or resolved the text, and then the builder's typecheck or tests failed: in #1 a type change had to be ported by hand to the moved code, #4 failed typecheck (TS2345), #5 failed a test fixture's type, and #2 had to carry a behaviour across two features (inferred from its commit message). Every builder caught its own break before pushing. No fix was silently overridden and CI never failed.

## What the lead did by hand

The lead ordered the work to avoid conflicts, twice:

- **Merge order** (02:03): "I'll merge it first… PRs 251 and 252 then rebase onto it." This failed because 251 merged first, and 254 conflicted anyway. PR 252 waited about 22 minutes behind 254 and still conflicted (#2).
- **Build order** (02:38): "#82, then #73 + #74, wait for #253, because all three change the same test-running code and would conflict in parallel." This held, and those three PRs did not conflict.

The second rule is a lease, enforced by the lead's judgement instead of the coordinator: it held the issue, not a file, for the time the PR was open.

## What this suggests for the runtime

These are readings, not measurements.

- **Leases would have moved the conflicts earlier, not removed them.** In one shared tree, #3, #4 and #6 would each have been a Deny or a Defer at the first edit, instead of a rebase at the end.
- **File leases alone miss the moved-code conflicts.** In #1 and #6, one side moved code into a new file. The other side's lease on the old file says nothing about the new one. A refactor like PR 254 behaves like a write across several files at once, which is close to a whole-tree lease.
- **Most of the work was in the broken-but-merged cases.** A conflict git reports is cheap to resolve; the expensive ones were the four that typechecked wrong after the merge. The knowledge channel for "building or testing the whole tree" is the one that caught them here, as it would under the runtime.
- **The fix the lead adopted afterwards is a barrier.** From the next tier on, one builder per task works on a stack of branches, a review queue runs beside it, and the only wait is a barrier at the end of the tier: every task merged, one review of the whole tier, then the merge to `main`. This is the scatter, barrier, gather shape of SPEC section 6, run by hand.
- **The cost was small per conflict but it adds up.** Six conflicts in 26 PRs, at most about 4 minutes each. The larger cost was the lead keeping builders apart by hand. Two to four builders were in flight instead of more, partly because of the 7 GB machine and partly to avoid these conflicts.

## Limits

- One run, one repo, one lead. The builders' work was chosen by issue, not to make them conflict.
- The conflict count comes from matching text in the transcripts. It counts rebases that conflicted, not conflicts inside each rebase.
- The cost times include whatever else the builder did in the same turn, such as review fixes (#3, #4).
- The transcripts and the audit that parsed them are on Derek's machine and are not committed.
