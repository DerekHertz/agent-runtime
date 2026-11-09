#!/usr/bin/env python3
"""M0 probe hook. Logs every hook invocation and optionally sleeps, denies,
redirects a Read, or blocks Stop, according to <run_dir>/config.json.

Usage (from a settings file): python3 -I probe_hook.py <run_dir>

The config is read from a file named on the command line, not from the
environment, so the env-inheritance measurement doesn't affect behaviour.
"""
import json
import os
import sys
import time

run_dir = sys.argv[1]
t_start = time.time()
raw = sys.stdin.read()
try:
    inp = json.loads(raw)
except json.JSONDecodeError:
    inp = {"_unparsed": raw}

try:
    with open(os.path.join(run_dir, "config.json")) as f:
        cfg = json.load(f)
except FileNotFoundError:
    cfg = {}

event = inp.get("hook_event_name", "?")
tool = inp.get("tool_name")
tool_input = inp.get("tool_input") or {}
out = None
action = "none"


def matches(rule_tool):
    return rule_tool == "*" or rule_tool == tool


if event == "PreToolUse":
    for rule_tool, secs in cfg.get("pre_sleep", {}).items():
        if matches(rule_tool):
            time.sleep(secs)
            action = f"slept {secs}s"
    for rule_tool, reason in cfg.get("pre_deny", {}).items():
        if matches(rule_tool):
            out = {"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason}}
            action = "deny"
    redirect = cfg.get("read_redirect")
    if redirect and tool == "Read" and str(tool_input.get("file_path", "")).endswith(redirect["suffix"]):
        new_input = dict(tool_input)
        new_input["file_path"] = redirect["to"]
        out = {"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "M0 probe: redirected to released snapshot",
            "updatedInput": new_input}}
        action = f"redirect -> {redirect['to']}"

if event == "Stop" and cfg.get("stop_block_max"):
    counter = os.path.join(run_dir, "stop_count")
    n = int(open(counter).read()) if os.path.exists(counter) else 0
    if n < cfg["stop_block_max"]:
        with open(counter, "w") as f:
            f.write(str(n + 1))
        out = {"decision": "block", "reason": cfg.get("stop_reason", "Continue.")}
        action = f"stop-block #{n + 1}"

record = {
    "t_start": t_start,
    "t_end": time.time(),
    "pid": os.getpid(),
    "event": event,
    "tool": tool,
    "tool_use_id": inp.get("tool_use_id"),
    "session_id": inp.get("session_id"),
    "env_slot": os.environ.get("AGENT_RUNTIME_SLOT"),
    "env_project_dir": os.environ.get("CLAUDE_PROJECT_DIR"),
    "action": action,
    "stdin": inp,
}
with open(os.path.join(run_dir, "hook-log.jsonl"), "a") as f:
    f.write(json.dumps(record) + "\n")

if out is not None:
    print(json.dumps(out))
sys.exit(0)
