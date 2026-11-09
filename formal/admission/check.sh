#!/usr/bin/env bash
# One command that checks the admission path of the coordinator: TLC on formal/admission/Admission.tla
# plus the real-code tests. Run it from anywhere; it works on the checkout it lives in.
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
      -cp "$JAR" tlc2.TLC -nowarning -workers 1 -metadir "$OUT/states-$name" \
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

  # Each broken variant must be caught, by the rule named here (mutants/<variant>.cfg checks
  # that one rule alone, so every rule is seen to fail at least once).
  for pair in commit_before_append:CoreMatchesTrace commit_before_append_ids:IdsConsecutive \
              ignore_append_error:AllowIsDurable no_tick:StampsIncrease \
              no_mutex:CallsDoNotCollide record_unknown_slot:DenyRecordsNothing \
              row_before_blob:RowsNameStoredBlobs; do
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
# 3. Real code: the existing unit test for the failed append, and the admission tests
# ---------------------------------------------------------------------------------------------
if [ "$want_code" = 1 ]; then
  mkdir -p "$OUT"
  cd "$ROOT" || exit 2
  if cargo test -p coordinator --lib failed_append_keeps_the_old_state --offline --locked >"$OUT/cargo-lib.log" 2>&1 \
     && grep -q "test adapter::tests::failed_append_keeps_the_old_state ... ok" "$OUT/cargo-lib.log"; then
    row "PASS  code: adapter::tests::failed_append_keeps_the_old_state"
  else
    fail=1
    row "FAIL  code: adapter::tests::failed_append_keeps_the_old_state (log $OUT/cargo-lib.log)"
  fi
  if cargo test -p coordinator --test admission_durability --offline --locked >"$OUT/cargo-admission.log" 2>&1 \
     && grep -q "test result: ok. 6 passed" "$OUT/cargo-admission.log"; then
    row "PASS  code: tests/admission_durability.rs (6 tests)"
  else
    fail=1
    row "FAIL  code: tests/admission_durability.rs: $(grep -E '^test .* FAILED|^error' "$OUT/cargo-admission.log" | head -3 | tr '\n' ' ')(log $OUT/cargo-admission.log)"
  fi
fi

echo
for r in "${rows[@]}"; do echo "$r"; done
echo
if [ "$fail" = 1 ]; then echo "RESULT: FAIL"; exit 1; fi
if [ "$drift" = 1 ]; then echo "RESULT: DRIFT (model and tests pass; the covered source changed since the sync record)"; exit 3; fi
echo "RESULT: PASS"
exit 0
