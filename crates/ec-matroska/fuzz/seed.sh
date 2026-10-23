#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's Matroska fixtures (fixtures/audio and
# fixtures/video, which hold the .mka and .mkv files).
#
# open_tracks and demux_packets get each fixture as-is, when it is 1 MiB or less.
# Larger fixtures are cut to 0.2 s with `ffmpeg -t 0.2 -c copy` first, so every seed stays
# small enough to mutate; the cut copies are made in a scratch dir and are not kept.
# seek gets the same seeds, each with a seek request appended: 9 bytes, a stream/mode byte
# then the target tick count (little-endian), which the target reads off the file's tail.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
FIXTURES=$(cd -- "$FUZZ_DIR/../../.." && pwd)/fixtures
[ -d "$FIXTURES/audio" ] || { echo "seed.sh: $FIXTURES/audio missing; run scripts/link-fixtures.sh" >&2; exit 2; }

OPEN_DIR="$FUZZ_DIR/corpus/open_tracks"
DEMUX_DIR="$FUZZ_DIR/corpus/demux_packets"
SEEK_DIR="$FUZZ_DIR/corpus/seek"
mkdir -p "$OPEN_DIR" "$DEMUX_DIR" "$SEEK_DIR"

SCRATCH=$(mktemp -d "${TMPDIR:-$HOME/.cache}/mkv-seed.XXXXXX")
trap 'rm -rf "$SCRATCH"' EXIT

MAX=$((1024 * 1024))
n=0
for src in "$FIXTURES"/audio/*.mka "$FIXTURES"/audio/*.mkv "$FIXTURES"/video/*.mkv; do
    [ -e "$src" ] || continue
    stem=$(basename "$src")
    stem=${stem%.*}
    if [ "$(stat -c %s "$src")" -le "$MAX" ]; then
        cp "$src" "$SCRATCH/$stem.mkv"
    else
        ffmpeg -v error -y -t 0.2 -i "$src" -c copy "$SCRATCH/$stem.mkv"
    fi
    seed="$SCRATCH/$stem.mkv"
    size=$(stat -c %s "$seed")
    [ "$size" -le "$MAX" ] || { echo "seed.sh: $stem is still $size bytes after the cut" >&2; exit 2; }
    cp "$seed" "$OPEN_DIR/$stem"
    cp "$seed" "$DEMUX_DIR/$stem"
    # Seek request: stream 0, mode SyncBefore, then targets at 0 ms, 500 ms and 4 s in
    # millisecond ticks (ticks are TimestampScale-relative, so these are the file's own).
    for ticks in 0 500 4000; do
        python3 -I -c '
import struct, sys
src, out, ticks = sys.argv[1], sys.argv[2], int(sys.argv[3])
data = open(src, "rb").read()
open(out, "wb").write(data + bytes([0]) + struct.pack("<q", ticks))
' "$seed" "$SEEK_DIR/$stem-t$ticks" "$ticks"
    done
    n=$((n + 1))
done

[ "$n" -gt 0 ] || { echo "seed.sh: no Matroska fixtures in $FIXTURES" >&2; exit 2; }

# Element bodies for the element-level targets, cut from the same fixtures by a walk of
# the top-level Segment children (sizes must be known; a streamed file with an unknown
# Segment or Cluster size stops the walk where it is). tracks gets the Tracks body
# behind an 8-byte TimestampScale of 1000000; index gets Cues (selector 0) and SeekHead
# (selector 1) bodies behind a selector byte and an 8-byte segment offset of 0.
TRACKS_DIR="$FUZZ_DIR/corpus/tracks"
INDEX_DIR="$FUZZ_DIR/corpus/index"
EBML_DIR="$FUZZ_DIR/corpus/ebml_walk"
LACE_DIR="$FUZZ_DIR/corpus/lacing"
mkdir -p "$TRACKS_DIR" "$INDEX_DIR" "$EBML_DIR" "$LACE_DIR"
python3 -I - "$FIXTURES" "$FUZZ_DIR/corpus" <<'PY'
import glob, os, struct, sys
fixtures, corpus = sys.argv[1], sys.argv[2]

def vint(buf, at):
    first = buf[at]
    n = 1
    while n <= 8 and not (first & (0x80 >> (n - 1))):
        n += 1
    raw = int.from_bytes(buf[at:at + n], "big")
    return raw, n

def children(buf):
    at = 0
    while at < len(buf):
        try:
            idv, il = vint(buf, at)
            sz, sl = vint(buf, at + il)
        except IndexError:
            return
        mask = (1 << (7 * sl)) - 1
        size = sz & mask
        if size == mask:
            return
        body = at + il + sl
        yield idv, buf[body:body + size]
        at = body + size

n = 0
for src in sorted(glob.glob(os.path.join(fixtures, "audio", "*.mka")) + glob.glob(os.path.join(fixtures, "audio", "*.mkv")) + glob.glob(os.path.join(fixtures, "video", "*.mkv"))):
    data = open(src, "rb").read()
    stem = os.path.splitext(os.path.basename(src))[0]
    for idv, body in children(data):
        if idv != 0x18538067:
            continue
        for cid, cbody in children(body):
            if cid == 0x1654AE6B:
                open(os.path.join(corpus, "tracks", stem), "wb").write(struct.pack("<Q", 1_000_000) + cbody)
            elif cid == 0x1C53BB6B:
                open(os.path.join(corpus, "index", stem + "-cues"), "wb").write(bytes([0]) + struct.pack("<Q", 0) + cbody)
            elif cid == 0x114D9B74:
                open(os.path.join(corpus, "index", stem + "-seekhead"), "wb").write(bytes([1]) + struct.pack("<Q", 0) + cbody)
        n += 1
print(f"element seeds from {n} Segments")
PY

echo "seeded $(ls "$OPEN_DIR" | wc -l) open_tracks, $(ls "$DEMUX_DIR" | wc -l) demux_packets, $(ls "$SEEK_DIR" | wc -l) seek, $(ls "$TRACKS_DIR" | wc -l) tracks, $(ls "$INDEX_DIR" | wc -l) index seeds"
