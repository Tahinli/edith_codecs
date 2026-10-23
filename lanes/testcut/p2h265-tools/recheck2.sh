#!/usr/bin/env bash
# Recheck the 3 ffmpeg T-timeout mutants on a quiet host: T alone (2.5x measured baseline), then fast profile.
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
TOOLS=$PWD/lanes/testcut/p2h265-tools
OUT=$(dirname "$PWD")/out
echo "load: $(cat /proc/loadavg)"
F="test(/(^|::)ffmpeg_decodes_bit_exactly_at_every_shape\$/)"
cargo nextest run --release -p ec-h265 -E "$F" --no-run >/dev/null 2>&1
S=$(date +%s); cargo nextest run --release -p ec-h265 -E "$F" > "$OUT/b2.log" 2>&1; echo "T baseline rc=$? $(( $(date +%s)-S ))s"
cat > "$OUT/names.json" <<'J'
["crates/ec-h265/src/residual.rs:609:42: replace + with - in residual_header","crates/ec-h265/src/ctu.rs:429:13: replace + with - in CtuEncoder<'a>::code_quadtree","crates/ec-h265/src/residual.rs:430:64: replace >> with << in rdoq"]
J
python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/names.json" R2-T 63 "$F"
python3 - <<'P'
import json,os
o=os.environ.get("OUT")
P
awk -F'\t' '$2=="KILLED"{print $1}' "$OUT/res-R2-T.tsv" > "$OUT/k2.txt"
python3 -c "import json,sys;json.dump([l.rstrip('\n') for l in open(sys.argv[1]) if l.strip()],open(sys.argv[2],'w'))" "$OUT/k2.txt" "$OUT/k2.json"
cargo nextest run --release -p ec-h265 --profile fast --no-run >/dev/null 2>&1
S=$(date +%s); cargo nextest run --release -p ec-h265 --profile fast --no-fail-fast > "$OUT/fb2.log" 2>&1; SECS=$(( $(date +%s)-S )); echo "fast baseline rc=$? ${SECS}s"
FAILFAST=1 python3 "$TOOLS/mutrun.py" "$OUT" "$OUT/k2.json" R2-F $(( SECS*5/2+5 )) "all()" --profile fast
cut -f1,2,3 "$OUT"/res-R2-*.tsv | cut -c1-200
echo R2_DONE
