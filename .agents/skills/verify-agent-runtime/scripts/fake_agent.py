#!/usr/bin/env python3
"""A stand-in for `claude` in the `launcher` journey. It spends no subscription usage.

`art run` starts it exactly as it would start Claude Code. It reads the `--settings` file the
launcher generated and runs the registered hook command for each event of one Edit call, the way
Claude Code would: PreToolUse, PostToolUse, Stop, SessionEnd. Each hook's stdin is a recorded
fixture with this session's id. It prints what it saw as JSON on stdout for the journey to check.
"""
import json
import os
import pathlib
import subprocess
import sys

FIXTURES = pathlib.Path(__file__).resolve().parents[4] / "crates/shim/tests/fixtures"
EVENTS = [("PreToolUse", "pre_tool_use_edit"), ("PostToolUse", "post_tool_use_edit"),
          ("Stop", "stop"), ("SessionEnd", "session_end")]


def arg(name):
    return sys.argv[sys.argv.index(name) + 1]


def main():
    settings = json.loads(pathlib.Path(arg("--settings")).read_text())
    session_id, slot = arg("--session-id"), os.environ.get("AGENT_RUNTIME_SLOT")
    hooks = []
    for event, fixture in EVENTS:
        hook = json.loads((FIXTURES / f"{fixture}.json").read_text())
        hook["session_id"] = session_id
        if "tool_use_id" in hook:
            hook["tool_use_id"] = f"toolu_fake_{slot}"
        command = settings["hooks"][event][0]["hooks"][0]["command"]
        r = subprocess.run(command, shell=True, input=json.dumps(hook), capture_output=True, text=True)
        hooks.append({"event": event, "exit_code": r.returncode, "stdout": r.stdout, "stderr": r.stderr})
    json.dump({"argv": sys.argv[1:], "cwd": os.getcwd(), "env": sorted(os.environ), "slot": slot,
               "session_id": session_id, "hooks": hooks}, sys.stdout)


if __name__ == "__main__":
    main()
