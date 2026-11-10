---
name: verification-pass-needs-frozen-inputs
description: a verify.py drive PASS needs its end checks; endpoint fingerprints miss an edit undone mid-run, so freeze sources and binaries while driving
metadata:
  type: project
---

Until the issue #3 AfterTool repair round 2 (2026-11-09), `verify.py drive` set PASS from journey checks alone. It recorded `sources.unchanged_during_run` but never acted on it, and it checked the build receipt only at the start. So a source edit or a rebuilt binary during a long `drive all` still produced PASS evidence that matched no single build. The drive now reruns the source fingerprint and the receipt and binary checks at the end, and any mismatch makes the run FAIL. `scripts/freshness_proof.py` proves this with the real binaries.

**Why:** evidence is only worth something if it describes one build. A guard that is recorded but does not decide the verdict gets ignored.

**How to apply:** treat a verifier's provenance fields as gates, not annotations. The end checks compare two endpoints, so a change that is made and undone during the run goes unseen. While a drive runs, one owner keeps the sources and the target dir frozen: no edits, and no cargo command on that target dir. In round 2, a `cargo test --workspace` and `cargo clippy` run after `verify.py build` relinked both binaries with sources unchanged, and doctor caught it. Two builds of the same sources also gave different binary hashes. So run `verify.py build` last, right before `drive`. Related: [[restored-sources-need-touch]], [[unix-socket-tests-need-local-binding]].
