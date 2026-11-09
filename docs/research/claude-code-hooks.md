# Claude Code hook facts relevant to the shim

Gathered 2026-11-08 from code.claude.com/docs/en/{hooks,headless,tools-reference,cli-reference}.md.
Items marked (unverified) came from a summarizing read of the hooks page; confirm against the raw page before depending on them in code.

## Timeouts and failure mode
- `command` hooks: per-hook `timeout` (seconds), default 600 s. Max not documented. (unverified)
- On timeout the hook is cancelled and **the tool call proceeds** (fails open), unless `onFailure: "block"` is set. (unverified)
- Non-zero exit other than 2 is a non-blocking error: the tool proceeds.
- **Implication:** a crashed or slow shim silently drops coherence. The shim must deny on its own internal errors (exit 2), and Defer waits must stay well under the hook timeout.

## PreToolUse
- Output: `hookSpecificOutput.permissionDecision` = allow | deny | ask | defer, plus `permissionDecisionReason`. A deny reason is shown to the model.
- Exit 2 blocks the call, and stderr becomes the reason.
- `hookSpecificOutput.updatedInput` replaces the whole tool input. This is the hook for transparent remote tool invocation (rewriting a Bash command). (unverified)

## PostToolUse / PostToolUseFailure
- PostToolUse fires only on success. **PostToolUseFailure** is a separate event for failed calls (`error`, `is_interrupt`). The shim must hook both, or AfterTool is missed on failures.
- Stdin includes `tool_name`, `tool_input`, `tool_use_id`, `tool_response`.
- `hookSpecificOutput.additionalContext` is shown to the model next to the tool result.
- PostToolBatch fires after a batch of parallel tool calls resolves. (unverified)

## Stop / SessionEnd / SessionStart
- A Stop hook can block stopping (exit 2 or `decision: "block"` + `reason`). The reason becomes Claude's next instruction. `stop_hook_active` guards loops, and at most **8 consecutive continuations** are allowed.
- SessionEnd cannot block. Its budget is 1.5 s by default, up to 60 s. It runs on SIGTERM in headless mode.
- SessionStart can inject `additionalContext` but cannot block.

## Identity and environment
- Common stdin fields: `session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`, `tool_use_id` (on tool events).
- Hooks inherit the `claude` process environment (except OTEL_*), so launcher-set env vars reach the shim. (unverified wording)
- `CLAUDE_PROJECT_DIR` is set.

## Headless launching
- `claude -p "<prompt>" --session-id <uuid> --settings <file>` is documented. Hooks from `--settings` do not run under `--bare`.
- In manual permission mode, calls that need approval are denied unless approved by a hook or `--permission-prompt-tool`.

## Parallelism
- Matching hooks for one event run in parallel.
- Whether PreToolUse hooks for **parallel tool calls in one turn** run concurrently is not documented. Assume they do: two shim processes for one agent can be in flight at once.

## Tools
- Mutation: `Edit`, `Write`, `NotebookEdit`. **`MultiEdit` is no longer a current tool.**
- Read: `Read`. `Grep` and `Glob` are absent by default on macOS and Linux; Claude uses Bash `grep`/`find` instead.
- Edit does **not** refuse a file changed since the last read. It only needs `old_string` to match the current content. Newer models can Edit or Write an unread existing file. Claude Code does not enforce "read before write"; the coordinator must.
- Bash default timeout is 2 min, max 10 min (10 min default in headless). A foreground command over its timeout is moved to the background.
