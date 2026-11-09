#!/usr/bin/env python3
"""M0 hook probe runner. Runs headless `claude -p --model haiku` sessions against
a fresh copy of bench/toy with probe_hook.py registered on every hook event.

Usage: python3 -I bench/m0/run_m0.py [experiment ...]   (default: all)

Sessions run with a clean environment (HOME, PATH, USER, ...) so they use your
logged-in subscription, exactly as from a terminal. Never pass --bare: it skips
hooks and subscription sign-in. Results go to bench/m0/runs/<experiment>/.
"""
import json
import os
import shutil
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
TOY = os.path.join(HERE, "..", "toy")
HOOK = os.path.join(HERE, "probe_hook.py")
RUNS = os.path.join(HERE, "runs")

LEASE_MSG = ("src/lib.rs is leased by agent slot 2 (intent: 'refactor mean and parse_kv'), "
             "held for 3m. Edits to it are refused until the lease is released. "
             "Work on something else, or try again later.")

EXPERIMENTS = {
    # Q1 (env) and Q4 (parallel PreToolUse): hooks sleep 3 s on Read so overlap is visible.
    "e1_env_parallel": dict(
        prompt=("Read README.md, Cargo.toml and src/lib.rs. Issue all three Read tool calls "
                "in parallel, in a single message. Then reply with just the crate name."),
        config={"pre_sleep": {"Read": 3}}, hook_timeout=60),
    # Q2: a 60 s PreToolUse block on Edit, well inside a 120 s hook timeout.
    "e2_block_60s": dict(
        prompt="In src/lib.rs change the doc comment of fn add to '/// Adds two integers.' using the Edit tool. Reply 'done'.",
        config={"pre_sleep": {"Edit": 60}}, hook_timeout=120),
    # Q3: the hook sleeps 20 s and would then deny, but its timeout is 5 s. Does the edit go through?
    "e3_timeout_fail_open": dict(
        prompt="In src/lib.rs change the doc comment of fn add to '/// Adds two integers.' using the Edit tool. Reply 'done'.",
        config={"pre_sleep": {"Edit": 20}, "pre_deny": {"Edit": "M0: should never be seen if the timeout fails open"}},
        hook_timeout=5),
    # Q5: Stop is blocked up to 20 times. How many continuations happen before the session ends?
    "e5_stop_cap": dict(
        prompt="Reply with the single word 'hi'.",
        config={"stop_block_max": 20, "stop_reason": "Barrier not released yet. Reply with the single word 'again'."},
        hook_timeout=60),
    # Q6: every Edit/Write is denied with a lease message. What does the model do?
    "e6_deny_behavior": dict(
        prompt="The test mean_works fails. Fix the bug in fn mean in src/lib.rs, then run cargo test.",
        config={"pre_deny": {"Edit": LEASE_MSG, "Write": LEASE_MSG}}, hook_timeout=60),
    # Q7: Read of src/lib.rs redirected via updatedInput to a snapshot containing a marker.
    "e7_read_redirect": dict(
        prompt="Read src/lib.rs and quote its first line exactly, inside backticks.",
        config="read_redirect", hook_timeout=60),
}


def settings_for(run_dir, timeout):
    cmd = f"python3 -I '{HOOK}' '{run_dir}'"
    entry = {"type": "command", "command": cmd, "timeout": timeout}
    hooks = {}
    for ev in ["PreToolUse", "PostToolUse", "PostToolUseFailure"]:
        hooks[ev] = [{"matcher": "*", "hooks": [entry]}]
    for ev in ["Stop", "SessionStart", "SessionEnd"]:
        hooks[ev] = [{"hooks": [entry]}]
    return {"hooks": hooks}


def run(name, spec):
    run_dir = os.path.join(RUNS, name)
    shutil.rmtree(run_dir, ignore_errors=True)
    work = os.path.join(run_dir, "work")
    shutil.copytree(TOY, work, ignore=shutil.ignore_patterns("target", "Cargo.lock"))

    config = spec["config"]
    if config == "read_redirect":
        snap = os.path.join(work, ".art", "snapshots", "lib.rs")
        os.makedirs(os.path.dirname(snap))
        with open(snap, "w") as f:
            f.write("// SNAPSHOT-MARKER-7731 (released version)\n")
            f.write(open(os.path.join(work, "src", "lib.rs")).read())
        config = {"read_redirect": {"suffix": "src/lib.rs", "to": snap}}
    with open(os.path.join(run_dir, "config.json"), "w") as f:
        json.dump(config, f)
    settings = os.path.join(run_dir, "settings.json")
    with open(settings, "w") as f:
        json.dump(settings_for(run_dir, spec["hook_timeout"]), f, indent=1)

    session_id = str(uuid.uuid4())
    env = {k: os.environ[k] for k in ("HOME", "PATH", "USER", "LOGNAME", "SHELL", "LANG", "TMPDIR") if k in os.environ}
    env["AGENT_RUNTIME_SLOT"] = "slot-1"
    cmd = ["claude", "-p", spec["prompt"], "--model", "haiku", "--settings", settings,
           "--session-id", session_id, "--permission-mode", "acceptEdits",
           "--allowedTools", "Read,Edit,Write,Bash(cargo test:*)", "--output-format", "json"]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, cwd=work, env=env, capture_output=True, text=True, timeout=600)
        rc, stdout, stderr = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired as e:
        rc, stdout, stderr = "timeout", e.stdout or "", e.stderr or ""
    wall = time.time() - t0
    for fname, data in (("stdout.json", stdout), ("stderr.txt", stderr)):
        with open(os.path.join(run_dir, fname), "w") as f:
            f.write(data if isinstance(data, str) else data.decode())

    # Copy the transcript the hooks reported.
    log = os.path.join(run_dir, "hook-log.jsonl")
    if os.path.exists(log):
        for line in open(log):
            tp = json.loads(line)["stdin"].get("transcript_path")
            if tp and os.path.exists(tp):
                shutil.copy(tp, os.path.join(run_dir, "transcript.jsonl"))
                break
    with open(os.path.join(run_dir, "meta.json"), "w") as f:
        json.dump({"rc": rc, "wall_s": round(wall, 1), "session_id": session_id, "cmd": cmd}, f, indent=1)
    print(f"{name}: rc={rc} wall={wall:.1f}s", flush=True)


if __name__ == "__main__":
    names = sys.argv[1:] or list(EXPERIMENTS)
    for n in names:
        run(n, EXPERIMENTS[n])
