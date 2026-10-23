#!/usr/bin/env bash
# Step 1 (T only). usage: run1.sh TEST:SECONDS ...   (run from the synced tree; OUT = <lane>/out)
# Per candidate: 20 seeded U(T) mutants (sample2.py), each applied from its own mutants.json diff
# (mutrun.py) and run against T only with timeout 3x T's runtime.
# Writes KT-<T>.txt (killed by T) and S1-<T>.txt (survived T alone), plus res-B1-<T>.tsv.
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
TOOLS=$PWD/lanes/testcut/p2h265-tools
OUT=$(dirname "$PWD")/out
mkdir -p "$OUT"
SEED=2026
for arg in "$@"; do
  T=${arg%%:*}; SECS=${arg##*:}; TO=$((SECS * 3))
  python3 "$TOOLS/sample2.py" "$OUT" "$T" 20 "$SEED" || { echo "$T sample failed"; continue; }
  echo "== $T timeout ${TO}s $(date +%T)"
  python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/s-$T.json" "B1-$T" "$TO" "test(/(^|::)$T\$/)" > "$OUT/B1-$T.log" 2>&1
  echo "exit $? $(date +%T)"
  awk -F'\t' '$2=="KILLED"{print $1}' "$OUT/res-B1-$T.tsv" > "$OUT/KT-$T.txt"
  awk -F'\t' '$2=="SURVIVED"{print $1}' "$OUT/res-B1-$T.tsv" > "$OUT/S1-$T.txt"
  echo "statuses: $(cut -f2 "$OUT/res-B1-$T.tsv" | sort | uniq -c | tr '\n' ' ')"
  echo "KT=$(wc -l < "$OUT/KT-$T.txt") S1=$(wc -l < "$OUT/S1-$T.txt")"
done
