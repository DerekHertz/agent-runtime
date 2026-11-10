"""The `leases` journey for verify.py: a write denied for a lease, through the real binaries.

Kept out of verify.py to hold each file under the line cap. `journey(c, h)` gets verify.py's `Ctx`
as `c` and verify.py itself as `h`, for its helpers, so this module imports nothing from it.
"""

STOP = ("--hook-event", "Stop")
FILE = "/work/src/lib.rs"


def journey(c, h):
    s = c.scratch()
    try:
        pre, stop = c.fixtures["pre_tool_use_edit"], c.fixtures["stop"]
        co = c.start("a", s, "r1", h.SLOTS, extra=("--leases",))
        run = s / "runs/r1"

        def gate(label, slot, call):
            return c.shim(label, slot, co.socket, h.with_ids(pre, session_id=f"sess-{slot}", tool_use_id=call))

        def allowed(label, r):
            c.check(f"{label}: allowed", r["exit_code"] == 0 and h.parse_json(r["stdout"]) == h.ALLOW,
                    exit_code=r["exit_code"], stdout=r["stdout"])

        def denied(label, r, holder):
            decision, reason = h.deny_reason(r)
            c.check(f"{label}: a deny decision on stdout, exit 0, naming the file, {holder} and the lease age",
                    r["exit_code"] == 0 and decision == "deny"
                    and reason.startswith(f"{FILE} is leased by {holder} (no stated intent), held for 0m 0"),
                    exit_code=r["exit_code"], decision=decision, reason=reason)
            return reason

        allowed("slot-1-edit", gate("slot-1-edit", "slot-1", "toolu_lease_a"))
        reason = denied("slot-2-edit", gate("slot-2-edit", "slot-2", "toolu_lease_b"), "slot-1")
        allowed("slot-1-edit-again", gate("slot-1-edit-again", "slot-1", "toolu_lease_a2"))
        rows = c.rows("denied", run)
        got = [(x["event_id"], x["slot"], x["kind"], x["path"], x["vclock"]) for x in rows]
        want = [(1, "slot-1", "BeforeTool", FILE, [1, 0]), (2, "slot-2", "Deny", FILE, [0, 1]),
                (3, "slot-1", "BeforeTool", FILE, [2, 0])]
        c.check("the trace holds the Deny between the holder's two writes, each event with its file",
                got == want, rows=got)
        if len(rows) == 3:
            c.check("the Deny's payload is the reason the agent was given, and its input blob is the call's raw input",
                    h.parse_json(rows[1]["payload"]) == reason and rows[1]["input_payload"] == h.raw_tool_input(pre),
                    payload=rows[1]["payload"])

        r = c.shim("slot-1-stop", "slot-1", co.socket, h.with_ids(stop, session_id="sess-slot-1"), args=STOP)
        c.check("slot-1's Stop is recorded silently", (r["exit_code"], r["stdout"], r["stderr"]) == (0, "", ""),
                exit_code=r["exit_code"], stderr=r["stderr"])
        allowed("slot-2-edit-after-release", gate("slot-2-edit-after-release", "slot-2", "toolu_lease_b2"))
        denied("slot-1-edit-after-release", gate("slot-1-edit-after-release", "slot-1", "toolu_lease_a3"), "slot-2")
        kinds = [x["kind"] for x in c.rows("released", run)]
        c.check("after the release the lease passes to slot-2 and slot-1 is the one denied",
                kinds == ["BeforeTool", "Deny", "BeforeTool", "Stop", "BeforeTool", "Deny"], kinds=kinds)

        r = c.run("art-denies", [c.bins["art"], "query", run, "denies"])
        c.check("art query denies lists both Denies with their slots and the file",
                r["exit_code"] == 0 and r["stdout"].splitlines()[-1:] == ["2 denies"]
                and r["stdout"].startswith(f"2 (slot-2)\t{FILE}\t{FILE} is leased by slot-1"),
                exit_code=r["exit_code"], stdout=r["stdout"])
        r = c.run("art-concurrent", [c.bins["art"], "query", run, "concurrent-writes"])
        c.check("a denied write is not a write: the pairs listed are only between admitted writes",
                r["exit_code"] == 0 and "\t2 (" not in r["stdout"] and "\t6 (" not in r["stdout"],
                stdout=r["stdout"])
    finally:
        c.drop_scratch(s)
