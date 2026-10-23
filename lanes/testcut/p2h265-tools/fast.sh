#!/usr/bin/env bash
# Step 2: KT mutants through the fast profile (release). usage: fast.sh T ... (skips T with empty KT)
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
TOOLS=$PWD/lanes/testcut/p2h265-tools
OUT=$(dirname "$PWD")/out
cargo nextest run --release -p ec-h265 --profile fast --no-run > "$OUT/fastbuild.log" 2>&1 || { echo fast build failed; exit 1; }
S=$(date +%s)
cargo nextest run --release -p ec-h265 --profile fast --no-fail-fast > "$OUT/fastbase.log" 2>&1
echo "fast baseline rc=$? $(( $(date +%s) - S ))s"
SECS=$(( $(date +%s) - S ))
# tests failing unmutated are excluded (fixture/personal-media tests)
grep -E "^\s+(FAIL|TIMEOUT|SIGKILL|SIGSEGV|SIGABRT)" "$OUT/fastbase.log" | sed -E 's/^.*\] +(\([0-9]+\/[0-9]+\) +)?//' | awk '{print $1" "$2}' | sort -u > "$OUT/fast-excluded.txt"
echo "excluded:"; cat "$OUT/fast-excluded.txt"
EX=""
while read -r bin name; do [ -n "$name" ] && EX="$EX and not test(=$name)"; done < "$OUT/fast-excluded.txt"
TO=$(( (SECS * 5 + 1) / 2 + 5 ))
for T in "$@"; do
  [ -s "$OUT/KT-$T.txt" ] && ! grep -q baseline_failed "$OUT/KT-$T.txt" || { echo "$T: KT empty"; continue; }
  python3 -c "import json,sys;json.dump([l.rstrip('\n') for l in open(sys.argv[1]) if l.strip()],open(sys.argv[2],'w'))" "$OUT/KT-$T.txt" "$OUT/KT-$T.json"
  FAILFAST=1 python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/KT-$T.json" "F-$T" "$TO" "all()$EX" --profile fast > "$OUT/F-$T.log" 2>&1
  awk -F'\t' '$2=="SURVIVED"{print $1}' "$OUT/res-F-$T.tsv" > "$OUT/K-$T.txt"
  echo "$T fast statuses: $(cut -f2 "$OUT/res-F-$T.tsv" | sort | uniq -c | tr '\n' ' ') K=$(wc -l < "$OUT/K-$T.txt")"
done
echo FAST_DONE
