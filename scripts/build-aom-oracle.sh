#!/usr/bin/env bash
# Provision the libaom oracle the ec-av1 gates and every decode-verification
# claim depend on: the C source (read as ground truth) plus aomenc/aomdec.
#
# It lives under ~/.cache, NOT /tmp: on 2026-08-30 a tmpfs reclaim wiped
# /tmp/libaom-src and all 20 aomenc gates in crates/ec-av1/src/stream.rs
# started SKIPping, leaving a green-but-vacuous suite. Re-run this whenever
# `cargo test -p ec-av1` prints "SKIP ...: no aomenc at ...".
#
# nasm/yasm are absent on this box, so the build uses AOM_TARGET_CPU=generic
# (C only). Decode is normative, so aomdec stays bit-exact; encode is slower.
# Install nasm and drop that flag for a faster oracle.
set -euo pipefail

VERSION="${AOM_VERSION:-v3.13.3}"
ROOT="${AOM_ORACLE_ROOT:-$HOME/.cache/aom-oracle}"
SRC="$ROOT/src"
BUILD="$ROOT/build"

if [ ! -d "$SRC/.git" ]; then
  git clone --depth 1 --branch "$VERSION" https://aomedia.googlesource.com/aom "$SRC"
fi

cmake -S "$SRC" -B "$BUILD" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DENABLE_TESTS=0 -DENABLE_DOCS=0 -DENABLE_EXAMPLES=1 \
  -DCONFIG_INSPECTION=1 \
  -DAOM_TARGET_CPU="${AOM_TARGET_CPU:-generic}"
ninja -C "$BUILD" aomenc aomdec

echo "oracle ready:"
echo "  source  $SRC"
echo "  aomenc  $BUILD/aomenc"
echo "  aomdec  $BUILD/aomdec"
