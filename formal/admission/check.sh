#!/usr/bin/env bash
# One command that checks the recording path of the coordinator (BeforeTool admissions, AfterTool
# completion reports, successful and failed, and Stop and SessionEnd lifecycle reports): TLC on
# formal/admission/Admission.tla plus the real-code tests. Run it from anywhere; it works on the
# checkout it lives in.
#
#   formal/admission/check.sh            model + code
#   formal/admission/check.sh --model    TLC only
#   formal/admission/check.sh --code     cargo tests only
#
# Exit 0: every section passed.  1: a rule, a self-test, or a test failed.  2: a tool is missing.
# 3: only the source-sync section failed (the covered files changed since the model was confirmed).
# Output of TLC and cargo goes to $FORMAL_OUT (default: <repo>/target/formal/admission), never into formal/.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=${FORMAL_OUT:-$ROOT/target/formal/admission}

# The pinned TLC. A different jar is a different tool: the check refuses it.
TLA_VERSION=v1.8.0
TLA_URL=https://github.com/tlaplus/tlaplus/releases/download/$TLA_VERSION/tla2tools.jar
TLA_SHA256=e1f11b75c4b5bc4478ba0ac5cb6ae28e40a01b0016bc33e0aa97da22dfdbcd7f
JAR=${TLA2TOOLS:-$ROOT/target/tla/tla2tools.jar}

want_model=1
want_code=1
case "${1:-}" in
  --model) want_code=0 ;;
  --code) want_model=0 ;;
  "") ;;
  *) echo "usage: $0 [--model|--code]" >&2; exit 2 ;;
esac

fail=0
drift=0
rows=()
row() { rows+=("$1"); }

sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }

# ---------------------------------------------------------------------------------------------
# Tools
# ---------------------------------------------------------------------------------------------
find_java() {
  local c
  for c in "${JAVA:-}" "${JAVA_HOME:+$JAVA_HOME/bin/java}" java \
           /Library/Java/JavaVirtualMachines/*/Contents/Home/bin/java; do
    [ -n "$c" ] || continue
    if "$c" -version >/dev/null 2>&1; then echo "$c"; return 0; fi
  done
  return 1
}

if [ "$want_model" = 1 ]; then
  JAVA_BIN=$(find_java) || {
    echo "TOOL MISSING: no working java (Java 11 or later). Set JAVA=/path/to/java." >&2
    exit 2
  }
  if [ ! -f "$JAR" ]; then
    mkdir -p "$(dirname "$JAR")"
    echo "tla2tools.jar not found at $JAR; downloading $TLA_VERSION"
    if ! curl -fsSL -o "$JAR" "$TLA_URL"; then
      rm -f "$JAR"
      echo "TOOL MISSING: the download was refused. Run this yourself, then rerun the check:" >&2
      echo "  curl -fsSL -o $JAR $TLA_URL" >&2
      exit 2
    fi
  fi
  got=$(sha256_of "$JAR")
  if [ "$got" != "$TLA_SHA256" ]; then
    echo "TOOL MISMATCH: $JAR has sha256 $got, expected $TLA_SHA256 ($TLA_VERSION)." >&2
    exit 2
  fi
  mkdir -p "$OUT/model" "$OUT/tmp"
  # TLC writes its trace files next to the spec, so run it on a copy under $OUT, not in formal/.
  cp "$HERE/Admission.tla" "$HERE"/*.cfg "$OUT/model/"
  mkdir -p "$OUT/model/mutants" && cp "$HERE"/mutants/*.cfg "$OUT/model/mutants/"
fi

# Runs TLC on one config; prints nothing; leaves the log at $OUT/<name>.log. TLC exits non-zero
# on a violation, so the exit status is not used: the log is read.
tlc() { # name config
  local name=$1 cfg=$2
  rm -rf "$OUT/states-$name"
  ( cd "$OUT/model" && "$JAVA_BIN" -XX:+UseParallelGC -Djava.io.tmpdir="$OUT/tmp" \
      -cp "$JAR" tlc2.TLC -nowarning -workers auto -metadir "$OUT/states-$name" \
      -config "$cfg" Admission.tla ) >"$OUT/$name.log" 2>&1
}

# The shortest path TLC found, as the actions in order.
path_of() { sed -n 's/^State [0-9]*: <\([A-Za-z]*\)(\("[a-z0-9]*"\)).*/\1(\2)/p' "$1" | tr '\n' ' '; }

# ---------------------------------------------------------------------------------------------
# 1. Model: baseline, witness, broken variants
# ---------------------------------------------------------------------------------------------
if [ "$want_model" = 1 ]; then
  echo "== TLC $TLA_VERSION (jar sha256 $got), $("$JAVA_BIN" -version 2>&1 | sed -n 's/.*version "\(.*\)".*/java \1/p' | head -1)"

  tlc baseline Admission.cfg
  if grep -q "Model checking completed. No error has been found." "$OUT/baseline.log"; then
    counts=$(grep -E "states generated, .* distinct states found" "$OUT/baseline.log" | sed 's/^ *//')
    depth=$(sed -n 's/^The depth of the complete state graph search is \([0-9]*\)\./\1/p' "$OUT/baseline.log")
    row "PASS  model baseline: all rules hold ($counts; depth $depth)"
  else
    fail=1
    row "FAIL  model baseline: $(grep -m1 -E '^Error:' "$OUT/baseline.log") path: $(path_of "$OUT/baseline.log") (log $OUT/baseline.log)"
  fi

  # The lifecycle baseline: the same rules with Stop and SessionEnd reports (kind L) among the calls.
  tlc lifecycle AdmissionLifecycle.cfg
  if grep -q "Model checking completed. No error has been found." "$OUT/lifecycle.log"; then
    counts=$(grep -E "states generated, .* distinct states found" "$OUT/lifecycle.log" | sed 's/^ *//')
    depth=$(sed -n 's/^The depth of the complete state graph search is \([0-9]*\)\./\1/p' "$OUT/lifecycle.log")
    row "PASS  model lifecycle baseline: all rules hold with lifecycle reports ($counts; depth $depth)"
  else
    fail=1
    row "FAIL  model lifecycle baseline: $(grep -m1 -E '^Error:' "$OUT/lifecycle.log") path: $(path_of "$OUT/lifecycle.log") (log $OUT/lifecycle.log)"
  fi

  tlc witness AdmissionWitness.cfg
  if grep -q "Invariant WitnessNotReached is violated" "$OUT/witness.log"; then
    row "PASS  witness: the model reaches 'a call failed, the retry reused its id and stamp' ($(path_of "$OUT/witness.log"))"
  else
    fail=1
    row "FAIL  witness: the case rule (c) is about is not reachable, so a pass would be vacuous (log $OUT/witness.log)"
  fi
  tlc orphan-witness AdmissionOrphanWitness.cfg
  if grep -q "Invariant OrphanNotReached is violated" "$OUT/orphan-witness.log"; then
    row "PASS  orphan witness: the model reaches 'a blob was stored, its row failed, no row names it' ($(path_of "$OUT/orphan-witness.log"))"
  else
    fail=1
    row "FAIL  orphan witness: the orphan case rule (f) allows is not reachable (log $OUT/orphan-witness.log)"
  fi
  tlc identity-witness AdmissionIdentityWitness.cfg
  if grep -q "Invariant IdentityRefusalNotReached is violated" "$OUT/identity-witness.log"; then
    row "PASS  identity witness: the model reaches 'a known slot's call refused for its identity' ($(path_of "$OUT/identity-witness.log"))"
  else
    fail=1
    row "FAIL  identity witness: no identity refusal is reachable, so rule (g) would be vacuous (log $OUT/identity-witness.log)"
  fi
  # The witnesses: each names a case a rule is about, and TLC must reach it.
  for spec in retry-identity:AdmissionRetryIdentityWitness:FailedIdentityNotRetried \
              completion-retry:AdmissionCompletionRetryWitness:CompletionRetryNotReached \
              repeat:AdmissionRepeatWitness:RepeatedCompletionNotReached \
              orphan-completion:AdmissionOrphanCompletionWitness:OrphanCompletionNotReached \
              out-of-order:AdmissionOutOfOrderWitness:OutOfOrderPairingNotReached \
              failure-retry:AdmissionFailureRetryWitness:FailureRetryNotReached \
              mixed-outcome:AdmissionMixedOutcomeWitness:MixedOutcomesNotReached \
              late-completion:AdmissionLateCompletionWitness:LateCompletionNotReached \
              lifecycle-retry:AdmissionLifecycleRetryWitness:LifecycleRetryNotReached \
              new-session:AdmissionNewSessionWitness:NewSessionNotReached; do
    name=${spec%%:*}; rest=${spec#*:}; cfg=${rest%%:*}; inv=${rest#*:}
    tlc "$name-witness" "$cfg.cfg"
    if grep -q "Invariant $inv is violated" "$OUT/$name-witness.log"; then
      row "PASS  $name witness: the model reaches the case $inv rules out ($(path_of "$OUT/$name-witness.log"))"
    else
      fail=1
      row "FAIL  $name witness: the case $inv rules out is not reachable, so its rule would be vacuous (log $OUT/$name-witness.log)"
    fi
  done

  # Each broken variant must be caught, by the rule named here (mutants/<variant>.cfg checks
  # that one rule alone, so every rule is seen to fail at least once).
  for pair in commit_before_append:CoreMatchesTrace commit_before_append_ids:IdsConsecutive \
              ignore_append_error:AllowIsDurable no_tick:StampsIncrease \
              no_mutex:CallsDoNotCollide record_unknown_slot:DenyRecordsNothing \
              row_before_blob:RowsNameStoredBlobs no_binding_check:SessionStaysOnSlot \
              no_repeat_check:CallRecordedOnce identity_before_append:CoreMatchesTrace \
              input_blob_after_row:RowsNameStoredBlobs no_tool_check:CallKeepsItsTool \
              guess_before:AfterPairsWithItsBefore last_completion:CoreMatchesTrace \
              completion_before_append:CoreMatchesTrace \
              failure_as_success:RowKeepsItsOutcome success_only_completion:CoreMatchesTrace \
              lifecycle_forgets_calls:AfterPairsWithItsBefore lifecycle_unbinds:SessionStaysOnSlot; do
    m=${pair%%:*}; rule=${pair##*:}
    tlc "mutant-$m" "mutants/$m.cfg"
    if grep -q "Invariant $rule is violated" "$OUT/mutant-$m.log"; then
      row "PASS  broken variant $m: TLC fails on $rule ($(path_of "$OUT/mutant-$m.log"))"
    else
      fail=1
      row "FAIL  broken variant $m was NOT caught on $rule: $(grep -m1 -E '^(Error:|Model checking completed)' "$OUT/mutant-$m.log") (log $OUT/mutant-$m.log)"
    fi
  done
fi

# ---------------------------------------------------------------------------------------------
# 2. Source sync: the files the model was confirmed against
# ---------------------------------------------------------------------------------------------
if [ "$want_model" = 1 ]; then
  if (cd "$ROOT" && shasum -a 256 -c "$HERE/SYNC.sha256" >"$OUT/sync.log" 2>&1); then
    row "PASS  sync: covered source files match the content the model was confirmed against"
  else
    drift=1
    row "DRIFT sync: $(grep -v ': OK$' "$OUT/sync.log" | tr '\n' ' ')-- reread the coverage table in the skill, then refresh SYNC.sha256 (log $OUT/sync.log)"
  fi
fi

# ---------------------------------------------------------------------------------------------
# 3. Real code: the adapter's failed-append unit tests, the admission, AfterTool, failure and
#    lifecycle durability tests
# ---------------------------------------------------------------------------------------------
if [ "$want_code" = 1 ]; then
  mkdir -p "$OUT"
  cd "$ROOT" || exit 2
  if cargo test -p coordinator --lib adapter::tests::failed_ --offline --locked >"$OUT/cargo-lib.log" 2>&1 \
     && grep -q "test adapter::tests::failed_append_keeps_the_old_state ... ok" "$OUT/cargo-lib.log" \
     && grep -q "test adapter::tests::failed_after_tool_append_completes_nothing ... ok" "$OUT/cargo-lib.log" \
     && grep -q "test adapter::tests::failed_failure_report_append_completes_nothing ... ok" "$OUT/cargo-lib.log" \
     && grep -q "test adapter::tests::failed_lifecycle_append_binds_nothing_and_keeps_the_calls ... ok" "$OUT/cargo-lib.log"; then
    row "PASS  code: adapter::tests::failed_append_keeps_the_old_state, failed_after_tool_append_completes_nothing, failed_failure_report_append_completes_nothing, failed_lifecycle_append_binds_nothing_and_keeps_the_calls"
  else
    fail=1
    row "FAIL  code: adapter::tests::failed_* (log $OUT/cargo-lib.log)"
  fi
  if cargo test -p coordinator --test admission_durability --offline --locked >"$OUT/cargo-admission.log" 2>&1 \
     && grep -q "test result: ok. 8 passed" "$OUT/cargo-admission.log"; then
    row "PASS  code: tests/admission_durability.rs (8 tests)"
  else
    fail=1
    row "FAIL  code: tests/admission_durability.rs: $(grep -E '^test .* FAILED|^error' "$OUT/cargo-admission.log" | head -3 | tr '\n' ' ')(log $OUT/cargo-admission.log)"
  fi
  if cargo test -p coordinator --test after_tool_durability --offline --locked >"$OUT/cargo-after-tool.log" 2>&1 \
     && grep -q "test result: ok. 6 passed" "$OUT/cargo-after-tool.log"; then
    row "PASS  code: tests/after_tool_durability.rs (6 tests)"
  else
    fail=1
    row "FAIL  code: tests/after_tool_durability.rs: $(grep -E '^test .* FAILED|^error' "$OUT/cargo-after-tool.log" | head -3 | tr '\n' ' ')(log $OUT/cargo-after-tool.log)"
  fi
  if cargo test -p coordinator --test failure_durability --offline --locked >"$OUT/cargo-failure.log" 2>&1 \
     && grep -q "test result: ok. 5 passed" "$OUT/cargo-failure.log"; then
    row "PASS  code: tests/failure_durability.rs (5 tests)"
  else
    fail=1
    row "FAIL  code: tests/failure_durability.rs: $(grep -E '^test .* FAILED|^error' "$OUT/cargo-failure.log" | head -3 | tr '\n' ' ')(log $OUT/cargo-failure.log)"
  fi
  if cargo test -p coordinator --test lifecycle_durability --offline --locked >"$OUT/cargo-lifecycle.log" 2>&1 \
     && grep -q "test result: ok. 5 passed" "$OUT/cargo-lifecycle.log"; then
    row "PASS  code: tests/lifecycle_durability.rs (5 tests)"
  else
    fail=1
    row "FAIL  code: tests/lifecycle_durability.rs: $(grep -E '^test .* FAILED|^error' "$OUT/cargo-lifecycle.log" | head -3 | tr '\n' ' ')(log $OUT/cargo-lifecycle.log)"
  fi
fi

echo
for r in "${rows[@]}"; do echo "$r"; done
echo
if [ "$fail" = 1 ]; then echo "RESULT: FAIL"; exit 1; fi
if [ "$drift" = 1 ]; then echo "RESULT: DRIFT (model and tests pass; the covered source changed since the sync record)"; exit 3; fi
echo "RESULT: PASS"
exit 0
