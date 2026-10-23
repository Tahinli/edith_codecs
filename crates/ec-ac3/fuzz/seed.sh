#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's AC-3 and E-AC-3 fixtures (root fixtures/audio).
#
# decode_stream  gets each fixture truncated to its first few syncframes (5 frames).
# parse_header   gets the first 256 bytes of each fixture, which covers syncinfo and the BSI.
#
# Frame boundaries are found by walking the 0x0B77 syncwords, so a stray match inside a frame
# only makes a seed slightly longer or shorter; it is still a valid starting point.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
FIXTURES=$(cd -- "$FUZZ_DIR/../../.." && pwd)/fixtures/audio
[ -d "$FIXTURES" ] || { echo "seed.sh: $FIXTURES missing; run scripts/link-fixtures.sh" >&2; exit 2; }

FILES=("$FIXTURES"/ac3-*.ac3 "$FIXTURES"/eac3-*.eac3 "$FIXTURES"/ac3dep-*.eac3)
[ -e "${FILES[0]}" ] || { echo "seed.sh: no ac3-*.ac3 or eac3-*.eac3 fixtures in $FIXTURES" >&2; exit 2; }

STREAM_DIR="$FUZZ_DIR/corpus/decode_stream"
HEADER_DIR="$FUZZ_DIR/corpus/parse_header"
mkdir -p "$STREAM_DIR" "$HEADER_DIR"

python3 -I - "$FIXTURES" "$STREAM_DIR" "$HEADER_DIR" <<'PY'
import glob, os, sys

fixtures, stream_dir, header_dir = sys.argv[1], sys.argv[2], sys.argv[3]
FRAMES = 5

paths = sorted(glob.glob(os.path.join(fixtures, "ac3-*.ac3")) +
               glob.glob(os.path.join(fixtures, "eac3-*.eac3")) +
               glob.glob(os.path.join(fixtures, "ac3dep-*.eac3")))
for path in paths:
    name = os.path.splitext(os.path.basename(path))[0]
    data = open(path, "rb").read()
    starts = []
    i = 0
    while i + 2 <= len(data) and len(starts) <= FRAMES:
        if data[i] == 0x0B and data[i + 1] == 0x77:
            starts.append(i)
            i += 2
        else:
            i += 1
    # Keep the first FRAMES frames: cut at the start of frame FRAMES (or the file end).
    end = starts[FRAMES] if len(starts) > FRAMES else len(data)
    open(os.path.join(stream_dir, name), "wb").write(data[:end])
    open(os.path.join(header_dir, name), "wb").write(data[:256])
print(f"seeded {len(os.listdir(stream_dir))} decode_stream and {len(os.listdir(header_dir))} parse_header seeds")
PY
