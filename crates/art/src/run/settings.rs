//! The Claude Code settings file that registers the shim on every hook event of a run
//! (`docs/hook-registration.md`).

use std::path::Path;

use serde_json::{json, Value};

/// The tools the gate and the tool reports are registered for.
const TOOL_MATCHER: &str = "Edit|Write|NotebookEdit|Read|Bash";

/// The timeout of every report hook. A report is bounded at 10 s inside the shim, and Claude
/// Code's 1.5 s default for SessionEnd would kill it first.
pub const REPORT_TIMEOUT_S: u64 = 30;

/// The settings JSON registering `shim` for the five hook events. The gate gets
/// `gate_timeout_s`; every report gets [`REPORT_TIMEOUT_S`].
pub fn settings(shim: &Path, gate_timeout_s: u64) -> Value {
    let hook = |event: &str, timeout: u64| {
        json!({
            "type": "command",
            "command": format!("{} --hook-event {event}", shell_quote(&shim.to_string_lossy())),
            "timeout": timeout,
        })
    };
    let tool = |event: &str, timeout: u64| json!([{ "matcher": TOOL_MATCHER, "hooks": [hook(event, timeout)] }]);
    let lifecycle = |event: &str| json!([{ "hooks": [hook(event, REPORT_TIMEOUT_S)] }]);
    json!({
        "hooks": {
            "PreToolUse": tool("PreToolUse", gate_timeout_s),
            "PostToolUse": tool("PostToolUse", REPORT_TIMEOUT_S),
            "PostToolUseFailure": tool("PostToolUseFailure", REPORT_TIMEOUT_S),
            "Stop": lifecycle("Stop"),
            "SessionEnd": lifecycle("SessionEnd"),
        }
    })
}

/// `text` as one single-quoted shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hook_event_runs_the_shim_named_for_it_with_its_timeout() {
        let s = settings(Path::new("/opt/my tools/art-shim"), 90);
        let hooks = s["hooks"].as_object().unwrap();
        let mut events: Vec<&str> = hooks.keys().map(String::as_str).collect();
        events.sort_unstable();
        assert_eq!(
            events,
            [
                "PostToolUse",
                "PostToolUseFailure",
                "PreToolUse",
                "SessionEnd",
                "Stop"
            ]
        );
        for (event, registrations) in hooks {
            let entry = &registrations[0];
            let hook = &entry["hooks"][0];
            assert_eq!(
                hook["command"],
                format!("'/opt/my tools/art-shim' --hook-event {event}")
            );
            let tool_event = event.contains("ToolUse");
            assert_eq!(entry.get("matcher").is_some(), tool_event, "{event}");
            let want = if event == "PreToolUse" {
                90
            } else {
                REPORT_TIMEOUT_S
            };
            assert_eq!(hook["timeout"], want, "{event}");
        }
    }
}
