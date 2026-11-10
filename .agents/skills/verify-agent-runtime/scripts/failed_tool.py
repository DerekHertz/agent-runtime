"""The `failed-tool` journey for verify.py: PostToolUseFailure reports through the real binaries.

Kept out of verify.py to hold each file under the line cap. `journey(c, h)` gets verify.py's `Ctx`
as `c` and verify.py itself as `h`, for its helpers, so this module imports nothing from it.
"""
import json
import os
import stat

FAILED = ("--hook-event", "PostToolUseFailure")
REPORT = ("--hook-event", "PostToolUse")
SESS = "sess-verify-failure"


def silent(r):
    return (r["exit_code"], r["stdout"], r["stderr"]) == (0, "", "")


def report_failed(r):
    """A report that could not be made: nonblocking exit 1, nothing on stdout, a reason on stderr."""
    return r["exit_code"] == 1 and r["stdout"] == "" and r["stderr"].startswith("agent-runtime:")


def with_fields(text, fields):
    """The failure fixture with its `"error":...,"is_interrupt":false` swapped by text for `fields`."""
    start, end = text.index('"error":'), text.index(',"duration_ms"')
    return text[:start] + fields + text[end:]


def raw_error(text):
    i = text.index('"error":') + len('"error":')
    _, end = json.JSONDecoder().raw_decode(text, i)
    return text[i:end]


def journey(c, h):
    s = c.scratch()
    try:
        pre, fail = c.fixtures["pre_tool_use_bash"], c.fixtures["post_tool_use_failure_bash"]
        pre_input, fail_input = h.raw_tool_input(pre), h.raw_tool_input(fail)
        fixture_error = raw_error(fail)
        co = c.start("a", s, "r1", h.SLOTS)
        run = s / "runs/r1"

        def ids(text, call):
            return h.with_ids(text, session_id=SESS, tool_use_id=call)

        def gate(label, call):
            r = c.shim(label, "slot-1", co.socket, ids(pre, call))
            c.check(f"{label}: allowed", r["exit_code"] == 0 and h.parse_json(r["stdout"]) == h.ALLOW,
                    exit_code=r["exit_code"], stdout=r["stdout"])

        def report(label, stdin, args=FAILED):
            r = c.shim(label, "slot-1", co.socket, stdin, args=args)
            c.check(f"{label}: exit 0, no stdout, no stderr", silent(r),
                    exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])

        def success(call):
            """A PostToolUse for a Bash call, made from the failure fixture by text."""
            text = with_fields(ids(fail, call), '"tool_response":{"stdout":"ok","stderr":""}')
            return text.replace('"hook_event_name":"PostToolUseFailure"', '"hook_event_name":"PostToolUse"')

        # Two overlapping calls failing in reverse order: B interrupted, A as recorded (is_interrupt false).
        gate("before-a", "toolu_fail_a")
        gate("before-b", "toolu_fail_b")
        report("fail-b-interrupted", with_fields(ids(fail, "toolu_fail_b"), '"error":"Interrupted by user","is_interrupt":true'))
        report("fail-a", ids(fail, "toolu_fail_a"))
        # A repeat of A with an empty message and no flag, then a success for A (success after failure).
        report("fail-a-again", with_fields(ids(fail, "toolu_fail_a"), '"error":""'))
        report("success-a-after-failure", success("toolu_fail_a"), args=REPORT)
        # C succeeds first, then a failure arrives for it; D is an orphan.
        gate("before-c", "toolu_fail_c")
        report("success-c", success("toolu_fail_c"), args=REPORT)
        report("fail-c-after-success", ids(fail, "toolu_fail_c"))
        report("orphan-d", with_fields(ids(fail, "toolu_fail_d"), '"error":"lost"'))
        rows = c.rows("failed", run)
        got = [(x["event_id"], x["kind"], x["tool_use_id"], x["before_event_id"], x["outcome"], x["is_interrupt"], x["vclock"])
               for x in rows]
        want = [(1, "BeforeTool", "toolu_fail_a", None, None, None, [1, 0]),
                (2, "BeforeTool", "toolu_fail_b", None, None, None, [2, 0]),
                (3, "AfterTool", "toolu_fail_b", 2, "failure", 1, [3, 0]),
                (4, "AfterTool", "toolu_fail_a", 1, "failure", 0, [4, 0]),
                (5, "AfterTool", "toolu_fail_a", 1, "failure", None, [5, 0]),
                (6, "AfterTool", "toolu_fail_a", 1, "success", None, [6, 0]),
                (7, "BeforeTool", "toolu_fail_c", None, None, None, [7, 0]),
                (8, "AfterTool", "toolu_fail_c", 7, "success", None, [8, 0]),
                (9, "AfterTool", "toolu_fail_c", 7, "failure", 0, [9, 0]),
                (10, "AfterTool", "toolu_fail_d", None, "failure", None, [10, 0])]
        c.check("failures pair by identity out of order, keep is_interrupt as 1, 0 or NULL (omitted), mix with "
                "successes as rows of their own, and each tick slot-1 once", got == want, rows=got)
        if len(rows) == 10:
            c.check("each failure's payload is its raw error text, byte for byte (no envelope)",
                    [(x["payload_source"], x["payload"]) for x in (rows[2], rows[3], rows[4], rows[8], rows[9])]
                    == [("blob", '"Interrupted by user"'), ("blob", fixture_error), ("blob", '""'),
                        ("blob", fixture_error), ("blob", '"lost"')],
                    payloads=[x["payload"] for x in rows[2:]])
            c.check("every AfterTool keeps the reported input, the orphan's included; BeforeTools keep their own",
                    all(x["input_payload"] == fail_input for x in rows if x["kind"] == "AfterTool")
                    and all(x["payload"] == pre_input and x["input_payload_hash"] is None
                            for x in rows if x["kind"] == "BeforeTool"),
                    inputs=[x["input_payload_source"] for x in rows])
        names = h.blob_names(run) or []
        named = {x[k] for x in rows for k in ("payload_hash", "input_payload_hash") if x[k]}
        c.check("every hash a row names is a file in blobs/, and blobs/ holds nothing else", sorted(named) == names,
                named=sorted(named), blobs=names)

        # A failed blob write: blobs/ read-only, then the corrected retry gets the same id and stamp.
        blobs = run / "blobs"
        mode = stat.S_IMODE(os.stat(blobs).st_mode)
        e = with_fields(ids(fail, "toolu_fail_e"), '"error":"E failed","is_interrupt":false')
        os.chmod(blobs, 0o555)
        try:
            r = c.shim("blob-write-fails", "slot-1", co.socket, e, args=FAILED)
        finally:
            os.chmod(blobs, mode)
        c.check("a failure whose error blob cannot be stored: exit 1 with the reason, no row",
                report_failed(r) and "blob" in r["stderr"] and len(c.rows("blob-failed", run)) == 10,
                exit_code=r["exit_code"], stderr=r["stderr"])
        report("blob-write-retry", e)
        rows = c.rows("blob-retry", run)
        last = rows[-1] if rows else {}
        c.check("the retry is row 11 at [11, 0], an orphan failure with is_interrupt 0",
                (last.get("event_id"), last.get("vclock"), last.get("before_event_id"), last.get("outcome"),
                 last.get("is_interrupt"), last.get("payload")) == (11, [11, 0], None, "failure", 0, '"E failed"'),
                row=last)

        # Refused or unreadable failures: nonblocking exit 1 and no row, whatever the cause.
        edit = ids(fail, "toolu_fail_a").replace('"tool_name":"Bash"', '"tool_name":"Edit"')
        r = c.shim("refused-other-tool", "slot-1", co.socket, edit, args=FAILED)
        c.check("a failure naming another tool than its call: exit 1 with the reason",
                report_failed(r) and "recorded as" in r["stderr"], exit_code=r["exit_code"], stderr=r["stderr"])
        bad = [("missing-error", with_fields(fail, '"is_interrupt":false'), "error"),
               ("null-error", with_fields(fail, '"error":null'), "error"),
               ("number-error", with_fields(fail, '"error":1'), "error"),
               ("string-is-interrupt", with_fields(fail, '"error":"x","is_interrupt":"true"'), "is_interrupt"),
               ("null-is-interrupt", with_fields(fail, '"error":"x","is_interrupt":null'), "is_interrupt"),
               ("malformed-json", "not json", "hook input"),
               ("post-tool-use-json", c.fixtures["post_tool_use_edit"], "registered for")]
        for label, stdin, reason in bad:
            r = c.shim(label, "slot-1", co.socket, stdin, args=FAILED)
            c.check(f"{label}: exit 1, empty stdout, reason names {reason!r}", report_failed(r) and reason in r["stderr"],
                    exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])
        for label, slot, sock in [("only-slot", "slot-1", None), ("no-coordinator", "slot-1", s / "missing.sock")]:
            r = c.shim(label, slot, sock, fail, args=FAILED)
            c.check(f"{label}: exit 1, not 2", report_failed(r), exit_code=r["exit_code"], stderr=r["stderr"])
        r = c.shim("failure-trailing-arg", "slot-1", co.socket, fail, args=(*FAILED, "extra"))
        c.check("a stray argument after --hook-event PostToolUseFailure: exit 1, not 2",
                report_failed(r) and "unexpected" in r["stderr"], exit_code=r["exit_code"], stderr=r["stderr"])
        r = c.run("outside-run", [c.bins["art-shim"], *FAILED], env={}, stdin=fail)
        c.check("outside a run a failure report is silent", silent(r), exit_code=r["exit_code"], stdout=r["stdout"])
        r = c.shim("legacy-registration", "slot-1", co.socket, fail, args=())
        c.check("the legacy no-argument registration still ignores PostToolUseFailure", silent(r), exit_code=r["exit_code"])
        r = c.shim("success-registration", "slot-1", co.socket, fail, args=REPORT)
        c.check("the PostToolUse registration refuses a PostToolUseFailure event: exit 1",
                report_failed(r) and "registered for" in r["stderr"], exit_code=r["exit_code"], stderr=r["stderr"])
        c.check("none of those reached the trace", len(c.rows("failed-refusals", run)) == 11)
        co.stop()
        r = c.shim("coordinator-gone", "slot-1", co.socket, fail, args=FAILED)
        c.check("coordinator gone: exit 1, not 2", report_failed(r), exit_code=r["exit_code"], stderr=r["stderr"])
    finally:
        c.drop_scratch(s)
