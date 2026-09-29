#!/usr/bin/env bash
# Anti-regression check for the aomdec dump rungs that
# scripts/instrument-aom-oracle.sh derives.
#
# Why this exists: the four u8-narrowing rungs (PREFILT, POSTDEBLOCK,
# PREFILT_WIDE, POSTCDEF) shipped with `fwrite(plane + r*stride, 1, w, f)`,
# which on a 10/12-bit stream reads a HALVED plane pointer
# (CONVERT_TO_BYTEPTR in aom_ports/mem.h) and SIGSEGVs with a 0-byte file
# (lanes/av1oraclehbd.report.md). A rung defect that only shows up as an
# aomdec crash during a decode is found late; this reds on the DERIVATION
# instead, in seconds, without a build.
#
# What it does: copies the oracle source tree to a scratch dir, replaces
# av1/decoder/decodeframe.c with the PRISTINE upstream file, runs the
# instrument script over the copy, and asserts the derived file's shape. The
# real oracle tree is never written to.
#
# Usage:
#   scripts/check-aom-oracle-rungs.sh [SRC_DIR] [BASE_REF]
# Defaults: $AOM_ORACLE_SRC or ~/.cache/aom-oracle/src, and base ref v3.13.3
# (override with AOM_ORACLE_BASE_REF). Exit 0 = all assertions hold.
set -euo pipefail

SRC="${1:-${AOM_ORACLE_SRC:-$HOME/.cache/aom-oracle/src}}"
BASE="${2:-${AOM_ORACLE_BASE_REF:-v3.13.3}}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DERIVED="$SRC/av1/decoder/decodeframe.c"

[ -f "$DERIVED" ] || { echo "no oracle source at $DERIVED" >&2; exit 1; }
git -C "$SRC" rev-parse --verify "$BASE^{commit}" >/dev/null 2>&1 || {
  echo "base ref '$BASE' is not a commit in $SRC" >&2
  exit 1
}

WORK="$(mktemp -d "${TMPDIR:-$HOME/.cache/tmp}/aom-rung-check.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
cp -a "$SRC" "$WORK/src"
git -C "$SRC" show "$BASE:av1/decoder/decodeframe.c" > "$WORK/src/av1/decoder/decodeframe.c"

AOM_ORACLE_SRC="$WORK/src" bash "$HERE/instrument-aom-oracle.sh" > "$WORK/derive.log" 2>&1 || {
  echo "FAIL: instrument-aom-oracle.sh errored on a pristine tree:" >&2
  tail -20 "$WORK/derive.log" >&2
  exit 1
}
cp "$WORK/src/av1/decoder/decodeframe.c" "$WORK/derived.c"

# Re-running must be a no-op, or the repair pass is not idempotent and a
# second run would rewrite a tree under someone's feet.
AOM_ORACLE_SRC="$WORK/src" bash "$HERE/instrument-aom-oracle.sh" > "$WORK/rerun.log" 2>&1
cmp -s "$WORK/derived.c" "$WORK/src/av1/decoder/decodeframe.c" || {
  echo "FAIL: a second instrument-aom-oracle.sh run changed the derived file" >&2
  exit 1
}

fail=0
check() { # description, actual, expected
  if [ "$2" = "$3" ]; then
    printf 'ok   %-58s %s\n' "$1" "$2"
  else
    printf 'FAIL %-58s got %s, want %s\n' "$1" "$2" "$3" >&2
    fail=1
  fi
}

legacy=$(grep -c 'fwrite(ec_b->[yuv]_buffer + ec_r \* ec_b->' "$WORK/derived.c" || true)
check "legacy u8 row loops left in the derived file" "$legacy" "0"

rows=$(grep -c 'ec_dump_narrow_row(ec_f, ec_b, ec_b->[yuv]_buffer' "$WORK/derived.c" || true)
check "ec_dump_narrow_row call sites (4 rungs x 3 planes)" "$rows" "12"

fin=$(grep -c 'ec_dump_finish(ec_f, "EC_AV1_' "$WORK/derived.c" || true)
check "ec_dump_finish call sites (one per narrowing rung)" "$fin" "4"

for rung in EC_AV1_PREFILT_DUMP EC_AV1_POSTDEBLOCK_DUMP EC_AV1_PREFILT_WIDE_DUMP \
            EC_AV1_POSTCDEF_DUMP; do
  n=$(grep -c "ec_dump_finish(ec_f, \"$rung\"" "$WORK/derived.c" || true)
  check "byte-count check wired into $rung" "$n" "1"
done

# Rung 12 (EC_AV1_FINAL_DUMP) is the depth-correct one and must stay that way:
# it converts, it writes 2 bytes per sample, and it is not routed through the
# narrowing checker.
conv=$(grep -c 'CONVERT_TO_SHORTPTR(ec_p8\[ec_pl\])' "$WORK/derived.c" || true)
check "rung 12 still converts plane pointers" "$conv" "1"
r12=$(grep -c 'ec_dump_finish(ec_f, "EC_AV1_FINAL_DUMP"' "$WORK/derived.c" || true)
check "rung 12 not routed through the narrowing checker" "$r12" "0"

if [ "$fail" -ne 0 ]; then
  echo "FAIL: derived decodeframe.c violates the rung contract (base $BASE)" >&2
  exit 1
fi
echo "ok   instrument-aom-oracle.sh derives the depth-correct, byte-checked rungs (base $BASE)"
