#!/usr/bin/env bash
set -uo pipefail
OUT=$HOME/lanes/testcut-p2-h265-c1/out
TOOLS=$PWD/lanes/testcut/p2h265-tools
NAMES="pixel_pipeline_runs_over_conformance_streams motion_derivation_is_deterministic_over_conformance_streams cu_syntax_sweep_over_conformance_streams ffmpeg_decodes_bit_exactly_at_every_shape noisy_near_black_lands_within_five_percent"
python3 "$TOOLS/analyze.py" "$OUT" $NAMES
cargo mutants -p ec-h265 --list --json > "$OUT/mutants.json" 2> "$OUT/mutants.list.err"
echo "mutants listed: $(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))))' "$OUT/mutants.json")"
for t in $NAMES; do python3 "$TOOLS/sample.py" "$OUT" "$t" 60; done
