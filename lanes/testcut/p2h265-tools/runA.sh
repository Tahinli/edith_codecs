#!/usr/bin/env bash
# Shared run A: fast suite vs union of all candidate samples. Writes $OUT/A.S.txt (survivors).
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
OUT=$HOME/lanes/testcut-p2-h265-c1/out
TOOLS=$PWD/lanes/testcut/p2h265-tools
python3 -c 'import json,glob
u=[]
for f in sorted(glob.glob("'$OUT'/sample-*.json")):
    for n in json.load(open(f)):
        if n not in u: u.append(n)
json.dump(u, open("'$OUT'/union.json","w")); print("union", len(u))'
RE=$(python3 -c 'import json,re,sys; print("|".join(re.escape(n) for n in json.load(open(sys.argv[1]))))' "$OUT/union.json")
cargo mutants --help 2>&1 | grep -E "^ +--(release|test-tool|cargo-test-arg|in-place|baseline)" | head -6
cargo mutants -p ec-h265 --test-tool nextest --baseline skip --timeout-multiplier 3 --re "$RE" --cargo-test-arg=--profile --cargo-test-arg=fast -o "$OUT/A" > "$OUT/A.log" 2>&1
echo "A exit $?"
python3 "$TOOLS/outcomes.py" "$OUT/A/mutants.out/outcomes.json" missed > "$OUT/A.S.txt"
echo "S=$(wc -l < "$OUT/A.S.txt")"
python3 -c 'import json,collections; print(collections.Counter(o.get("summary") for o in json.load(open("'$OUT'/A/mutants.out/outcomes.json"))["outcomes"]))' 2>/dev/null
