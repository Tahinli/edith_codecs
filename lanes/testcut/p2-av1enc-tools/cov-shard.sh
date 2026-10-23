#!/bin/sh
# Per-test region coverage of ec-av1 (release, instrumented). Run on a server in the synced repo root.
# usage: cov-shard.sh <testlist> ; prints TSV on stdout (see extract_regions.py); "FAST" = fast profile.
set -u
export EC_MEMGUARD_MAX=12G EC_AV1_AOM_DIR="$HOME/fixtures/av1-aom" EC_REQUIRE_FIXTURES=1 EC_TOOL_CACHE_DIR="$HOME/.cache/ec-tool-cache"
LIST=$1
TD=${CARGO_TARGET_DIR:-target}/llvm-cov-target
W=$(mktemp -d)
here=$(cd "$(dirname "$0")" && pwd)
clean() { find "$TD" -name '*.profraw' -delete 2>/dev/null; }
one() { # label, nextest filter args...
  label=$1; shift
  clean
  cargo llvm-cov nextest --release -p ec-av1 --no-report -j 4 --no-fail-fast "$@" >"$W/run.log" 2>&1
  rc=$?
  [ $rc -eq 0 ] || echo "RUNFAIL $label rc=$rc (coverage of this run is suspect)" >&2
  # A child that died mid-write leaves a truncated profraw that breaks the whole merge: drop it.
  PD=$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata
  find "$TD" -name '*.profraw' | while read -r f; do
    "$PD" show "$f" >/dev/null 2>&1 || { echo "BADPROF $label $f" >&2; rm -f "$f"; }
  done
  echo "PROFRAWS $label $(find "$TD" -name '*.profraw' | wc -l)" >&2
  cargo llvm-cov report --release --json --output-path "$W/cov.json" >"$W/report.out" 2>"$W/report.err" || { echo "REPORTFAIL $label" >&2; tail -n 20 "$W/report.err" >&2; }
  python3 "$here/extract_regions.py" "$label" "$W/cov.json"
  echo "DONE $label rc=$rc" >&2
}
one FAST --profile fast
while IFS= read -r T; do
  [ -n "$T" ] || continue
  one "$T" -E "test(=$T)"
done < "$LIST"
rm -rf "$W"
