"""The `leases` journey for verify.py: a write denied for a lease, through the real binaries.

Kept out of verify.py to hold each file under the line cap. `journey(c, h)` gets verify.py's `Ctx`
as `c` and verify.py itself as `h`, for its helpers, so this module imports nothing from it.
"""

STOP = ("--hook-event", "Stop")
FILE = "/work/src/lib.rs"


def journey(c, h):
    s = c.scratch()
    try:
        pre, stop, read = c.fixtures["pre_tool_use_edit"], c.fixtures["stop"], c.fixtures["pre_tool_use_read"]
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
        want = [(1, "slot-1", "BeforeTool", FILE, [1, 0]), (2, "slot-2", "Deny", FILE, [1, 1]),
                (3, "slot-1", "BeforeTool", FILE, [2, 0])]
        c.check("the trace holds the Deny between the holder's two writes, and the denied slot's stamp has "
                "absorbed the holder's clock from when it took the lease", got == want, rows=got)
        if len(rows) == 3:
            c.check("the Deny's payload is the reason the agent was given, and its input blob is the call's raw input",
                    h.parse_json(rows[1]["payload"]) == reason and rows[1]["input_payload"] == h.raw_tool_input(pre),
                    payload=rows[1]["payload"])

        r = c.shim("slot-1-stop", "slot-1", co.socket, h.with_ids(stop, session_id="sess-slot-1"), args=STOP)
        c.check("slot-1's Stop is recorded silently", (r["exit_code"], r["stdout"], r["stderr"]) == (0, "", ""),
                exit_code=r["exit_code"], stderr=r["stderr"])
        r = gate("slot-2-edit-stale", "slot-2", "toolu_lease_b2")
        decision, why = h.deny_reason(r)
        c.check("after the release slot-2's write is denied until it reads the file again",
                r["exit_code"] == 0 and decision == "deny" and why.startswith(f"{FILE} changed since you last read it")
                and "Re-read first" in why, decision=decision, reason=why)
        r = c.shim("slot-2-read", "slot-2", co.socket, h.with_ids(read, session_id="sess-slot-2", tool_use_id="toolu_lease_b3"))
        allowed("slot-2-read", r)
        allowed("slot-2-edit-after-read", gate("slot-2-edit-after-read", "slot-2", "toolu_lease_b4"))
        denied("slot-1-edit-after-release", gate("slot-1-edit-after-release", "slot-1", "toolu_lease_a3"), "slot-2")
        rows = c.rows("released", run)
        got = [(x["kind"], x["tool"], x["vclock"]) for x in rows[3:]]
        want = [("Stop", None, [3, 0]), ("Deny", "Edit", [1, 2]), ("BeforeTool", "Read", [3, 3]),
                ("BeforeTool", "Edit", [3, 4]), ("Deny", "Edit", [4, 4])]
        c.check("the Read absorbs slot-1's clock at its release, so slot-2's write is stamped after all of slot-1's "
                "work; slot-1, denied in turn, absorbs slot-2's lease clock", got == want, rows=got)

        r = c.run("art-denies", [c.bins["art"], "query", run, "denies"])
        c.check("art query denies lists the three Denies with their slots and the file",
                r["exit_code"] == 0 and r["stdout"].splitlines()[-1:] == ["3 denies"]
                and r["stdout"].startswith(f"2 (slot-2)\t{FILE}\t{FILE} is leased by slot-1"),
                exit_code=r["exit_code"], stdout=r["stdout"])
        r = c.run("art-concurrent", [c.bins["art"], "query", run, "concurrent-writes"])
        c.check("with leases and the channels, the two slots' writes to the file are ordered: no concurrent pair",
                r["exit_code"] == 0 and r["stdout"] == "0 concurrent write pairs\n", stdout=r["stdout"])
    finally:
        c.drop_scratch(s)
