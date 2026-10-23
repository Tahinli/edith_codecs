#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ with synthetic AMR-NB files, one per mode.
# Needs ffmpeg with libopencore_amrnb. fuzz_container gets each .amr as-is;
# fuzz_nb_decode gets it with the 6-byte magic stripped (ToC + frames).
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
CONT="$FUZZ_DIR/corpus/fuzz_container"
DEC="$FUZZ_DIR/corpus/fuzz_nb_decode"
mkdir -p "$CONT" "$DEC"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

i=0
for rate in 4750 5150 5900 6700 7400 7950 10200 12200; do
    f="$TMP/m$i.amr"
    ffmpeg -nostdin -v error -f lavfi -i "sine=frequency=$((300 + 150 * i)):duration=2" \
        -ar 8000 -ac 1 -c:a libopencore_amrnb -b:a "$rate" -f amr "$f"
    cp -f "$f" "$CONT/mode$i.amr"
    tail -c +7 "$f" > "$DEC/mode$i"
    i=$((i + 1))
done
echo "seeded $i seeds per target"
