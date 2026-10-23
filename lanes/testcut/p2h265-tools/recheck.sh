#!/usr/bin/env bash
# Recheck the timeout-only rows with long timeouts. Runs from synced tree.
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
TOOLS=$PWD/lanes/testcut/p2h265-tools
OUT=$(dirname "$PWD")/out
EXN="$(awk '{print " and not test(=" $2 ")"}' "$OUT/fast-excluded.txt" | tr -d '\n')"
N="crates/ec-h265/src/ctu.rs:534:29: replace + with - in CtuEncoder<'a>::save_meta"
F="crates/ec-h265/src/ctu.rs:429:13: replace + with - in CtuEncoder<'a>::code_quadtree"
python3 -c "import json,sys;json.dump([sys.argv[1]],open(sys.argv[3],'w'));json.dump([sys.argv[2]],open(sys.argv[4],'w'))" "$N" "$F" "$OUT/rc-n.json" "$OUT/rc-f.json"
FAILFAST=1 python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/rc-n.json" rc-n-fast 1200 "all()$EXN" --profile fast
python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/rc-f.json" rc-f-T 600 "test(/(^|::)ffmpeg_decodes_bit_exactly_at_every_shape\$/)"
FAILFAST=1 python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/rc-f.json" rc-f-fast 1200 "all()$EXN" --profile fast
echo RECHECK_DONE
