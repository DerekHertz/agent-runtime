"""The `launcher` journey for verify.py: `art run` with a stand-in agent, then `art query`.

Kept out of verify.py to hold each file under the line cap. `journey(c, h)` gets verify.py's `Ctx`
as `c` and verify.py itself as `h`, for its helpers, so this module imports nothing from it.
"""
import json
import pathlib

FAKE_AGENT = pathlib.Path(__file__).resolve().parent / "fake_agent.py"
SLOTS = ["slot-1", "slot-2"]
LEAKS = {"ANTHROPIC_API_KEY": "sk-not-a-real-key", "CLAUDECODE": "1", "CLAUDE_CODE_ENTRYPOINT": "cli"}


def journey(c, h):
    s = c.scratch()
    try:
        run_file, runs = s / "run.json", s / "runs"
        run_file.write_text(json.dumps({
            "target": str(h.REPO / "bench/toy"), "agent": str(FAKE_AGENT), "gate_timeout_s": 75,
            "slots": [{"slot": slot, "prompt": f"edit add as {slot}"} for slot in SLOTS]}))
        r = c.run("art-run", [c.bins["art"], "run", run_file, "--run-id", "r1", "--runs-dir", runs], env=LEAKS, timeout=120)
        c.check("art run exits 0 and reports a clean coordinator shutdown",
                r["exit_code"] == 0 and "coordinator shut down cleanly: true" in r["stdout"],
                exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])
        run = runs / "r1"

        settings = h.parse_json((run / "settings.json").read_text()) if (run / "settings.json").is_file() else {}
        hooks = settings.get("hooks", {})
        want = {"PreToolUse": 75, "PostToolUse": 30, "PostToolUseFailure": 30, "Stop": 30, "SessionEnd": 30}
        got = {event: entries[0]["hooks"][0].get("timeout") for event, entries in hooks.items()}
        commands_ok = all(entries[0]["hooks"][0]["command"] == f"'{c.bins['art-shim']}' --hook-event {event}"
                          for event, entries in hooks.items())
        c.check("the generated settings register the shim on the five hook events with their timeouts",
                got == want and commands_ok, timeouts=got)

        sessions = []
        for slot in SLOTS:
            out = run / "sessions" / slot / "stdout.json"
            sessions.append(h.parse_json(out.read_text()) if out.is_file() else None)
        c.check("each session's stdout is kept in the run directory", all(sessions), found=[bool(x) for x in sessions])
        if all(sessions):
            c.check("each session ran in the run's copy of the target, which has its source and no build output",
                    all(x["cwd"] == str((run / "work").resolve()) for x in sessions)
                    and (run / "work/src/lib.rs").is_file() and not (run / "work/target").exists(),
                    cwds=[x["cwd"] for x in sessions])
            c.check("each session got its own slot, the run's socket, and none of the parent's key or Claude Code variables",
                    [x["slot"] for x in sessions] == SLOTS
                    and all("AGENT_RUNTIME_SOCKET" in x["env"] and not set(x["env"]) & set(LEAKS) for x in sessions),
                    env=[x["env"] for x in sessions])
            c.check("every registered hook ran the shim and succeeded, and the gate allowed the call",
                    all(k["exit_code"] == 0 for x in sessions for k in x["hooks"])
                    and all(h.parse_json(x["hooks"][0]["stdout"]) == h.ALLOW for x in sessions),
                    hooks=[[(k["event"], k["exit_code"], k["stderr"]) for k in x["hooks"]] for x in sessions])
            c.check("the session was started headless with a fixed session id and never --bare",
                    all(x["argv"][0] == "-p" and "--session-id" in x["argv"] and "--bare" not in x["argv"] for x in sessions))

        rows = c.rows("launcher", run)
        per_slot = {slot: [x["kind"] for x in rows if x["slot"] == slot] for slot in SLOTS}
        c.check("both sessions' hook events are in one trace, each slot's in order",
                all(kinds == ["BeforeTool", "AfterTool", "Stop", "SessionEnd"] for kinds in per_slot.values())
                and len(rows) == 8, kinds=per_slot)
        summary = h.parse_json((run / "run.json").read_text()) if (run / "run.json").is_file() else {}
        c.check("run.json records each session's exit and the clean shutdown",
                summary.get("coordinator_clean_shutdown") is True
                and [(x["slot"], x["exit"]) for x in summary.get("sessions", [])] == [(slot, "0") for slot in SLOTS],
                summary=summary)

        r = c.run("art-query", [c.bins["art"], "query", run, "concurrent-writes"])
        before = {x["slot"]: x["event_id"] for x in rows if x["kind"] == "BeforeTool"}
        first, second = sorted(before.items(), key=lambda kv: kv[1]) if len(before) == 2 else (("?", 0), ("?", 0))
        c.check("art query reports the two slots' concurrent edits of one file",
                r["exit_code"] == 0 and r["stdout"] == f"/work/src/lib.rs\t{first[1]} ({first[0]})\t{second[1]} ({second[0]})\n"
                "1 concurrent write pairs\n", exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])

        r = c.run("art-run-again", [c.bins["art"], "run", run_file, "--run-id", "r1", "--runs-dir", runs], timeout=60)
        c.check("a second run with the same run id is refused: exit 2, the trace unchanged",
                r["exit_code"] == 2 and "already exists" in r["stderr"] and len(c.rows("launcher-again", run)) == 8,
                exit_code=r["exit_code"], stderr=r["stderr"])
    finally:
        c.drop_scratch(s)
