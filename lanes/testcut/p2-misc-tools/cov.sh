#!/usr/bin/env bash
# Usage: cov.sh <crate> <out-dir> <test-name|fast>
# Writes <out-dir>/<test-name|fast>.json (llvm-cov JSON). Runs under the synced tree root.
# Test failures (wall-clock tests under instrumentation) do not stop the report; the run's
# exit code is printed as run-exit=N. A truncated profraw from a child that died mid-write
# is deleted before the report, because one bad profraw makes the whole merge fail.
set -uo pipefail
crate=$1; out=$2; t=$3
mkdir -p "$out"
if [ "$t" = fast ]; then sel=(--profile fast); else sel=(-E "test(=$t)"); fi
cargo llvm-cov clean --workspace >/dev/null 2>&1
cargo llvm-cov nextest --release -p "$crate" "${sel[@]}" --no-fail-fast --no-report
echo "run-exit=$?"
D="${CARGO_TARGET_DIR:-target}/llvm-cov-target"
PD="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata"
[ -x "$PD" ] || PD=llvm-profdata
bad=0
while IFS= read -r f; do
  if ! "$PD" show "$f" >/dev/null 2>&1; then rm -f "$f"; bad=$((bad+1)); fi
done < <(find "$D" -name '*.profraw' 2>/dev/null)
echo "dropped-bad-profraw=$bad"
cargo llvm-cov report --release --json --output-path "$out/$t.json"
echo "cov-exit=$?"
