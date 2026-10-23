#!/bin/bash
# Unmutated baseline in the same scratch layout as mutrun.sh (cwd = synced repo root).
# Runs the fast profile and the heavy union; any test failing here is excluded from kill attribution.
# usage: baseline.sh <outdir>
set -u
ROOT=$(pwd); OUT=$1
TMP=${TMPDIR:-$HOME/tmp}
# One fixed scratch path and its own target dir per lane: a test binary embeds the path it was built in
# (CARGO_MANIFEST_DIR), so reusing a binary built in another scratch dir breaks fixture lookups.
SCR="$TMP/scratch-av1e"
export CARGO_TARGET_DIR="$HOME/.cache/tgt-mut-$(basename "$TMP")"
export EC_MEMGUARD_MAX=${EC_MEMGUARD_MAX:-12G} EC_REQUIRE_FIXTURES=1 EC_AV1_AOM_DIR=${EC_AV1_AOM_DIR:-$HOME/fixtures/av1-aom} EC_TOOL_CACHE_DIR=${EC_TOOL_CACHE_DIR:-$HOME/.cache/ec-tool-cache}
mkdir -p "$OUT" "$SCR"
rsync -a --exclude=target/ --exclude=/.git --exclude=/fixtures "$ROOT/" "$SCR/"
ln -sfn "$ROOT/fixtures" "$SCR/fixtures"
M=lanes/testcut/p2-av1enc-tools/matrix
expr=$(awk 'NF{printf "%s%s", (n++?" | ":""), "test(=" $0 ")"}' $M/baseline-heavy.txt)
(cd "$SCR" && cargo nextest run --release -p ec-av1 --no-fail-fast --profile fast) > "$OUT/base-fast.log" 2>&1
echo "base fast rc=$?" >&2
(cd "$SCR" && cargo nextest run --release -p ec-av1 --no-fail-fast -E "$expr") > "$OUT/base-heavy.log" 2>&1
echo "base heavy rc=$?" >&2
grep -hE "^\s+(FAIL|TIMEOUT) \[" "$OUT/base-fast.log" "$OUT/base-heavy.log" | awk '{print $NF}' | sort -u > "$OUT/base.fail"
