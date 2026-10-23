#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the committed tiny fixtures (tests/data).
#
# demux_ts  gets every tiny fixture as is, plus a cut of the first 64 KiB of each
#           so the tail-scan path sees short files.
# demux_ps  gets every ps_* fixture and a 64 KiB cut of each.
# ts_parts  gets the first 4 KiB of each.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail
FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
DATA="$FUZZ_DIR/../tests/data"
for t in demux_ts ts_parts demux_ps; do mkdir -p "$FUZZ_DIR/corpus/$t"; done
for f in "$DATA"/tiny_*; do
    stem=$(basename "$f"); stem=${stem%.*}_${f##*.}
    cp "$f" "$FUZZ_DIR/corpus/demux_ts/$stem"
    head -c 65536 "$f" > "$FUZZ_DIR/corpus/demux_ts/$stem.cut"
    head -c 4096 "$f" > "$FUZZ_DIR/corpus/ts_parts/$stem"
done
for f in "$DATA"/ps_*; do
    stem=$(basename "$f"); stem=${stem%.*}_${f##*.}
    cp "$f" "$FUZZ_DIR/corpus/demux_ps/$stem"
    head -c 65536 "$f" > "$FUZZ_DIR/corpus/demux_ps/$stem.cut"
done
echo "seeded $(ls "$FUZZ_DIR/corpus/demux_ts" | wc -l) demux_ts and $(ls "$FUZZ_DIR/corpus/ts_parts" | wc -l) ts_parts seeds"
