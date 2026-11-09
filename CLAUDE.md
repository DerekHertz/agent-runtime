# Claude Code project instructions

@AGENTS.md

## Completion handoff

At task completion, invoke Codex through Bash from the project root; do not merely recommend a review. For an entirely uncommitted task run `codex review --uncommitted`. For a task branch run `codex review --base <actual-base>`, or use `codex review --commit <sha>` for an explicitly selected commit. Inspect `codex review --help` before combining options. If committed changes coexist with uncommitted repairs, review both scopes explicitly unless coverage of both is verified.

Capture and read the actual findings, using ignored `.claude/reviews/` files when needed. Follow the review/repair loop in AGENTS.md, with at most three repair rounds. If Codex is unavailable, report that independent review is incomplete; do not substitute self-review. Do not edit while Codex is reviewing.

Rerun setup in a new checkout or worktree so its local `autoMemoryDirectory` points to that checkout's `.agents/memory`, not another branch's memory.
