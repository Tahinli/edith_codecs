#!/usr/bin/env bash
# Smoke: two mutants from the union sample through the fast suite, to time one mutant and check the runner.
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
OUT=$HOME/lanes/testcut-p2-h265-c1/out
mkdir -p "$OUT"
python3 -c 'import json,sys; print("\n".join(json.load(open(sys.argv[1]))[:2]))' "$OUT/sample-pixel_pipeline_runs_over_conformance_streams.json" > "$OUT/smoke.names"
RE=$(python3 -c 'import re,sys; print("|".join(re.escape(l.rstrip("\n")) for l in open(sys.argv[1]) if l.strip()))' "$OUT/smoke.names")
date +%T
cargo mutants -p ec-h265 --test-tool nextest --baseline skip --timeout-multiplier 3 --re "$RE" --cargo-test-arg=--profile --cargo-test-arg=fast -o "$OUT/smoke" > "$OUT/smoke.log" 2>&1
echo "exit $?"; date +%T
tail -5 "$OUT/smoke.log"
