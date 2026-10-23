#!/usr/bin/env bash
# Phase 2 coverage pass for ec-h265 (runs on the server inside the synced tree).
set -uo pipefail
export EC_REQUIRE_FIXTURES=1
OUT=$HOME/lanes/testcut-p2-h265-c1/out
mkdir -p "$OUT"
PROF=${CARGO_TARGET_DIR:-$PWD/target}/llvm-cov-target
run_cov() {
  local name=$1; shift
  cargo llvm-cov clean --profraw-only > /dev/null 2>&1
  find "$PROF" -name '*.profraw' -delete 2>/dev/null
  cargo llvm-cov nextest --release --no-fail-fast -p ec-h265 --no-report "$@" > "$OUT/$name.run.log" 2>&1
  echo "$name nextest exit $?"
  PD=$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata
  for f in $(find "$PROF" -name '*.profraw' 2>/dev/null); do
    if ! "$PD" show "$f" > /dev/null 2>&1; then echo "$name: dropping corrupt $f"; rm -f "$f"; fi
  done
  cargo llvm-cov report --release --json --output-path "$OUT/$name.json" > "$OUT/$name.rep.log" 2>&1
  echo "$name report exit $?"
}
cargo llvm-cov clean --workspace > /dev/null 2>&1
run_cov fast --profile fast
for t in pixel_pipeline_runs_over_conformance_streams motion_derivation_is_deterministic_over_conformance_streams cu_syntax_sweep_over_conformance_streams ffmpeg_decodes_bit_exactly_at_every_shape noisy_near_black_lands_within_five_percent; do
  run_cov "$t" -E "test(/(^|::)$t\$/)"
done
grep -h -E "PASS|FAIL|Summary" "$OUT"/*.run.log | tail -40
