#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's mp4 fixtures (root fixtures/).
#
# demux_mp4  gets every mp4/m4a/mov fixture at 1 MiB or less as-is. Larger ones are
#            re-muxed with `ffmpeg -t 0.2 -c copy` (a few frames, still a real moov
#            with its sample tables), so the picture tracks are seeded too.
# esds_parse gets the payload of the first `esds` box in each seed, after its
#            version/flags word, found by a plain byte search for the box type.
# is_mp4     gets the first 64 bytes of each seed.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
FIXTURES=$(cd -- "$FUZZ_DIR/../../.." && pwd)/fixtures
[ -d "$FIXTURES" ] || { echo "seed.sh: $FIXTURES missing; run scripts/link-fixtures.sh" >&2; exit 2; }
command -v ffmpeg >/dev/null || { echo "seed.sh: ffmpeg needed for the small video seeds" >&2; exit 2; }

MP4_DIR="$FUZZ_DIR/corpus/demux_mp4"
ESDS_DIR="$FUZZ_DIR/corpus/esds_parse"
HEAD_DIR="$FUZZ_DIR/corpus/is_mp4"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/ec-mp4-seed.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$MP4_DIR" "$ESDS_DIR" "$HEAD_DIR"

LIMIT=$((1024 * 1024))
for src in "$FIXTURES"/audio/*.mp4 "$FIXTURES"/audio/*.m4a "$FIXTURES"/audio/*.mov \
           "$FIXTURES"/video/*.mp4 "$FIXTURES"/video/*.mov "$FIXTURES"/video/*.m4v; do
    [ -e "$src" ] || continue
    stem=$(basename "$src"); stem=${stem%.*}
    if [ "$(stat -c %s "$src")" -le "$LIMIT" ]; then
        cp "$src" "$MP4_DIR/$stem"
    else
        # Shorten until it fits: a 4K intra frame alone can be most of a megabyte.
        cut=""
        for secs in 0.2 0.05 0.01; do
            ffmpeg -v error -y -t "$secs" -i "$src" -c copy -map 0 -movflags +faststart "$TMP/$stem.mp4" \
                || { echo "seed.sh: ffmpeg could not cut $src" >&2; exit 2; }
            if [ "$(stat -c %s "$TMP/$stem.mp4")" -le "$LIMIT" ]; then cut=1; break; fi
        done
        [ -n "$cut" ] || { echo "seed.sh: $stem does not cut under 1 MiB" >&2; exit 2; }
        cp "$TMP/$stem.mp4" "$MP4_DIR/$stem"
    fi
done

python3 -I - "$MP4_DIR" "$ESDS_DIR" "$HEAD_DIR" <<'PY'
import os, sys

mp4_dir, esds_dir, head_dir = sys.argv[1], sys.argv[2], sys.argv[3]

def find_esds(data):
    """Payload after version/flags of the first `esds` box, or None."""
    at = data.find(b"esds")
    while at != -1:
        start = at - 4
        if start >= 0:
            size = int.from_bytes(data[start:start + 4], "big")
            if 12 <= size <= len(data) - start:
                return data[start + 12:start + size]
        at = data.find(b"esds", at + 1)
    return None

count = {"esds": 0, "head": 0}
for name in sorted(os.listdir(mp4_dir)):
    data = open(os.path.join(mp4_dir, name), "rb").read()
    open(os.path.join(head_dir, name), "wb").write(data[:64])
    count["head"] += 1
    payload = find_esds(data)
    if payload is not None:
        open(os.path.join(esds_dir, name), "wb").write(payload)
        count["esds"] += 1
print(f"seeded {len(os.listdir(mp4_dir))} demux_mp4, {count['esds']} esds_parse, {count['head']} is_mp4 seeds")
PY
