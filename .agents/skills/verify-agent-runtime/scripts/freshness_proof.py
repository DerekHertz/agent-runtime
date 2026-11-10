#!/usr/bin/env python3
"""Proof that `verify.py drive` refuses PASS when sources or binaries drift during the run.

    freshness_proof.py [--feature admitted-tool-call] [--scratch-root /tmp] [--target-dir DIR]

Three scenarios, each a real `verify.cmd_drive` of one existing journey against the real binaries:

    baseline  nothing changes                                      -> PASS, exit 0
    source    one source file of the copy gets a comment appended  -> FAIL, exit 1
    binary    the copied art-shim gets one byte appended           -> FAIL, exit 1

Each scenario runs on its own isolated copy: the files `verify.source_manifest()` lists, plus
art-coordinator, art-shim and the build receipt copied from the real target dir. The copy has the
same source aggregate as the checkout, so the copied receipt holds and doctor passes as it would
on the checkout. The only injection is a wrapper around the journey's FEATURES entry: it runs the
original journey, and only once every one of its checks passed does it change the copy, after the
journey and before cmd_drive's verdict. So a FAIL in the drift scenarios comes from the end guard
alone, never from a journey check.

Needs `verify.py build` first (the real receipt must match the checkout) and Unix-socket binding,
like `verify.py drive`. Exit 0 proven, 1 a scenario went wrong, 2 blocked. The checkout's sources,
binaries and receipt are hashed before and after and must be unchanged. Copies are removed;
evidence is kept under <repo>/.claude/reviews/evidence/verify-agent-runtime-freshness/<UTC>/.
"""
import argparse, datetime, hashlib, json, pathlib, shutil, sys, tempfile

sys.dont_write_bytecode = True
import verify  # noqa: E402  the real harness, from this directory

REAL_REPO = verify.REPO
PROOF_ROOT = REAL_REPO / ".claude/reviews/evidence/verify-agent-runtime-freshness"
SOURCE_TO_PERTURB = "crates/coordinator/src/core/mod.rs"
SCENARIOS = {"baseline": (0, "PASS", None),
             "source": (1, "FAIL", "end: sources unchanged since the run started"),
             "binary": (1, "FAIL", "end: art-shim unchanged since that build")}


def real_state(args):
    """Hashes of what the proof must never touch: the checkout's sources, binaries and receipt."""
    receipt = verify.receipt_path(args)
    return {"sources": verify.source_manifest()["aggregate_sha256"],
            "binaries": {n: verify.sha256(p) if p.is_file() else None for n, p in verify.binaries(args).items()},
            "receipt": verify.sha256(receipt) if receipt.is_file() else None}


def isolate(args, files, root):
    """Copy the listed sources, both binaries and the receipt under root; return the copy's args."""
    repo, target = root / "repo", root / "target"
    for rel in files:
        dst = repo / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(REAL_REPO / rel, dst)
    (target / "debug").mkdir(parents=True)
    for name, p in verify.binaries(args).items():
        shutil.copy2(p, target / "debug" / name)  # keeps the executable bit
    shutil.copyfile(verify.receipt_path(args), target / "debug" / verify.receipt_path(args).name)
    return repo, argparse.Namespace(target_dir=str(target), scratch_root=args.scratch_root, feature=args.feature)


def perturb(kind, repo, cargs):
    if kind == "source":
        with open(repo / SOURCE_TO_PERTURB, "a") as f:
            f.write("\n// freshness proof: changed after the journey passed\n")
    elif kind == "binary":
        with open(verify.binaries(cargs)["art-shim"], "ab") as f:
            f.write(b"\0")


def scenario(kind, args, files, ev):
    """One real drive on its own copy; return a record with the outcome and its own checks."""
    root = pathlib.Path(tempfile.mkdtemp(prefix="artv-fresh-"))
    saved = {k: getattr(verify, k) for k in ("REPO", "EVIDENCE_ROOT", "ACTIVE")}
    original = verify.FEATURES[args.feature]
    rec, summary = {"scenario": kind, "perturbed": False}, {}
    assert SOURCE_TO_PERTURB in files, SOURCE_TO_PERTURB
    try:
        repo, cargs = isolate(args, files, root)
        verify.REPO, verify.EVIDENCE_ROOT = repo, ev / kind
        verify.ACTIVE = verify.EVIDENCE_ROOT / "active-processes.json"
        copy_aggregate = verify.source_manifest()["aggregate_sha256"]
        rec["copy_matches_checkout"] = copy_aggregate == saved_aggregate(files)

        def wrapped(c):
            original(c)
            if kind != "baseline" and c.checks and all(k["passed"] for k in c.checks):
                perturb(kind, repo, cargs)
                rec["perturbed"] = True

        verify.FEATURES[args.feature] = wrapped
        rec["exit_code"] = verify.cmd_drive(cargs)
        runs = sorted(p for p in verify.EVIDENCE_ROOT.iterdir() if (p / "summary.json").is_file())
        summary = json.loads((runs[-1] / "summary.json").read_text()) if runs else {}
        rec["summary"] = str(runs[-1] / "summary.json") if runs else None
    finally:
        verify.FEATURES[args.feature] = original
        for k, v in saved.items():
            setattr(verify, k, v)
        shutil.rmtree(root, ignore_errors=True)
    feature = summary.get("features", {}).get(args.feature, {})
    rec.update(status=summary.get("status"), status_reasons=summary.get("status_reasons"),
               feature_checks=f"{sum(k['passed'] for k in feature.get('checks', []))}/{len(feature.get('checks', []))}",
               failed_end_checks=[c["name"] for c in summary.get("end_checks", []) if not c["passed"]],
               copy_removed=not root.exists())
    return rec, feature


def saved_aggregate(files):
    lines = "".join(f"{k}\t{v}\n" for k, v in sorted(files.items()))
    return hashlib.sha256(lines.encode()).hexdigest()


def judge(kind, rec, feature):
    want_code, want_status, want_end = SCENARIOS[kind]
    checks = feature.get("checks", [])
    expect = {
        "the copy hashes like the checkout": rec.get("copy_matches_checkout"),
        f"exit {want_code}": rec.get("exit_code") == want_code,
        f"status {want_status}": rec.get("status") == want_status,
        "every journey check passed": bool(checks) and all(k["passed"] for k in checks) and feature.get("passed"),
        "the isolated copy was removed": rec["copy_removed"],
    }
    if want_end is None:
        expect["no end check failed"] = rec.get("failed_end_checks") == [] and rec.get("status_reasons") == []
    else:
        expect["the copy was changed after the journey passed"] = rec["perturbed"]
        expect[f"'{want_end}' failed"] = want_end in (rec.get("failed_end_checks") or [])
        expect["no reason blames a feature"] = not any(r.startswith("feature ") for r in rec.get("status_reasons") or [])
    rec["expectations"] = {k: bool(v) for k, v in expect.items()}
    return all(rec["expectations"].values())


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--feature", default="admitted-tool-call", help="one journey id from `verify.py list`")
    p.add_argument("--target-dir", help="the real cargo target dir (default $CARGO_TARGET_DIR or <repo>/target)")
    p.add_argument("--scratch-root", default="/tmp", help="short parent dir for run dirs and sockets (default /tmp)")
    args = p.parse_args()
    if args.feature not in verify.FEATURES:
        print(f"unknown feature {args.feature!r}; known: {list(verify.FEATURES)}")
        return 2
    pre = [c for c in verify.receipt_checks(args) if not c["passed"]]
    if pre:
        print("BLOCKED: the checkout's binaries are not fresh; run `verify.py build` first")
        for c in pre:
            print(f"  [FAIL] {c['name']}: {c['detail']}")
        return 2
    ev = PROOF_ROOT / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    ev.mkdir(parents=True)
    before = real_state(args)
    files = verify.source_manifest()["files"]
    results, verdict = [], 0
    for kind in SCENARIOS:
        print(f"=== scenario {kind}")
        rec, feature = scenario(kind, args, files, ev)
        if rec.get("status") == "BLOCKED":
            rec["expectations"] = {}
            results.append(rec)
            print(f"BLOCKED: scenario {kind} could not drive (see {rec.get('summary')})")
            verdict = 2
            break
        good = judge(kind, rec, feature)
        results.append(rec)
        for k, v in rec["expectations"].items():
            print(f"  [{'ok' if v else 'FAIL'}] {kind}: {k}")
        verdict = verdict or (0 if good else 1)
    after = real_state(args)
    untouched = after == before
    print(f"  [{'ok' if untouched else 'FAIL'}] the checkout's sources, binaries and receipt are unchanged")
    if not untouched:
        verdict = 1
    status = {0: "PROVEN", 1: "FAIL", 2: "BLOCKED"}[verdict]
    (ev / "proof.json").write_text(json.dumps({"status": status, "feature": args.feature, "checkout_before": before,
                                               "checkout_after": after, "scenarios": results}, indent=2) + "\n")
    print(f"{status}: freshness proof, evidence in {ev}/proof.json")
    return verdict


if __name__ == "__main__":
    sys.exit(main())
