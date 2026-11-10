#!/usr/bin/env python3
"""Verification harness for agent-runtime: drives the REAL art-coordinator and art-shim binaries.

    verify.py build                 cargo clean -p <each workspace package>, cargo build --workspace, write the receipt
    verify.py doctor [--save]       read-only readiness check (exit 0 ready, 2 blocked)
    verify.py drive <feature|all>   run journeys, write evidence + summary.json (exit 0 pass, 1 fail, 2 blocked);
                                    PASS also needs the end checks: sources, receipt and binaries as at the start
    verify.py cleanup               stop only what this harness started, remove scratch dirs, keep evidence
    verify.py list                  list feature ids

Python 3 stdlib only. Evidence: <repo>/.claude/reviews/evidence/verify-agent-runtime/<UTC>/ (gitignored).
"""
import argparse, datetime, hashlib, json, os, pathlib, shutil, signal, socket, sqlite3
import struct, subprocess, sys, tempfile, time, traceback

sys.dont_write_bytecode = True  # importing the journey module must leave no __pycache__ in the skill
import after_tool  # noqa: E402  the after-tool journey, in its own file (scripts/after_tool.py)
import failed_tool  # noqa: E402  the failed-tool journey (scripts/failed_tool.py)
import lifecycle  # noqa: E402  the lifecycle journey, Stop and SessionEnd (scripts/lifecycle.py)
import launcher  # noqa: E402  the launcher journey, art run and art query (scripts/launcher.py)
import leases  # noqa: E402  the leases journey, a denied write and its release (scripts/leases.py)

SCRIPT = pathlib.Path(__file__).resolve()
SKILL = SCRIPT.parents[1]
REPO = SCRIPT.parents[4]
EVIDENCE_ROOT = REPO / ".claude/reviews/evidence/verify-agent-runtime"
ACTIVE = EVIDENCE_ROOT / "active-processes.json"
FIXTURES = REPO / "crates/shim/tests/fixtures"
ENV_VARS = ("AGENT_RUNTIME_SLOT", "AGENT_RUNTIME_SOCKET")
MAX_SOCKET_PATH = 100  # macOS sun_path is 104 bytes including the NUL
ALLOW = {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}}
# Hook JSON in FIXTURES (see its README.md): the *_synthetic ones were written by hand, not recorded.
FIXTURE_NAMES = ("pre_tool_use_edit", "pre_tool_use_bash", "pre_tool_use_read", "pre_tool_use_write_synthetic",
                 "pre_tool_use_notebook_edit_synthetic", "post_tool_use_edit", "post_tool_use_failure_bash",
                 "stop", "session_end")


def utc():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sh(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def target_dir(args):
    return pathlib.Path(args.target_dir or os.environ.get("CARGO_TARGET_DIR") or REPO / "target").resolve()


def binaries(args):
    d = target_dir(args) / "debug"
    return {"art-coordinator": d / "art-coordinator", "art-shim": d / "art-shim", "art": d / "art"}


def clean_env(**extra):
    env = {k: v for k, v in os.environ.items() if k not in ENV_VARS}
    env.update(extra)
    return env


# ---------------------------------------------------------------- active process registry
def load_active():
    try:
        return json.loads(ACTIVE.read_text())
    except (OSError, ValueError):
        return []


def save_active(entries):
    ACTIVE.parent.mkdir(parents=True, exist_ok=True)
    ACTIVE.write_text(json.dumps(entries, indent=2) + "\n")


def register(entry):
    save_active(load_active() + [entry])


def unregister(match):
    save_active([e for e in load_active() if not match(e)])


def pid_command(pid):
    out = sh(["ps", "-p", str(pid), "-o", "command="])
    return out.stdout.strip() if out.returncode == 0 and out.stdout.strip() else None


def terminate(pid, needle):
    """SIGTERM then SIGKILL pid, but only while its command line still contains needle."""
    for sig, wait in ((signal.SIGTERM, 8.0), (signal.SIGKILL, 3.0)):
        cmd = pid_command(pid)
        if cmd is None:
            return True
        if needle not in cmd:
            return True  # pid was reused by something that is not ours: leave it alone
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            return True
        end = time.monotonic() + wait
        while time.monotonic() < end:
            if pid_command(pid) is None:
                return True
            time.sleep(0.05)
    return pid_command(pid) is None


def rm_scratch(path):
    p = pathlib.Path(path)
    if p.name.startswith("artv-") and p.is_dir():  # only dirs this harness created
        shutil.rmtree(p, ignore_errors=True)


def cmd_cleanup(args=None):
    report, left = [], 0
    for e in load_active():
        if e["kind"] == "coordinator":
            ok = terminate(e["pid"], e["needle"])
            report.append(f"coordinator pid {e['pid']}: {'stopped or already gone' if ok else 'STILL RUNNING'}")
            if not ok:
                left += 1
                continue
        if e.get("scratch"):
            rm_scratch(e["scratch"])
            if os.path.exists(e["scratch"]):
                report.append(f"scratch {e['scratch']}: NOT removed")
                left += 1
                continue
            report.append(f"scratch {e['scratch']}: removed")
        unregister(lambda x, e=e: x == e)
    print("\n".join(report) or "nothing to clean up")
    print(f"evidence kept at {EVIDENCE_ROOT}")
    return 1 if left else 0


# ---------------------------------------------------------------- build receipt
def receipt_path(args):
    return target_dir(args) / "debug" / "verify-agent-runtime-build-receipt.json"


def load_receipt(args):
    try:
        return json.loads(receipt_path(args).read_text())
    except (OSError, ValueError):
        return None


def receipt_checks(args):
    """Doctor's freshness checks: the binaries are the ones `verify.py build` produced from the current sources.

    Content, not mtimes: an incremental build leaves an unaffected binary untouched, so timestamps can't tell.
    """
    out = []
    add = lambda name, ok, detail="": out.append({"name": name, "passed": bool(ok), "detail": detail})
    receipt = load_receipt(args)
    add("build receipt present", receipt is not None,
        str(receipt_path(args)) if receipt else f"missing {receipt_path(args)}; run `verify.py build`")
    if receipt is None:
        return out
    current = source_manifest()["aggregate_sha256"]
    add("binaries were built from the current sources", receipt.get("sources_aggregate_sha256") == current,
        f"receipt {receipt.get('sources_aggregate_sha256')}, now {current}; run `verify.py build` if this fails")
    for name, p in binaries(args).items():
        want = receipt.get("binaries", {}).get(name)
        got = sha256(p) if p.is_file() else None
        add(f"{name} unchanged since that build", got is not None and got == want,
            f"receipt {want}, now {got}; run `verify.py build` if this fails")
    return out


# ---------------------------------------------------------------- doctor
def bind_probe(root):
    """Create a throwaway scratch dir and bind a Unix socket in it; return (ok, detail)."""
    try:
        d = tempfile.mkdtemp(prefix="artv-", dir=root)
    except OSError as e:
        return False, (f"{e}: cannot create a scratch dir in {root}. The Claude Code sandbox only allows $TMPDIR and "
                       f"/tmp/claude: rerun with the sandbox disabled, or pass --scratch-root <short writable dir>")
    s = socket.socket(socket.AF_UNIX)
    try:
        s.bind(os.path.join(d, "c.sock"))
        return True, d
    except PermissionError as e:
        return False, f"{e} (EPERM): the Claude Code sandbox refuses Unix-socket bind; rerun with the sandbox disabled"
    except OSError as e:
        return False, str(e)
    finally:
        s.close()
        shutil.rmtree(d, ignore_errors=True)


def doctor_checks(args):
    out = []
    add = lambda name, ok, detail="": out.append({"name": name, "passed": bool(ok), "detail": detail})
    bins = binaries(args)
    for name, p in bins.items():
        add(f"{name} exists", p.is_file() and os.access(p, os.X_OK), str(p))
    out.extend(receipt_checks(args))
    if bins["art-coordinator"].is_file():
        r = sh([str(bins["art-coordinator"]), "--help"], env=clean_env(), timeout=10)
        add("art-coordinator --help works", r.returncode == 0 and "usage: art-coordinator" in r.stdout,
            f"exit {r.returncode}")
    if bins["art-shim"].is_file():
        r = subprocess.run([str(bins["art-shim"])], input="", capture_output=True, text=True, env=clean_env(), timeout=10)
        add("art-shim with no env exits 0 silently", (r.returncode, r.stdout, r.stderr) == (0, "", ""), f"exit {r.returncode}")
    missing = [f"{n}.json" for n in FIXTURE_NAMES if not (FIXTURES / f"{n}.json").is_file()]
    add("hook fixtures present", not missing, f"missing {missing}" if missing else str(FIXTURES))
    add("python sqlite3 module", bool(sqlite3.sqlite_version), sqlite3.sqlite_version)
    root = pathlib.Path(args.scratch_root)
    sample = root / "artv-XXXXXXXX" / "c.sock"
    add(f"socket path length <= {MAX_SOCKET_PATH}", len(str(sample)) <= MAX_SOCKET_PATH, f"{len(str(sample))} bytes: {sample}")
    ok, detail = bind_probe(root)
    add("scratch root is writable and a Unix socket can be bound there", ok, detail)
    live = [e for e in load_active() if e["kind"] == "coordinator" and pid_command(e["pid"])]
    add("no leftover harness coordinators", not live, f"run `verify.py cleanup`: {live}" if live else "")
    return out


def cmd_doctor(args):
    checks = doctor_checks(args)
    for c in checks:
        print(f"[{'ok' if c['passed'] else 'FAIL'}] {c['name']}: {c['detail']}")
    if args.save:
        d = EVIDENCE_ROOT / f"doctor-{datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')}"
        d.mkdir(parents=True)
        (d / "doctor.json").write_text(json.dumps(checks, indent=2) + "\n")
        print(f"saved {d}/doctor.json")
    return 0 if all(c["passed"] for c in checks) else 2


def workspace_packages():
    """The root workspace's own package names, from `cargo metadata --no-deps`; (names, error)."""
    r = sh(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked", "--offline"], cwd=REPO)
    if r.returncode != 0:
        return None, f"cargo metadata exited {r.returncode}: {r.stderr.strip()}"
    try:
        meta = json.loads(r.stdout)
        members = set(meta["workspace_members"])
        names = sorted(p["name"] for p in meta["packages"] if p["id"] in members)
    except (ValueError, KeyError, TypeError) as e:
        return None, f"unreadable cargo metadata: {e}"
    return (names, None) if names else (None, "cargo metadata lists no workspace members")


def cmd_build(args):
    """Clean the workspace's own packages, cargo build, then record which sources produced which
    binaries (the receipt doctor checks)."""
    receipt = receipt_path(args)
    receipt.unlink(missing_ok=True)  # a failed or raced build must not leave an older receipt vouching for it
    before = source_manifest()
    env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir(args)))
    # Cargo decides freshness by mtime, so a source restored with an older mtime than its artifact
    # (a mutation check, a `cp -p`, a checkout) leaves the stale artifact in place. Cleaning the
    # workspace's own packages forces them to rebuild from the bytes the receipt hashes; third-party
    # dependencies stay cached. `cargo clean` has no --workspace, hence one -p per member.
    names, err = workspace_packages()
    if names is None:
        print(f"clean: {err}; no receipt written")
        return 1
    print(f"clean: cargo clean -p for the workspace packages {names} in {target_dir(args)}")
    clean = ["cargo", "clean", "--locked", "--offline"] + [a for n in names for a in ("-p", n)]
    rc = subprocess.run(clean, cwd=REPO, env=env).returncode
    if rc != 0:
        print(f"clean: cargo clean exited {rc}; no receipt written")
        return rc
    rc = subprocess.run(["cargo", "build", "--workspace", "--locked", "--offline"], cwd=REPO, env=env).returncode
    if rc != 0:
        return rc
    after = source_manifest()
    if after["aggregate_sha256"] != before["aggregate_sha256"]:
        print("sources changed during the build; no receipt written. Run `verify.py build` again.")
        return 1
    bins = {name: sha256(p) for name, p in binaries(args).items()}
    receipt.write_text(json.dumps({"built_utc": utc(), "sources_aggregate_sha256": after["aggregate_sha256"],
                                   "source_file_count": after["file_count"], "binaries": bins,
                                   "cleaned_packages": names, "target_dir": str(target_dir(args))}, indent=2) + "\n")
    print(f"build receipt: {receipt}")
    return 0


# ---------------------------------------------------------------- journey plumbing
class Coord:
    def __init__(self, tag, proc, socket_path, files):
        self.tag, self.proc, self.socket, self.files = tag, proc, socket_path, files

    def stop(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        for f in self.files:
            f.close()
        unregister(lambda e: e.get("pid") == self.proc.pid)
        return self.proc.returncode


class Ctx:
    def __init__(self, args, ev, name):
        self.args, self.name, self.bins = args, name, binaries(args)
        self.dir = ev / name
        self.dir.mkdir(parents=True)
        self.n, self.checks, self.commands = 0, [], []
        self.fixtures = {k: (FIXTURES / f"{k}.json").read_text() for k in FIXTURE_NAMES}

    def check(self, name, ok, **info):
        self.checks.append({"name": name, "passed": bool(ok), **info})
        print(f"  [{'ok' if ok else 'FAIL'}] {name}")
        return bool(ok)

    def run(self, label, argv, env=None, stdin=None, timeout=30):
        """Run a command to completion, record it, return a result dict."""
        self.n += 1
        rec = {"label": label, "argv": [str(a) for a in argv], "stdin": stdin, "started_utc": utc(),
               "env_AGENT_RUNTIME": {k: (env or {}).get(k) for k in ENV_VARS}}
        try:
            r = subprocess.run([str(a) for a in argv], input=stdin if stdin is not None else "", capture_output=True,
                               text=True, env=clean_env(**(env or {})), timeout=timeout)
            rec.update(exit_code=r.returncode, stdout=r.stdout, stderr=r.stderr)
        except subprocess.TimeoutExpired as e:
            rec.update(exit_code=None, timed_out=True, stdout=str(e.stdout or ""), stderr=str(e.stderr or ""))
        rec["finished_utc"] = utc()
        f = self.dir / f"{self.n:02d}-{label}.json"
        f.write_text(json.dumps(rec, indent=2) + "\n")
        self.commands.append({"label": label, "exit_code": rec["exit_code"], "file": str(f.relative_to(self.dir.parent))})
        return rec

    def shim(self, label, slot, socket_path, stdin, args=(), **kw):
        """Run art-shim as Claude Code would; `args` is the hook registration's command line."""
        env = {}
        if slot is not None:
            env["AGENT_RUNTIME_SLOT"] = slot
        if socket_path is not None:
            env["AGENT_RUNTIME_SOCKET"] = str(socket_path)
        return self.run(label, [self.bins["art-shim"], *args], env=env, stdin=stdin, **kw)

    def coordinator_argv(self, run_id, slots, runs_dir, sock):
        return [self.bins["art-coordinator"], "--run-id", run_id, "--slots", ",".join(slots),
                "--runs-dir", runs_dir, "--socket", sock]

    def start(self, tag, scratch, run_id, slots, extra=()):
        """Start a real art-coordinator in scratch; return it once its socket accepts a connection."""
        sock, runs_dir = scratch / "c.sock", scratch / "runs"
        if len(str(sock)) > MAX_SOCKET_PATH:
            raise RuntimeError(f"socket path too long ({len(str(sock))}): {sock}")
        argv = [str(a) for a in (*self.coordinator_argv(run_id, slots, runs_dir, sock), *extra)]
        files = [open(self.dir / f"coordinator-{tag}.{s}", "w") for s in ("stdout", "stderr")]
        proc = subprocess.Popen(argv, stdout=files[0], stderr=files[1], env=clean_env())
        register({"kind": "coordinator", "pid": proc.pid, "needle": str(sock), "scratch": str(scratch),
                  "socket": str(sock), "started_utc": utc(), "argv": argv})
        c = Coord(tag, proc, sock, files)
        end = time.monotonic() + 15
        while True:
            if proc.poll() is not None:
                c.stop()
                raise RuntimeError(f"coordinator {tag} exited {proc.returncode} before ready: "
                                   f"{(self.dir / f'coordinator-{tag}.stderr').read_text()}")
            if time.monotonic() > end:
                c.stop()
                raise RuntimeError(f"coordinator {tag} not ready after 15 s")
            if os.path.exists(sock):
                s = socket.socket(socket.AF_UNIX)
                try:
                    s.connect(str(sock))
                    break
                except OSError:
                    pass
                finally:
                    s.close()
            time.sleep(0.02)
        return c

    def refuse(self, label, run_id, slots, sock, runs_dir):
        """Start a second coordinator that must exit non-zero on its own."""
        return self.run(label, self.coordinator_argv(run_id, slots, runs_dir, sock), timeout=15)

    def rows(self, label, run_dir):
        """Read the run's trace.db read-only and save the decoded rows as evidence."""
        db = pathlib.Path(run_dir) / "trace.db"
        rows = []
        if db.exists():
            conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
            try:
                q = ("SELECT event_id, agent_id, session_id, vclock, kind, tool, path, payload_hash, tool_input, wall_time, "
                     "tool_use_id, before_event_id, outcome, input_payload_hash, is_interrupt FROM events ORDER BY event_id")
                for r in conn.execute(q):
                    n = len(r[3]) // 8
                    source, payload = resolve_payload(run_dir, r[7], r[8])
                    in_source, in_payload = resolve_payload(run_dir, r[13], None)
                    rows.append({"event_id": r[0], "slot": r[1], "session_id": r[2], "vclock": list(struct.unpack(f"<{n}Q", r[3])),
                                 "kind": r[4], "tool": r[5], "path": r[6], "payload_hash": r[7].hex() if r[7] is not None else None,
                                 "tool_input": r[8], "payload_source": source, "payload": payload, "wall_time": r[9],
                                 "tool_use_id": r[10], "before_event_id": r[11], "outcome": r[12],
                                 "input_payload_hash": r[13].hex() if r[13] is not None else None,
                                 "input_payload_source": in_source, "input_payload": in_payload, "is_interrupt": r[14]})
            finally:
                conn.close()
        (self.dir / f"trace-{label}.json").write_text(json.dumps({"db": str(db), "rows": rows}, indent=2) + "\n")
        return rows

    def scratch(self):
        d = pathlib.Path(tempfile.mkdtemp(prefix="artv-", dir=self.args.scratch_root))
        register({"kind": "scratch", "scratch": str(d), "started_utc": utc()})
        return d

    def drop_scratch(self, d):
        rm_scratch(d)
        unregister(lambda e: e.get("kind") == "scratch" and e["scratch"] == str(d))


def resolve_payload(run_dir, payload_hash, inline):
    """Where a row's payload lives and its exact text: ("blob", text) read from blobs/<hex of the
    hash>, ("inline", text) for an issue #2 row, ("none", None), or ("missing", None) when the row
    names a blob that is not there (or a hash that is not 32 bytes). The blob's name is not
    rehashed here: python's stdlib has no blake3, so hash correctness is the Rust tests' job."""
    if payload_hash is None:
        return ("inline", inline) if inline is not None else ("none", None)
    blob = pathlib.Path(run_dir) / "blobs" / payload_hash.hex()
    if len(payload_hash) != 32 or not blob.is_file():
        return ("missing", None)
    return ("blob", blob.read_bytes().decode("utf-8", "surrogateescape"))


def blob_names(run_dir):
    d = pathlib.Path(run_dir) / "blobs"
    return sorted(p.name for p in d.iterdir()) if d.is_dir() else None


def raw_tool_input(text):
    key = '"tool_input":'
    assert text.count(key) == 1
    i = text.index(key) + len(key)
    while text[i].isspace():
        i += 1
    _, end = json.JSONDecoder().raw_decode(text, i)
    return text[i:end]


def with_ids(text, session_id=None, tool_use_id=None):
    """The hook JSON with its top-level session_id and/or tool_use_id values swapped, by text, so
    every other byte (the raw tool_input included) stays as recorded."""
    fx = json.loads(text)
    for field, value in (("session_id", session_id), ("tool_use_id", tool_use_id)):
        if value is not None:
            old = f"\"{field}\":{json.dumps(fx[field])}"
            assert text.count(old) == 1, old
            text = text.replace(old, f"\"{field}\":{json.dumps(value)}")
    return text


def parse_json(s):
    try:
        return json.loads(s)
    except ValueError:
        return None


def is_blocked(r):
    return r["exit_code"] == 2 and r["stdout"] == "" and r["stderr"].startswith("agent-runtime:")


def deny_reason(r):
    h = (parse_json(r["stdout"]) or {}).get("hookSpecificOutput", {})
    return h.get("permissionDecision"), h.get("permissionDecisionReason", "")


def refused(r):
    return r["exit_code"] not in (0, None)


def ids_clocks(rows):
    return [(x["event_id"], x["vclock"]) for x in rows]


# ---------------------------------------------------------------- journeys
SLOTS = ["slot-1", "slot-2"]


def j_admitted_tool_call(c):
    s = c.scratch()
    try:
        co = c.start("a", s, "r1", SLOTS)
        edit = c.fixtures["pre_tool_use_edit"]
        fx = json.loads(edit)
        t0 = int(time.time() * 1000)
        r = c.shim("edit-slot-2", "slot-2", co.socket, edit)
        t1 = int(time.time() * 1000)
        c.check("shim exits 0", r["exit_code"] == 0, exit_code=r["exit_code"])
        c.check("stdout is exactly the allow decision", parse_json(r["stdout"]) == ALLOW, stdout=r["stdout"])
        c.check("stderr empty", r["stderr"] == "", stderr=r["stderr"])
        rows = c.rows("after-edit", s / "runs/r1")
        c.check("exactly one event in trace.db", len(rows) == 1, count=len(rows))
        e = rows[0] if rows else {}
        c.check("event is BeforeTool for slot-2, the fixture session, tool Edit",
                (e.get("kind"), e.get("slot"), e.get("session_id"), e.get("tool")) == ("BeforeTool", "slot-2", fx["session_id"], "Edit"),
                row={k: e.get(k) for k in ("kind", "slot", "session_id", "tool")})
        c.check("the row keeps the fixture's tool_use_id", e.get("tool_use_id") == fx["tool_use_id"], tool_use_id=e.get("tool_use_id"))
        c.check("event_id is 1 and vclock is [0,1]", (e.get("event_id"), e.get("vclock")) == (1, [0, 1]),
                event_id=e.get("event_id"), vclock=e.get("vclock"))
        c.check("the row keeps no inline tool_input", e.get("tool_input") is None, stored=e.get("tool_input"))
        h = e.get("payload_hash")
        c.check("payload_hash is 32 bytes and names the only file in blobs/",
                h is not None and len(h) == 64 and blob_names(s / "runs/r1") == [h], payload_hash=h, blobs=blob_names(s / "runs/r1"))
        c.check("the blob's bytes are byte-identical to the fixture's raw tool_input",
                (e.get("payload_source"), e.get("payload")) == ("blob", raw_tool_input(edit)),
                source=e.get("payload_source"), stored=e.get("payload"))
        # wall_time is display-only (SPEC section 5): recorded as evidence, never a pass/fail criterion.
        (c.dir / "wall-time.txt").write_text(f"call window {t0}..{t1} ms; event wall_time {e.get('wall_time')} (informational only)\n")
        c.check("coordinator stderr announces its socket", f"listening on {co.socket}" in (c.dir / "coordinator-a.stderr").read_text())
        code = co.stop()
        c.check("SIGTERM stops the coordinator cleanly and removes its socket", code == 0 and not co.socket.exists(),
                exit_code=code, socket_exists=co.socket.exists())
    finally:
        c.drop_scratch(s)


def j_outside_run_silent(c):
    s = c.scratch()
    try:
        co = c.start("a", s, "r1", SLOTS)
        edit = c.fixtures["pre_tool_use_edit"]
        cases = [("no-env-fixture", {}, edit), ("no-env-garbage", {}, "this is not JSON"), ("no-env-empty-stdin", {}, ""),
                 ("empty-env-values", {"AGENT_RUNTIME_SLOT": "", "AGENT_RUNTIME_SOCKET": ""}, edit)]
        for label, env, stdin in cases:
            r = c.run(label, [c.bins["art-shim"]], env=env, stdin=stdin)
            c.check(f"{label}: exit 0, no stdout, no stderr", (r["exit_code"], r["stdout"], r["stderr"]) == (0, "", ""),
                    exit_code=r["exit_code"], stdout=r["stdout"], stderr=r["stderr"])
        c.check("coordinator saw no event", c.rows("after-silent", s / "runs/r1") == [])
        r = c.shim("sanity-in-run", "slot-1", co.socket, edit)
        c.check("coordinator still serves a real run call afterwards", r["exit_code"] == 0 and parse_json(r["stdout"]) == ALLOW)
        c.check("the sanity call is event 1", [x["event_id"] for x in c.rows("after-sanity", s / "runs/r1")] == [1])
        co.stop()
    finally:
        c.drop_scratch(s)


def j_fail_closed(c):
    s = c.scratch()
    try:
        edit = c.fixtures["pre_tool_use_edit"]
        missing = s / "missing.sock"
        r = c.shim("no-coordinator", "slot-1", missing, edit)
        c.check("env set + no coordinator: exit 2, empty stdout, stderr starts 'agent-runtime:'", is_blocked(r),
                exit_code=r["exit_code"], stderr=r["stderr"])
        stale = s / "stale.sock"
        k = socket.socket(socket.AF_UNIX)
        k.bind(str(stale))
        k.close()  # leaves a socket file nobody listens on
        r = c.shim("stale-socket", "slot-1", stale, edit)
        c.check("stale socket file (connection refused): exit 2, no allow", is_blocked(r), exit_code=r["exit_code"], stderr=r["stderr"])
        r = c.shim("only-slot", "slot-1", None, edit)
        c.check("only AGENT_RUNTIME_SLOT set: exit 2", is_blocked(r) and "AGENT_RUNTIME_SOCKET" in r["stderr"], exit_code=r["exit_code"], stderr=r["stderr"])
        r = c.shim("only-socket", None, missing, edit)
        c.check("only AGENT_RUNTIME_SOCKET set: exit 2", is_blocked(r) and "AGENT_RUNTIME_SLOT" in r["stderr"], exit_code=r["exit_code"], stderr=r["stderr"])
        co = c.start("a", s, "r1", SLOTS)
        r = c.shim("only-slot-with-live-coordinator", "slot-1", None, edit)
        c.check("half-configured env fails closed even with a live coordinator", is_blocked(r), exit_code=r["exit_code"])
        r = c.shim("allowed-while-alive", "slot-1", co.socket, edit)
        c.check("same call is allowed while the coordinator is up", r["exit_code"] == 0 and parse_json(r["stdout"]) == ALLOW)
        co.stop()
        r = c.shim("after-coordinator-stopped", "slot-1", co.socket, edit)
        c.check("coordinator gone mid-run: exit 2, no allow", is_blocked(r), exit_code=r["exit_code"], stderr=r["stderr"])
        c.check("the blocked calls left only the one allowed event", [x["event_id"] for x in c.rows("final", s / "runs/r1")] == [1])
    finally:
        c.drop_scratch(s)


def j_run_reservation(c):
    s = c.scratch()
    try:
        edit = c.fixtures["pre_tool_use_edit"]
        a = c.start("a", s, "r1", SLOTS)
        ino = os.stat(a.socket).st_ino
        runs, other = s / "runs", s / "o.sock"
        r = c.refuse("same-run-other-socket", "r1", SLOTS, other, runs)
        c.check("same run dir + different socket: refused (non-zero exit)", refused(r), exit_code=r["exit_code"], stderr=r["stderr"])
        c.check("...stderr says the run is already held", "already held" in r["stderr"], stderr=r["stderr"])
        c.check("...and no second socket appeared", not other.exists())
        r = c.refuse("same-socket-new-run", "r2", SLOTS, a.socket, runs)
        c.check("same socket + different run: refused", refused(r) and "in use" in r["stderr"], exit_code=r["exit_code"], stderr=r["stderr"])
        c.check("...the new run dir was not created", not (runs / "r2").exists())
        r = c.refuse("same-run-same-socket", "r1", SLOTS, a.socket, runs)
        c.check("same run dir + same socket: refused", refused(r), exit_code=r["exit_code"], stderr=r["stderr"])
        c.check("first coordinator's socket file is untouched (same inode)", os.stat(a.socket).st_ino == ino)
        c.check("first coordinator is still running", a.proc.poll() is None)
        r = c.shim("first-still-works", "slot-1", a.socket, edit)
        c.check("first coordinator still admits calls", r["exit_code"] == 0 and parse_json(r["stdout"]) == ALLOW, exit_code=r["exit_code"])
        rows = c.rows("after-refusals", runs / "r1")
        c.check("exactly one event, stamped from zero (event 1, vclock [1,0])", ids_clocks(rows) == [(1, [1, 0])], rows=ids_clocks(rows))
        a.stop()
        r = c.refuse("populated-run", "r1", SLOTS, s / "c.sock", runs)
        c.check("a run dir that already has events is refused", refused(r), exit_code=r["exit_code"], stderr=r["stderr"])
        c.check("...stderr says resuming a run is not supported", "resuming a run is not supported" in r["stderr"], stderr=r["stderr"])
        c.check("...no socket left behind and the trace is unchanged", not (s / "c.sock").exists() and len(c.rows("final", runs / "r1")) == 1)
    finally:
        c.drop_scratch(s)


def j_unknown_slot(c):
    s = c.scratch()
    try:
        edit = c.fixtures["pre_tool_use_edit"]
        co = c.start("a", s, "r1", SLOTS)
        r = c.shim("unknown-slot", "slot-9", co.socket, edit)
        decision, reason = deny_reason(r)
        c.check("shim exits 0 with a deny decision", r["exit_code"] == 0 and decision == "deny", exit_code=r["exit_code"], stdout=r["stdout"])
        c.check("deny reason names the unknown slot", 'unknown slot "slot-9"' in reason, reason=reason)
        c.check("no event was recorded", c.rows("after-unknown", s / "runs/r1") == [])
        c.check("no payload blob was stored for the denied call", blob_names(s / "runs/r1") == [], blobs=blob_names(s / "runs/r1"))
        r = c.shim("valid-after-unknown", "slot-1", co.socket, edit)
        rows = c.rows("after-valid", s / "runs/r1")
        c.check("a valid slot afterwards is allowed as event 1 with vclock [1,0] (the deny consumed nothing)",
                r["exit_code"] == 0 and ids_clocks(rows) == [(1, [1, 0])], rows=ids_clocks(rows))
        co.stop()
    finally:
        c.drop_scratch(s)


def j_clocks(c):
    s = c.scratch()
    try:
        edit, bash = c.fixtures["pre_tool_use_edit"], c.fixtures["pre_tool_use_bash"]
        # A second Edit call with the same input: Claude Code gives it its own tool_use_id.
        edit_again = with_ids(edit, tool_use_id="toolu_verify_edit_again")
        co = c.start("a", s, "r1", SLOTS)
        for i, (slot, payload) in enumerate([("slot-1", edit), ("slot-2", bash), ("slot-1", edit_again)], 1):
            r = c.shim(f"call-{i}-{slot}", slot, co.socket, payload)
            c.check(f"call {i} ({slot}) allowed", r["exit_code"] == 0 and parse_json(r["stdout"]) == ALLOW, exit_code=r["exit_code"])
        rows = c.rows("final", s / "runs/r1")
        c.check("event ids are 1,2,3 in call order", [x["event_id"] for x in rows] == [1, 2, 3])
        c.check("each slot's own counter advances, the other's stays put: [1,0] [0,1] [2,0]",
                [x["vclock"] for x in rows] == [[1, 0], [0, 1], [2, 0]], vclocks=[x["vclock"] for x in rows])
        c.check("slots recorded in call order", [x["slot"] for x in rows] == ["slot-1", "slot-2", "slot-1"])
        c.check("Bash call stored with tool Bash and its blob byte-identical to the raw tool_input",
                len(rows) > 1 and rows[1]["tool"] == "Bash" and (rows[1]["payload_source"], rows[1]["payload"]) == ("blob", raw_tool_input(bash)))
        hashes = [x["payload_hash"] for x in rows]
        c.check("the two Edit calls share one blob; blobs/ holds exactly the Edit and Bash blobs, no temporary files",
                len(rows) == 3 and hashes[0] == hashes[2] != hashes[1] and blob_names(s / "runs/r1") == sorted({hashes[0], hashes[1]}),
                payload_hashes=hashes, blobs=blob_names(s / "runs/r1"))
        co.stop()
    finally:
        c.drop_scratch(s)


def j_hook_input(c):
    s = c.scratch()
    try:
        edit = c.fixtures["pre_tool_use_edit"]
        co = c.start("a", s, "r1", SLOTS)
        r = c.shim("post-tool-use", "slot-1", co.socket, c.fixtures["post_tool_use_edit"])
        c.check("PostToolUse is ignored: exit 0, no output", (r["exit_code"], r["stdout"], r["stderr"]) == (0, "", ""), exit_code=r["exit_code"], stdout=r["stdout"])
        for label, payload in (("malformed-json", "not json"), ("empty-stdin", ""), ("json-array", "[]")):
            r = c.shim(label, "slot-1", co.socket, payload)
            c.check(f"{label} inside a run: exit 2, stderr starts 'agent-runtime:'", is_blocked(r), exit_code=r["exit_code"], stderr=r["stderr"])
        fx = json.loads(edit)
        for field in ("session_id", "tool_name", "tool_input", "tool_use_id"):
            d = {k: v for k, v in fx.items() if k != field}
            r = c.shim(f"missing-{field}", "slot-1", co.socket, json.dumps(d))
            c.check(f"PreToolUse without {field}: exit 2", is_blocked(r), exit_code=r["exit_code"], stderr=r["stderr"])
        c.check("none of those reached the trace", c.rows("after-bad-input", s / "runs/r1") == [])
        c.check("...or the blob store", blob_names(s / "runs/r1") == [], blobs=blob_names(s / "runs/r1"))
        r = c.shim("valid-after", "slot-1", co.socket, edit)
        c.check("a valid call afterwards is allowed as event 1", r["exit_code"] == 0 and [x["event_id"] for x in c.rows("final", s / "runs/r1")] == [1])
        co.stop()
    finally:
        c.drop_scratch(s)


def j_call_identity(c):
    s = c.scratch()
    try:
        edit = c.fixtures["pre_tool_use_edit"]
        fx = json.loads(edit)
        sess, tid = fx["session_id"], fx["tool_use_id"]
        retry = with_ids(edit, tool_use_id="toolu_verify_notool")
        assert retry.count('"tool_name":"Edit"') == 1
        no_tool = retry.replace('"tool_name":"Edit"', '"tool_name":""')
        co = c.start("a", s, "r1", SLOTS)
        run = s / "runs/r1"

        def decision(label, slot, stdin):
            r = c.shim(label, slot, co.socket, stdin)
            d, reason = deny_reason(r)
            return r["exit_code"], d, reason

        code, d, _ = decision("first-call", "slot-1", edit)
        c.check("first call of the session on slot-1 is allowed", (code, d) == (0, "allow"), exit_code=code, decision=d)
        steps = [("repeat-before", "slot-1", edit, "already recorded"),
                 ("session-on-other-slot", "slot-2", with_ids(edit, tool_use_id="toolu_verify_moved"), "bound to slot"),
                 ("empty-tool-use-id", "slot-1", with_ids(edit, tool_use_id=""), "empty tool_use_id"),
                 ("empty-session-id", "slot-1", with_ids(edit, session_id="", tool_use_id="toolu_verify_nosess"), "empty session_id"),
                 ("empty-tool-name", "slot-1", no_tool, "no tool")]
        for label, slot, stdin, why in steps:
            code, d, reason = decision(label, slot, stdin)
            c.check(f"{label}: exit 0 with a deny decision whose reason says {why!r}", (code, d) == (0, "deny") and why in reason,
                    exit_code=code, decision=d, reason=reason)
        rows = c.rows("after-refusals", run)
        c.check("the refusals recorded nothing: one row, event 1, vclock [1,0]", ids_clocks(rows) == [(1, [1, 0])], rows=ids_clocks(rows))
        c.check("...and stored one blob only", len(blob_names(run) or []) == 1, blobs=blob_names(run))
        calls = [("restart-same-slot", "slot-1", with_ids(edit, session_id="sess-verify-restart", tool_use_id="toolu_verify_r1")),
                 ("same-id-other-session", "slot-2", with_ids(edit, session_id="sess-verify-other")),
                 ("overlap-1", "slot-1", with_ids(edit, tool_use_id="toolu_verify_o1")),
                 ("overlap-2", "slot-1", with_ids(edit, tool_use_id="toolu_verify_o2")),
                 ("empty-tool-retry", "slot-1", retry)]
        for label, slot, stdin in calls:
            code, d, reason = decision(label, slot, stdin)
            c.check(f"{label}: allowed", (code, d) == (0, "allow"), exit_code=code, decision=d, reason=reason)
        rows = c.rows("final", run)
        got = [(x["event_id"], x["slot"], x["session_id"], x["tool_use_id"], x["vclock"]) for x in rows]
        want = [(1, "slot-1", sess, tid, [1, 0]), (2, "slot-1", "sess-verify-restart", "toolu_verify_r1", [2, 0]),
                (3, "slot-2", "sess-verify-other", tid, [0, 1]), (4, "slot-1", sess, "toolu_verify_o1", [3, 0]),
                (5, "slot-1", sess, "toolu_verify_o2", [4, 0]), (6, "slot-1", sess, "toolu_verify_notool", [5, 0])]
        c.check("rows: a restarted session continues slot-1's clock, an equal id in another session is its own call, "
                "overlapping calls keep their own events, and the empty-tool call's id was left free for its retry",
                got == want, rows=got)
        c.check("every row's blob is the fixture's raw tool_input",
                all((x["payload_source"], x["payload"]) == ("blob", raw_tool_input(edit)) for x in rows))
        co.stop()
    finally:
        c.drop_scratch(s)


FEATURES = {
    "admitted-tool-call": j_admitted_tool_call,
    "outside-run-silent": j_outside_run_silent,
    "fail-closed": j_fail_closed,
    "run-reservation": j_run_reservation,
    "unknown-slot": j_unknown_slot,
    "slot-clocks": j_clocks,
    "hook-input": j_hook_input,
    "call-identity": j_call_identity,
    "after-tool": lambda c: after_tool.journey(c, sys.modules[__name__]),
    "failed-tool": lambda c: failed_tool.journey(c, sys.modules[__name__]),
    "lifecycle": lambda c: lifecycle.journey(c, sys.modules[__name__]),
    "launcher": lambda c: launcher.journey(c, sys.modules[__name__]),
    "leases": lambda c: leases.journey(c, sys.modules[__name__]),
}


SOURCE_GLOBS = ("Cargo.toml", "Cargo.lock", "proto/**/*", "crates/**/src/**/*", "crates/**/build.rs", "crates/**/Cargo.toml")


def source_manifest():
    """sha256 of every authored workspace file the binaries depend on, plus an aggregate.

    Aggregate = sha256 over the lines "<repo-relative path>\\t<sha256 of file bytes>\\n", sorted by path.
    Unlike a `git status --porcelain` hash it changes whenever any listed file's bytes change.
    """
    files = {}
    for pat in SOURCE_GLOBS:
        for p in REPO.glob(pat):
            rel = p.relative_to(REPO)
            if p.is_file() and "target" not in rel.parts:
                files[rel.as_posix()] = sha256(p)
    lines = "".join(f"{k}\t{v}\n" for k, v in sorted(files.items()))
    return {"aggregate_sha256": hashlib.sha256(lines.encode()).hexdigest(), "file_count": len(files), "files": dict(sorted(files.items()))}


def provenance(args):
    git = lambda *a: sh(["git", *a], cwd=REPO).stdout
    bins = {}
    for n, p in binaries(args).items():
        if p.is_file():
            bins[n] = {"path": str(p), "sha256": sha256(p), "size": p.stat().st_size,
                       "mtime_utc": datetime.datetime.fromtimestamp(p.stat().st_mtime, datetime.timezone.utc).isoformat()}
    status = git("status", "--porcelain")
    return {"repo": str(REPO), "git_head": git("rev-parse", "HEAD").strip(),
            "git_status_porcelain_sha256": hashlib.sha256(status.encode()).hexdigest(),
            "git_status_porcelain_lines": len(status.splitlines()), "binaries": bins,
            "build_receipt": load_receipt(args),
            "platform": sys.platform, "python": sys.version.split()[0], "sqlite": sqlite3.sqlite_version,
            "target_dir": str(target_dir(args))}


def quiet_cleanup():
    import contextlib, io
    with contextlib.redirect_stdout(io.StringIO()):
        return cmd_cleanup()


def end_checks(args, manifest, start_receipt):
    """The run's end guard: what doctor vouched for at the start still holds after the journeys.

    These compare two endpoints only. An edit made and undone (or a binary swapped and swapped
    back) between them is invisible here, so the sources and target dir must still stay frozen
    while a drive runs.
    """
    out = []
    add = lambda name, ok, detail="": out.append({"name": name, "passed": bool(ok), "detail": detail})
    now = source_manifest()
    was, cur = manifest["files"], now["files"]
    changed = sorted(k for k in set(was) | set(cur) if was.get(k) != cur.get(k))
    add("end: sources unchanged since the run started", now["aggregate_sha256"] == manifest["aggregate_sha256"],
        f"start {manifest['aggregate_sha256']}, end {now['aggregate_sha256']}; changed {changed}")
    add("end: build receipt is the one doctor checked at the start", load_receipt(args) == start_receipt,
        str(receipt_path(args)))
    out.extend(dict(c, name=f"end: {c['name']}") for c in receipt_checks(args))
    return out


def cmd_drive(args):
    names = list(FEATURES) if args.feature == "all" else [args.feature]
    bad = [n for n in names if n not in FEATURES]
    if bad:
        print(f"unknown feature {bad}; known: {list(FEATURES)}")
        return 2
    if load_active():
        print(f"refusing to drive: {ACTIVE} lists unfinished harness state; run `verify.py cleanup` first")
        return 2
    ev = EVIDENCE_ROOT / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    ev.mkdir(parents=True)
    manifest = source_manifest()
    (ev / "sources.json").write_text(json.dumps(manifest, indent=2) + "\n")
    summary = {"started_utc": utc(), "evidence_dir": str(ev), **provenance(args),
               "sources": {"aggregate_sha256": manifest["aggregate_sha256"], "file_count": manifest["file_count"],
                           "per_file_list": "sources.json"}, "features": {}}
    doc = doctor_checks(args)
    summary["doctor"] = doc
    for d in doc:
        print(f"[doctor {'ok' if d['passed'] else 'FAIL'}] {d['name']}: {d['detail']}")
    if not all(d["passed"] for d in doc):
        summary.update(status="BLOCKED", finished_utc=utc())
        (ev / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(f"BLOCKED by doctor; summary at {ev}/summary.json")
        return 2
    for name in names:
        print(f"== {name}")
        c = Ctx(args, ev, name)
        started = utc()
        try:
            FEATURES[name](c)
        except Exception:
            c.check("journey completed without an exception", False, traceback=traceback.format_exc())
        summary["features"][name] = {"passed": bool(c.checks) and all(k["passed"] for k in c.checks), "started_utc": started,
                                     "finished_utc": utc(), "checks": c.checks, "commands": c.commands}
        quiet_cleanup()
    end = end_checks(args, manifest, summary["build_receipt"])
    summary["end_checks"] = end
    summary["sources"]["unchanged_during_run"] = end[0]["passed"]
    reasons = [f"feature {n} failed" for n, f in summary["features"].items() if not f["passed"]]
    reasons += [f"{c['name']} failed: {c['detail']}" for c in end if not c["passed"]]
    ok = not reasons
    summary.update(status="PASS" if ok else "FAIL", status_reasons=reasons, finished_utc=utc())
    (ev / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for c in end:
        print(f"[{'ok' if c['passed'] else 'FAIL'}] {c['name']}: {c['detail']}")
    for n, f in summary["features"].items():
        print(f"{'PASS' if f['passed'] else 'FAIL'} {n} ({sum(k['passed'] for k in f['checks'])}/{len(f['checks'])} checks)")
    for r in reasons:
        print(f"FAIL reason: {r}")
    print(f"{summary['status']}: evidence in {ev}")
    return 0 if ok else 1


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--target-dir", help="cargo target dir (default $CARGO_TARGET_DIR or <repo>/target)")
    p.add_argument("--scratch-root", default="/tmp", help="short parent dir for run dirs and sockets (default /tmp)")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("build")
    d = sub.add_parser("doctor")
    d.add_argument("--save", action="store_true")
    dr = sub.add_parser("drive")
    dr.add_argument("feature", help="feature id or 'all'")
    sub.add_parser("cleanup")
    sub.add_parser("list")
    args = p.parse_args()
    if not (REPO / "Cargo.toml").is_file():
        print(f"{REPO} is not the agent-runtime checkout")
        return 2
    if args.cmd == "list":
        print("\n".join(FEATURES))
        return 0
    return {"build": cmd_build, "doctor": cmd_doctor, "drive": cmd_drive, "cleanup": cmd_cleanup}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
