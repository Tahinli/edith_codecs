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
# The same class of loss applies to the prediction rungs: EC_PREDOUT8 (rung
# 16) and EC_PREDND (rung 17) lived only in the hand-patched oracle tree, so
# a rebuild from the script dropped the only probe that prints on a 10/12-bit
# stream (both, and the pre-existing EC_PREDOUT, sit after an `is_hbd` early
# return). Those are asserted here too, from a PRISTINE reconintra.c.
#
# What it does: copies the oracle source tree to a scratch dir, replaces
# av1/decoder/decodeframe.c and av1/common/reconintra.c with the PRISTINE
# upstream files, runs the instrument script over the copy, and asserts the
# derived files' shape. The real oracle tree is never written to.
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
RECONINTRA="$SRC/av1/common/reconintra.c"

[ -f "$DERIVED" ] || { echo "no oracle source at $DERIVED" >&2; exit 1; }
[ -f "$RECONINTRA" ] || { echo "no oracle source at $RECONINTRA" >&2; exit 1; }
git -C "$SRC" rev-parse --verify "$BASE^{commit}" >/dev/null 2>&1 || {
  echo "base ref '$BASE' is not a commit in $SRC" >&2
  exit 1
}

WORK="$(mktemp -d "${TMPDIR:-$HOME/.cache/tmp}/aom-rung-check.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
cp -a "$SRC" "$WORK/src"
git -C "$SRC" show "$BASE:av1/decoder/decodeframe.c" > "$WORK/src/av1/decoder/decodeframe.c"
git -C "$SRC" show "$BASE:av1/common/reconintra.c" > "$WORK/src/av1/common/reconintra.c"

AOM_ORACLE_SRC="$WORK/src" bash "$HERE/instrument-aom-oracle.sh" > "$WORK/derive.log" 2>&1 || {
  echo "FAIL: instrument-aom-oracle.sh errored on a pristine tree:" >&2
  tail -20 "$WORK/derive.log" >&2
  exit 1
}
cp "$WORK/src/av1/decoder/decodeframe.c" "$WORK/derived.c"
cp "$WORK/src/av1/common/reconintra.c" "$WORK/derived-intra.c"

# Re-running must be a no-op, or the repair pass is not idempotent and a
# second run would rewrite a tree under someone's feet.
AOM_ORACLE_SRC="$WORK/src" bash "$HERE/instrument-aom-oracle.sh" > "$WORK/rerun.log" 2>&1
cmp -s "$WORK/derived.c" "$WORK/src/av1/decoder/decodeframe.c" || {
  echo "FAIL: a second instrument-aom-oracle.sh run changed the derived file" >&2
  exit 1
}
cmp -s "$WORK/derived-intra.c" "$WORK/src/av1/common/reconintra.c" || {
  echo "FAIL: a second instrument-aom-oracle.sh run changed reconintra.c" >&2
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

# --- rungs 16/17: the prediction rungs (lane-av1rungs17) --------------------
# EC_PREDOUT8 must be installed at BOTH 8-bit prediction sites in
# reconintra.c -- the non-directional early return (DC, SMOOTH*, PAETH) and
# the directional / filter-intra tail. A one-site install would look fine on
# the directional arm and print nothing at all for every non-directional
# block, which is most of an intra frame.
p8=$(grep -c 'EC_PREDOUT8 mi_row=%d' "$WORK/derived-intra.c" || true)
check "rung 16 EC_PREDOUT8 install sites (both 8-bit paths)" "$p8" "2"

# `mode=` is what makes a non-directional diff attributable at all; a rebuild
# that lost it would still install two sites and still pass the count above.
p8m=$(grep -c 'EC_PREDOUT8 mi_row=%d.*txh=%d mode=%d sum=%ld' "$WORK/derived-intra.c" || true)
check "rung 16 EC_PREDOUT8 sites carry mode= (EC_PREDND field order)" "$p8m" "2"

# EC_PREDND is the hbd twin and the ONLY prediction probe that fires on a
# 10/12-bit stream. One site, at the non-directional early return.
pnd=$(grep -c 'EC_PREDND mi_row=%d' "$WORK/derived-intra.c" || true)
check "rung 17 EC_PREDND install sites (hbd non-directional)" "$pnd" "1"
pndm=$(grep -c 'txw=%d txh=%d mode=%d n_top=%d n_left=%d' "$WORK/derived-intra.c" || true)
check "rung 17 EC_PREDND site carries mode=" "$pndm" "1"

# Rung 16 claims to be idempotent in BOTH directions: an install on a
# pristine file, AND an upgrade of the no-`mode=` form the oracle tree's git
# HEAD carries. Rebuild the no-mode form, re-derive, and require the result to
# land back on the same bytes -- an upgrade path that silently no-ops (or
# double-writes) is exactly what a rebuild would hit.
python3 - "$WORK/derived-intra.c" "$WORK/nomode-intra.c" <<'PYUP'
import sys
s = open(sys.argv[1]).read()
# Strip `mode=` from the format string and `mode` from the argument list at
# both EC_PREDOUT8 sites only; EC_PREDND's own mode= must survive untouched.
for fmt_old, fmt_new in (
    ('txw=%d txh=%d mode=%d sum=%ld row0=", xd->mi_row',
     'txw=%d txh=%d sum=%ld row0=", xd->mi_row'),
    ('txw=%d txh=%d mode=%d sum=%ld row0=", mi_row',
     'txw=%d txh=%d sum=%ld row0=", mi_row'),
):
    assert fmt_old in s, "upgrade fixture: %r not found" % fmt_old
    s = s.replace(fmt_old, fmt_new, 1)
assert s.count('col_off, txwpx, txhpx, mode, ec_sum);') == 2, "upgrade fixture: args"
s = s.replace('col_off, txwpx, txhpx, mode, ec_sum);',
              'col_off, txwpx, txhpx, ec_sum);')
assert 'EC_PREDOUT8 mi_row=%d' in s and s.count('txw=%d txh=%d sum=%ld row0=') == 2
open(sys.argv[2], 'w').write(s)
PYUP
cp "$WORK/nomode-intra.c" "$WORK/src/av1/common/reconintra.c"
AOM_ORACLE_SRC="$WORK/src" bash "$HERE/instrument-aom-oracle.sh" > "$WORK/upgrade.log" 2>&1 || {
  echo "FAIL: instrument-aom-oracle.sh errored on the no-mode reconintra.c:" >&2
  tail -20 "$WORK/upgrade.log" >&2
  exit 1
}
cmp -s "$WORK/derived-intra.c" "$WORK/src/av1/common/reconintra.c" || {
  echo "FAIL: re-deriving the no-mode EC_PREDOUT8 form did not land back on the derived bytes" >&2
  diff -u "$WORK/derived-intra.c" "$WORK/src/av1/common/reconintra.c" | head -30 >&2
  exit 1
}

if [ "$fail" -ne 0 ]; then
  echo "FAIL: a derived file violates the rung contract (base $BASE)" >&2
  exit 1
fi
echo "ok   instrument-aom-oracle.sh derives the depth-correct, byte-checked rungs (base $BASE)"
