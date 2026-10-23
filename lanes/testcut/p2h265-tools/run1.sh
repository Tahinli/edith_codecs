#!/usr/bin/env bash
# Step 1 (T only), one chain, RELEASE profile. usage: run1.sh T ...   (run from the synced tree; OUT = <lane>/out)
# Per candidate: build, measured unmutated baseline of T (wall seconds of the run), then SAMPLE_N (default 10)
# seeded mutants (sample2.py) each applied in place (mutrun.py), built, and run against T only with
# timeout = 2.5 x measured baseline. Writes KT-<T>.txt, S1-<T>.txt, res-B1-<T>.tsv, base-<T>.secs.
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
TOOLS=$PWD/lanes/testcut/p2h265-tools
OUT=$(dirname "$PWD")/out
SEED=2026
N=${SAMPLE_N:-10}
for T in "$@"; do
  F="test(/(^|::)$T\$/)"
  echo "== $T build $(date +%T)"
  cargo nextest run --release -p ec-h265 -E "$F" --no-run > "$OUT/build-$T.log" 2>&1 || { echo "$T build failed"; continue; }
  S=$(date +%s)
  cargo nextest run --release -p ec-h265 --no-fail-fast -E "$F" > "$OUT/base-$T.log" 2>&1
  BRC=$?; E=$(date +%s); SECS=$((E - S))
  echo "$SECS" > "$OUT/base-$T.secs"
  if [ "$BRC" -ne 0 ] || ! grep -qE "Starting [0-9]+ tests?" "$OUT/base-$T.log"; then
    echo "$T baseline_failed rc=$BRC"; echo "baseline_failed" > "$OUT/KT-$T.txt"; : > "$OUT/S1-$T.txt"; continue
  fi
  TO=$(( (SECS * 5 + 1) / 2 + 5 ))
  echo "baseline ${SECS}s timeout ${TO}s $(date +%T)"
  python3 "$TOOLS/sample2.py" "$OUT" "$T" "$N" "$SEED" || { echo "$T sample failed"; continue; }
  python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/s-$T.json" "B1-$T" "$TO" "$F" > "$OUT/B1-$T.log" 2>&1
  echo "exit $? $(date +%T)"
  awk -F'\t' '$2=="KILLED"{print $1}' "$OUT/res-B1-$T.tsv" > "$OUT/KT-$T.txt"
  awk -F'\t' '$2=="SURVIVED"{print $1}' "$OUT/res-B1-$T.tsv" > "$OUT/S1-$T.txt"
  echo "statuses: $(cut -f2 "$OUT/res-B1-$T.tsv" | sort | uniq -c | tr '\n' ' ')"
  echo "KT=$(wc -l < "$OUT/KT-$T.txt") S1=$(wc -l < "$OUT/S1-$T.txt")"
done
echo CHAIN_DONE
