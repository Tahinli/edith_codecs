#!/usr/bin/env bash
# usage: mut.sh TEST OTHERS_EXPR  (run from the synced tree; OTHERS_EXPR = nextest -E for remaining heavy tests, or empty)
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
OUT=$HOME/lanes/testcut-p2-h265-c1/out
T=$1; OTHERS=${2:-}
TOOLS=$PWD/lanes/testcut/p2h265-tools
TF="test(/(^|::)$T\$/)"
RE=$(python3 -c 'import json,re,sys; print("|".join(re.escape(n) for n in json.load(open(sys.argv[1]))))' "$OUT/sample-$T.json")
[ -n "$RE" ] || { echo "$T: empty sample"; exit 0; }
COMMON=(cargo mutants -p ec-h265 --test-tool nextest --baseline skip --timeout-multiplier 3 --re "$RE")
echo "== $T run A (fast suite) $(date +%T)"
"${COMMON[@]}" --cargo-test-arg=--profile --cargo-test-arg=fast -o "$OUT/$T.A" > "$OUT/$T.A.log" 2>&1
echo "A exit $?"
python3 "$TOOLS/outcomes.py" "$OUT/$T.A/mutants.out/outcomes.json" missed > "$OUT/$T.S.txt"
SN=$(wc -l < "$OUT/$T.S.txt")
echo "S=$SN survivors of fast suite"
[ "$SN" -eq 0 ] && { echo "$T: no survivors"; exit 0; }
RES=$(python3 -c 'import re,sys; print("|".join(re.escape(l.rstrip("\n")) for l in open(sys.argv[1]) if l.strip()))' "$OUT/$T.S.txt")
echo "== $T run B (T only) $(date +%T)"
cargo mutants -p ec-h265 --test-tool nextest --baseline skip --timeout-multiplier 3 --re "$RES" --cargo-test-arg=-E --cargo-test-arg="$TF" -o "$OUT/$T.B" > "$OUT/$T.B.log" 2>&1
echo "B exit $?"
python3 "$TOOLS/outcomes.py" "$OUT/$T.B/mutants.out/outcomes.json" killed > "$OUT/$T.K.txt"
KN=$(wc -l < "$OUT/$T.K.txt")
echo "K=$KN killed by T among S"
[ "$KN" -eq 0 ] && { echo "$T: K empty"; exit 0; }
KRE=$(python3 -c 'import re,sys; print("|".join(re.escape(l.rstrip("\n")) for l in open(sys.argv[1]) if l.strip()))' "$OUT/$T.K.txt")
[ -n "$OTHERS" ] || { echo "$T: no other heavy tests"; exit 0; }
echo "== $T run C (other heavy tests) $(date +%T)"
cargo mutants -p ec-h265 --test-tool nextest --baseline skip --timeout-multiplier 3 --re "$KRE" --cargo-test-arg=-E --cargo-test-arg="$OTHERS" -o "$OUT/$T.C" > "$OUT/$T.C.log" 2>&1
echo "C exit $?"
python3 "$TOOLS/outcomes.py" "$OUT/$T.C/mutants.out/outcomes.json" killed > "$OUT/$T.Kc.txt"
echo "K'=$(comm -23 <(sort "$OUT/$T.K.txt") <(sort "$OUT/$T.Kc.txt" 2>/dev/null) | wc -l) (see $T.Kp.txt)"; comm -23 <(sort "$OUT/$T.K.txt") <(sort "$OUT/$T.Kc.txt" 2>/dev/null) > "$OUT/$T.Kp.txt"
echo "$T done $(date +%T)"
