#!/usr/bin/env bash
# Copy the seed streams from the shared JVT fixtures into the fuzz corpus.
# The corpus is not committed (see .gitignore); run this before a fuzz run.
set -euo pipefail

here=$(cd -- "$(dirname -- "$0")" && pwd)
src="$here/../../../fixtures/vectors/h264-jvt"
dst="$here/corpus/decode_annexb"

seeds=(
  CABA2_SVA_B/CABA2_SVA_B.264
  CANL3_SVA_B/CANL3_SVA_B.264
  BA3_SVA_C/BA3_SVA_C.264
  SL1_SVA_B/SL1_SVA_B.264
  AUD_MW_E/AUD_MW_E.264
)

mkdir -p "$dst"
for rel in "${seeds[@]}"; do
  f="$src/$rel"
  if [ ! -f "$f" ]; then
    echo "seed.sh: missing fixture $f (run scripts/link-fixtures.sh)" >&2
    exit 1
  fi
  cp -f "$f" "$dst/$(basename "$rel")"
done
# The avcC target shares the same seeds; it parses them as raw packets.
mkdir -p "$here/corpus/decode_avcc"
cp -f "$dst"/*.264 "$here/corpus/decode_avcc/"
echo "seed.sh: $(ls "$dst" | wc -l) seeds in $dst"
