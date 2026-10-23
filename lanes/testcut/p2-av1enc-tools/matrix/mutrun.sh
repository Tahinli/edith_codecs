#!/bin/bash
# Batched mutant runner (replaces cargo-mutants selection, whose --re does not select one mutant).
# Applies one sampled mutant with scripts/mutate.py in a scratch copy, runs the heavy tests of the
# mutant (or the fast profile), records the outcome, and reverts before the next mutant.
# usage (cwd = synced repo root): mutrun.sh <sample.json> <idx-file> <outdir> heavy|fast
set -u
ROOT=$(pwd)
SAMPLE=$1; IDX=$2; OUT=$3; MODE=$4
TMP=${TMPDIR:-$HOME/tmp}
# One fixed scratch path and its own target dir per lane: a test binary embeds the path it was built in
# (CARGO_MANIFEST_DIR), so reusing a binary built in another scratch dir breaks fixture lookups.
SCR="$TMP/scratch-av1e"
export CARGO_TARGET_DIR="$HOME/.cache/tgt-mut-$(basename "$TMP")"
export EC_MEMGUARD_MAX=${EC_MEMGUARD_MAX:-12G} EC_REQUIRE_FIXTURES=1 EC_AV1_AOM_DIR=${EC_AV1_AOM_DIR:-$HOME/fixtures/av1-aom} EC_TOOL_CACHE_DIR=${EC_TOOL_CACHE_DIR:-$HOME/.cache/ec-tool-cache}
mkdir -p "$OUT" "$SCR"
rsync -a --exclude=target/ --exclude=/.git --exclude=/fixtures "$ROOT/" "$SCR/"
# The top-level fixtures/ is a symlink on the host: tests read fixtures/video/... through ../../fixtures.
ln -sfn "$ROOT/fixtures" "$SCR/fixtures"
while IFS=$'\t' read -r idx tests; do
  [ -n "$idx" ] || continue
  rm -f "$OUT/$idx".status "$OUT/$idx".fail
  python3 "$ROOT/scripts/mutate.py" revert "$SCR"
  applied=$(python3 "$ROOT/scripts/mutate.py" apply "$SAMPLE" "$idx" "$SCR" 2>&1)
  echo "$applied" > "$OUT/$idx.applied"
  if [ "$MODE" = fast ]; then
    sel=(--profile fast)
  else
    expr=$(awk 'NF{printf "%s%s", (n++?" | ":""), "test(=" $0 ")"}' "$tests")
    sel=(-E "$expr")
  fi
  (cd "$SCR" && timeout "${HEAVY_TIMEOUT:-1200}" cargo nextest run --release -p ec-av1 --no-fail-fast "${sel[@]}") > "$OUT/$idx.log" 2>&1
  rc=$?
  python3 "$ROOT/scripts/mutate.py" revert "$SCR"
  # Verify the revert: the scratch copy must match the synced tree again for the mutated file.
  rel=$(python3 -c "import json,sys;print(json.load(open('$SAMPLE'))[int($idx)]['file'])")
  cmp -s "$ROOT/$rel" "$SCR/$rel" || echo "REVERT MISMATCH idx=$idx" >&2
  python3 "$ROOT/scripts/kill-matrix-analyze.py" evidence "$OUT/$idx.log" > "$OUT/$idx.evidence" 2>/dev/null
  if [ $rc -eq 124 ]; then
    status=timeout
  elif grep -qE "error\[E[0-9]+\]|could not compile" "$OUT/$idx.log" && ! grep -qE "^\s+(PASS|FAIL) \[" "$OUT/$idx.log"; then
    status=unviable
  elif grep -qE "^\s+(PASS|FAIL|TIMEOUT|SIGSEGV|SIGABRT) \[" "$OUT/$idx.log"; then
    if grep -qE "^\s+(FAIL|TIMEOUT|SIGSEGV|SIGABRT) \[" "$OUT/$idx.log"; then status=fail; else status=pass; fi
  else
    status=error
  fi
  if [ "$status" = fail ]; then
    grep -E "^\s+(FAIL|TIMEOUT|SIGSEGV|SIGABRT) \[" "$OUT/$idx.log" | awk '{print $NF}' | sort -u > "$OUT/$idx.fail"
  elif [ "$status" = timeout ] && [ "$MODE" = heavy ]; then
    # A hang counts as caught; the killer is unknown, so every covering test is recorded (conservative).
    cp "$tests" "$OUT/$idx.fail"
  fi
  echo "$status" > "$OUT/$idx.status"
  echo "mutrun: $MODE idx=$idx status=$status rc=$rc" >&2
done < "$IDX"
