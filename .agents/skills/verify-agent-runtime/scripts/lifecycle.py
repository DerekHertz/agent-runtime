"""The `lifecycle` journey for verify.py: Stop and SessionEnd reports through the real binaries.

Kept out of verify.py to hold each file under the line cap. `journey(c, h)` gets verify.py's `Ctx`
as `c` and verify.py itself as `h`, for its helpers, so this module imports nothing from it.
"""

STOP = ("--hook-event", "Stop")
END = ("--hook-event", "SessionEnd")
REPORT = ("--hook-event", "PostToolUse")
SESS, NEXT = "sess-verify-lifecycle", "sess-verify-lifecycle-2"
CALL = "toolu_life_a"


def silent(r):
    return (r["exit_code"], r["stdout"], r["stderr"]) == (0, "", "")


def report_failed(r):
    """A report that could not be made: nonblocking exit 1, nothing on stdout, a reason on stderr."""
    return r["exit_code"] == 1 and r["stdout"] == "" and r["stderr"].startswith("agent-runtime:")


def journey(c, h):
    s = c.scratch()
    try:
        pre, post = c.fixtures["pre_tool_use_edit"], c.fixtures["post_tool_use_edit"]
        stop, end = c.fixtures["stop"], c.fixtures["session_end"]
        co = c.start("a", s, "r1", h.SLOTS)
        run = s / "runs/r1"

        def report(label, slot, stdin, args):
            r = c.shim(label, slot, co.socket, stdin, args=args)
            c.check(f"{label}: exit 0, no stdout, no stderr", silent(r),
                    exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])

        r = c.shim("before-a", "slot-1", co.socket, h.with_ids(pre, session_id=SESS, tool_use_id=CALL))
        c.check("before-a: allowed", r["exit_code"] == 0 and h.parse_json(r["stdout"]) == h.ALLOW,
                exit_code=r["exit_code"], stdout=r["stdout"])
        report("stop", "slot-1", h.with_ids(stop, session_id=SESS), STOP)
        report("session-end", "slot-1", h.with_ids(end, session_id=SESS), END)
        # The call was still open when its session ended: its completion pairs all the same.
        report("late-completion", "slot-1", h.with_ids(post, session_id=SESS, tool_use_id=CALL), REPORT)
        r = c.shim("ended-session-other-slot", "slot-2", co.socket, h.with_ids(stop, session_id=SESS), args=STOP)
        c.check("the ended session cannot report from another slot: exit 1 naming its slot",
                report_failed(r) and "slot-1" in r["stderr"], exit_code=r["exit_code"], stderr=r["stderr"])
        report("new-session-stop", "slot-1", h.with_ids(stop, session_id=NEXT), STOP)

        rows = c.rows("lifecycle", run)
        got = [(x["event_id"], x["kind"], x["session_id"], x["tool_use_id"], x["before_event_id"], x["vclock"])
               for x in rows]
        want = [(1, "BeforeTool", SESS, CALL, None, [1, 0]),
                (2, "Stop", SESS, None, None, [2, 0]),
                (3, "SessionEnd", SESS, None, None, [3, 0]),
                (4, "AfterTool", SESS, CALL, 1, [4, 0]),
                (5, "Stop", NEXT, None, None, [5, 0])]
        c.check("Stop and SessionEnd are rows of their slot, a late completion still pairs, the refused report "
                "left no row, and a new session continues slot-1's clock", got == want, rows=got)
        if len(rows) == 5:
            c.check("each lifecycle row keeps exactly the hook's lifecycle fields, raw",
                    [h.parse_json(rows[i]["payload"]) for i in (1, 2, 4)]
                    == [{"stop_hook_active": False, "last_assistant_message": "done"}, {"reason": "other"},
                        {"stop_hook_active": False, "last_assistant_message": "done"}],
                    payloads=[rows[i]["payload"] for i in (1, 2, 4)])

        # A lifecycle report that cannot be made never blocks Stop: exit 1, not 2, and no row.
        bad = [("stop-flag-not-boolean", stop.replace('"stop_hook_active":false', '"stop_hook_active":"no"'), STOP),
               ("end-reason-not-string", end.replace('"reason":"other"', '"reason":1'), END),
               ("malformed-json", "not json", STOP),
               ("session-end-under-stop", end, STOP)]
        for label, stdin, args in bad:
            r = c.shim(label, "slot-1", co.socket, stdin, args=args)
            c.check(f"{label}: exit 1, empty stdout, a reason on stderr", report_failed(r),
                    exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])
        r = c.run("outside-run", [c.bins["art-shim"], *STOP], env={}, stdin=stop)
        c.check("outside a run a Stop report is silent", silent(r), exit_code=r["exit_code"], stdout=r["stdout"])
        c.check("none of those reached the trace", len(c.rows("lifecycle-refusals", run)) == 5)
        co.stop()
        r = c.shim("coordinator-gone", "slot-1", co.socket, stop, args=STOP)
        c.check("coordinator gone: a Stop report exits 1, not 2", report_failed(r),
                exit_code=r["exit_code"], stderr=r["stderr"])
    finally:
        c.drop_scratch(s)
