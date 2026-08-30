#!/usr/bin/env bash
# Provision the libaom oracle the ec-av1 gates and every decode-verification
# claim depend on: the C source (read as ground truth) plus aomenc/aomdec.
#
# It lives under ~/.cache, NOT /tmp: on 2026-08-30 a tmpfs reclaim wiped
# /tmp/libaom-src and all 20 aomenc gates in crates/ec-av1/src/stream.rs
# started SKIPping, leaving a green-but-vacuous suite. Re-run this whenever
# `cargo test -p ec-av1` prints "SKIP ...: no aomenc at ...".
#
# Needs an assembler (nasm or yasm) for the SIMD build. Without one it falls
# back to AOM_TARGET_CPU=generic (C only) automatically -- decode is normative
# so aomdec stays bit-exact either way, but encode is slower, which matters
# because every gate shells out to aomenc 40 times. Install nasm for speed.
set -euo pipefail

VERSION="${AOM_VERSION:-v3.13.3}"
ROOT="${AOM_ORACLE_ROOT:-$HOME/.cache/aom-oracle}"
SRC="$ROOT/src"
BUILD="$ROOT/build"

if [ ! -d "$SRC/.git" ]; then
  git clone --depth 1 --branch "$VERSION" https://aomedia.googlesource.com/aom "$SRC"
fi

# A cmake build tree bakes in its own absolute path and its source's, so a
# build dir that was produced elsewhere and then moved cannot regenerate
# (`ninja: error: rebuilding build.ninja`) -- which silently costs you the
# ability to re-instrument aomdec later. Never stage a build under one name
# and rename it; configure it where it will live. If the cache disagrees with
# where we are now, drop it and reconfigure in place.
if [ -f "$BUILD/CMakeCache.txt" ] &&
   ! grep -q "^CMAKE_HOME_DIRECTORY:INTERNAL=$SRC$" "$BUILD/CMakeCache.txt"; then
  echo "build tree was configured for a different path -- reconfiguring in place" >&2
  rm -f "$BUILD/CMakeCache.txt"
  rm -rf "$BUILD/CMakeFiles"
fi

CPU_ARGS=()
if [ -n "${AOM_TARGET_CPU:-}" ]; then
  CPU_ARGS=(-DAOM_TARGET_CPU="$AOM_TARGET_CPU")
elif ! command -v nasm >/dev/null && ! command -v yasm >/dev/null; then
  echo "no nasm/yasm found -- falling back to the C-only (generic) build" >&2
  CPU_ARGS=(-DAOM_TARGET_CPU=generic)
fi

cmake -S "$SRC" -B "$BUILD" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DENABLE_TESTS=0 -DENABLE_DOCS=0 -DENABLE_EXAMPLES=1 \
  -DCONFIG_INSPECTION=1 \
  "${CPU_ARGS[@]}"
ninja -C "$BUILD" aomenc aomdec

echo "oracle ready:"
echo "  source  $SRC"
echo "  aomenc  $BUILD/aomenc"
echo "  aomdec  $BUILD/aomdec"
