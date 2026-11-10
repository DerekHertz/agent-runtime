"""The `after-tool` journey for verify.py: PostToolUse reports through the real binaries.

Kept out of verify.py to hold each file under the line cap. `journey(c, h)` gets verify.py's `Ctx`
as `c` and verify.py itself as `h`, for its helpers, so this module imports nothing from it.
"""
import json

REPORT = ("--hook-event", "PostToolUse")
FIXTURE_RESPONSE = '"tool_response":{"filePath":"/work/src/lib.rs"}'


def with_response(text, raw):
    """The PostToolUse fixture with its tool_response swapped by text for the raw JSON `raw`."""
    assert text.count(FIXTURE_RESPONSE) == 1
    return text.replace(FIXTURE_RESPONSE, f'"tool_response":{raw}')


def silent(r):
    return (r["exit_code"], r["stdout"], r["stderr"]) == (0, "", "")


def report_failed(r):
    """A report that could not be made: nonblocking exit 1, nothing on stdout, a reason on stderr."""
    return r["exit_code"] == 1 and r["stdout"] == "" and r["stderr"].startswith("agent-runtime:")


def journey(c, h):
    s = c.scratch()
    try:
        pre, post = c.fixtures["pre_tool_use_edit"], c.fixtures["post_tool_use_edit"]
        fx = json.loads(post)
        sess = fx["session_id"]
        pre_input, post_input = h.raw_tool_input(pre), h.raw_tool_input(post)
        co = c.start("a", s, "r1", h.SLOTS)
        run = s / "runs/r1"

        def gate(label, slot, stdin):
            r = c.shim(label, slot, co.socket, stdin)
            ok = r["exit_code"] == 0 and h.parse_json(r["stdout"]) == h.ALLOW
            c.check(f"{label}: allowed", ok, exit_code=r["exit_code"], stdout=r["stdout"])

        def report(label, slot, stdin, args=REPORT):
            r = c.shim(label, slot, co.socket, stdin, args=args)
            c.check(f"{label}: exit 0, no stdout, no stderr", silent(r),
                    exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])

        # Two overlapping calls, completed in reverse order; B's response is an explicit null.
        gate("before-a", "slot-1", h.with_ids(pre, tool_use_id="toolu_verify_a"))
        gate("before-b", "slot-1", h.with_ids(pre, tool_use_id="toolu_verify_b"))
        report("after-b-null", "slot-1", with_response(h.with_ids(post, tool_use_id="toolu_verify_b"), "null"))
        report("after-a", "slot-1", h.with_ids(post, tool_use_id="toolu_verify_a"))
        rows = c.rows("after-reversed", run)
        got = [(x["event_id"], x["kind"], x["tool_use_id"], x["before_event_id"], x["outcome"], x["vclock"]) for x in rows]
        want = [(1, "BeforeTool", "toolu_verify_a", None, None, [1, 0]),
                (2, "BeforeTool", "toolu_verify_b", None, None, [2, 0]),
                (3, "AfterTool", "toolu_verify_b", 2, "success", [3, 0]),
                (4, "AfterTool", "toolu_verify_a", 1, "success", [4, 0])]
        c.check("reversed completions pair with their own BeforeTool, each tick slot-1, outcome success", got == want, rows=got)
        if len(rows) == 4:
            c.check("B's explicit null response is the blob 'null', byte for byte",
                    (rows[2]["payload_source"], rows[2]["payload"]) == ("blob", "null"),
                    source=rows[2]["payload_source"], payload=rows[2]["payload"])
            c.check("A's response is the fixture's raw tool_response",
                    rows[3]["payload"] == '{"filePath":"/work/src/lib.rs"}', payload=rows[3]["payload"])
            c.check("each AfterTool keeps the PostToolUse input, and each BeforeTool its own (they differ)",
                    pre_input != post_input
                    and all((x["input_payload_source"], x["input_payload"]) == ("blob", post_input) for x in rows[2:])
                    and all(x["payload"] == pre_input and x["input_payload_hash"] is None for x in rows[:2]),
                    inputs=[(x["payload_source"], x["input_payload_source"]) for x in rows])

        # A repeated report and an orphan; an equal id in another session pairs with its own call.
        report("after-a-again", "slot-1", with_response(h.with_ids(post, tool_use_id="toolu_verify_a"), "42"))
        report("orphan", "slot-1", h.with_ids(post, tool_use_id="toolu_verify_orphan"))
        gate("other-session-before", "slot-2", h.with_ids(pre, session_id="sess-verify-other", tool_use_id="toolu_verify_a"))
        report("other-session-after", "slot-2", h.with_ids(post, session_id="sess-verify-other", tool_use_id="toolu_verify_a"))
        rows = c.rows("after-repeat-orphan", run)
        got = [(x["event_id"], x["session_id"], x["tool_use_id"], x["before_event_id"], x["vclock"]) for x in rows[4:]]
        want = [(5, sess, "toolu_verify_a", 1, [5, 0]), (6, sess, "toolu_verify_orphan", None, [6, 0]),
                (7, "sess-verify-other", "toolu_verify_a", None, [0, 1]), (8, "sess-verify-other", "toolu_verify_a", 7, [0, 2])]
        c.check("the repeat names the same BeforeTool as a new row; the orphan has none; the other session pairs with its own",
                got == want, rows=got)
        if len(rows) == 8:
            c.check("the repeat keeps its own response, and the first report's row is unchanged",
                    rows[4]["payload"] == "42" and rows[3]["payload"] == '{"filePath":"/work/src/lib.rs"}')
            c.check("the orphan keeps its input and response",
                    (rows[5]["input_payload"], rows[5]["payload"]) == (post_input, '{"filePath":"/work/src/lib.rs"}'))
        names = h.blob_names(run) or []
        named = {x[k] for x in rows for k in ("payload_hash", "input_payload_hash") if x[k]}
        c.check("every hash a row names is a file in blobs/, and blobs/ holds nothing else", sorted(named) == names,
                named=sorted(named), blobs=names)

        # Refused or unreadable reports: nonblocking exit 1 and no row, whatever the cause.
        bash = h.with_ids(post, tool_use_id="toolu_verify_b").replace('"tool_name":"Edit"', '"tool_name":"Bash"')
        r = c.shim("refused-other-tool", "slot-1", co.socket, bash, args=REPORT)
        c.check("a report naming another tool than its call: exit 1 with the reason", report_failed(r) and "recorded as" in r["stderr"],
                exit_code=r["exit_code"], stderr=r["stderr"])
        no_response = json.dumps({k: v for k, v in fx.items() if k != "tool_response"})
        failures = [("malformed-json", "slot-1", co.socket, "not json"), ("pre-tool-use-json", "slot-1", co.socket, pre),
                    ("missing-tool-response", "slot-1", co.socket, no_response),
                    ("only-slot", "slot-1", None, post), ("no-coordinator", "slot-1", s / "missing.sock", post)]
        for label, slot, sock, stdin in failures:
            r = c.shim(label, slot, sock, stdin, args=REPORT)
            c.check(f"{label}: exit 1, empty stdout, stderr starts 'agent-runtime:'", report_failed(r),
                    exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])
        no_tool = h.with_ids(post, tool_use_id="toolu_verify_no_tool").replace('"tool_name":"Edit"', '"tool_name":""')
        r = c.shim("empty-tool-orphan", "slot-1", co.socket, no_tool, args=REPORT)
        c.check("an orphan report with an empty tool_name: exit 1 with the reason", report_failed(r) and "no tool" in r["stderr"],
                exit_code=r["exit_code"], stderr=r["stderr"])
        r = c.shim("report-trailing-arg", "slot-1", co.socket, post, args=(*REPORT, "extra"))
        c.check("a stray argument after --hook-event PostToolUse: exit 1, not 2", report_failed(r),
                exit_code=r["exit_code"], stderr=r["stderr"])
        r = c.shim("gate-trailing-arg", "slot-1", co.socket, pre, args=("--hook-event", "PreToolUse", "extra"))
        c.check("a stray argument after --hook-event PreToolUse: exit 2", r["exit_code"] == 2 and r["stdout"] == "",
                exit_code=r["exit_code"], stderr=r["stderr"])
        r = c.run("outside-run", [c.bins["art-shim"], *REPORT], env={}, stdin=post)
        c.check("outside a run a report is silent", silent(r), exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])
        r = c.shim("legacy-registration", "slot-1", co.socket, post, args=())
        c.check("the legacy no-argument registration still ignores PostToolUse", silent(r), exit_code=r["exit_code"])
        c.check("none of those reached the trace", len(c.rows("after-failures", run)) == 8)
        co.stop()
        r = c.shim("coordinator-gone", "slot-1", co.socket, post, args=REPORT)
        c.check("coordinator gone: exit 1, not 2", report_failed(r), exit_code=r["exit_code"], stderr=r["stderr"])
    finally:
        c.drop_scratch(s)
