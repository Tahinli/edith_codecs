#!/usr/bin/env bash
# Provision an aomenc that can emit AFFINE global motion.
#
# Stock libaom NEVER writes a 6-parameter (AFFINE) global-motion model: its
# encoder-side model search is pinned to ROTZOOM,
#
#   av1/encoder/global_motion_facade.c:24
#     #define FIRST_GLOBAL_TRANS_TYPE ROTZOOM
#     #define LAST_GLOBAL_TRANS_TYPE  ROTZOOM
#
# so no `--enable-global-motion=1` recipe over any content can produce the
# AFFINE case the decoder must still handle (the *bitstream* allows it, the
# spec's `global_motion_params` reads 6 params, and ffmpeg decodes it). This
# builds a side copy of the oracle source with that search range widened to
# AFFINE -- decode-normative output, still ordinary libaom, still verified
# against ffmpeg -- into its own prefix so the shared oracle at
# ~/.cache/aom-oracle (used by every other gate) is untouched.
#
# Used by the `a_real_affine_global_motion_stream_decodes_pixel_exact` gate in
# crates/ec-av1/src/stream.rs.
set -euo pipefail

ROOT="${AOM_AFFINE_ROOT:-$HOME/.cache/aom-affine}"
ORACLE_SRC="${AOM_ORACLE_ROOT:-$HOME/.cache/aom-oracle}/src"
SRC="$ROOT/src"
BUILD="$ROOT/build"

if [ ! -d "$ORACLE_SRC/.git" ]; then
  echo "no oracle source at $ORACLE_SRC -- run scripts/build-aom-oracle.sh first" >&2
  exit 1
fi

if [ ! -d "$SRC" ]; then
  mkdir -p "$ROOT"
  cp -a "$ORACLE_SRC" "$SRC"
fi

facade="$SRC/av1/encoder/global_motion_facade.c"
sed -i 's/^#define LAST_GLOBAL_TRANS_TYPE ROTZOOM$/#define LAST_GLOBAL_TRANS_TYPE AFFINE/' "$facade"
grep -q '^#define LAST_GLOBAL_TRANS_TYPE AFFINE$' "$facade" ||
  { echo "patch did not apply to $facade" >&2; exit 1; }

cmake -S "$SRC" -B "$BUILD" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DENABLE_TESTS=0 -DENABLE_DOCS=0 -DENABLE_EXAMPLES=1
ninja -C "$BUILD" aomenc

echo "affine-capable encoder ready: $BUILD/aomenc"
