# Hook input fixtures

Hook JSON that the tests and the verify journeys pipe into `art-shim`. Observed fixtures come from the M0 hook probe (Claude Code 2.1.318, `bench/m0/runs/*/hook-log.jsonl`, gitignored). Their local paths are shortened to `/work` and the transcript path is anonymized. The probe's logger stored parsed JSON, so each observed fixture is a compact re-serialization with the key order kept, not the original stdin bytes.

| Fixture | Kind | Source |
| --- | --- | --- |
| `pre_tool_use_edit.json` | observed | M0 e2, PreToolUse Edit |
| `pre_tool_use_bash.json` | observed | M0 e5, PreToolUse Bash |
| `pre_tool_use_read.json` | observed | M0 e2, PreToolUse Read |
| `post_tool_use_edit.json` | hand-made | PostToolUse Edit in the M0 shape with a shortened input and response, added in PR #14; no matching recorded event |
| `post_tool_use_failure_bash.json` | observed | M0 e6, PostToolUseFailure Bash (`is_interrupt: false`) |
| `stop.json` | observed | M0 e2, Stop (`stop_hook_active: false`, `last_assistant_message: "done"`) |
| `session_end.json` | observed | M0 e2, SessionEnd (`reason: "other"`) |
| `pre_tool_use_write_synthetic.json` | **synthetic** | Written by hand: no Write call was recorded in M0 |
| `pre_tool_use_notebook_edit_synthetic.json` | **synthetic** | Written by hand: no NotebookEdit call was recorded in M0 |

The synthetic fixtures follow the tool input shapes in the Claude Code tools reference. They show that the runtime records those tools' raw inputs. They are not evidence of what a live session sends. Tests that edit a fixture's fields by text (an interrupted failure, an omitted flag, a changed input) are synthetic variants too.
