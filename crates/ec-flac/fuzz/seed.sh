#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's FLAC fixtures (root fixtures/).
#
# decode_file and parse_metadata get every .flac as-is: the root fixtures/audio set plus the
# xiph FLAC testbench (subset, uncommon, faulty).
# decode_frame gets each file's frames only, behind a selector byte of 0: the metadata blocks
# are stripped so the fuzzer starts from bare frames, as a container would hand them out.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
FIXTURES="$ROOT/fixtures"
[ -d "$FIXTURES/audio" ] || { echo "seed.sh: $FIXTURES/audio missing; run scripts/link-fixtures.sh" >&2; exit 2; }
XIPH="$FIXTURES/vectors/flac-xiph/flac-test-files-main"
[ -d "$XIPH" ] || { echo "seed.sh: $XIPH missing; run scripts/fetch-vectors.sh" >&2; exit 2; }

FILE_DIR="$FUZZ_DIR/corpus/decode_file"
META_DIR="$FUZZ_DIR/corpus/parse_metadata"
FRAME_DIR="$FUZZ_DIR/corpus/decode_frame"
mkdir -p "$FILE_DIR" "$META_DIR" "$FRAME_DIR"

python3 -I - "$FIXTURES/audio" "$XIPH" "$FILE_DIR" "$META_DIR" "$FRAME_DIR" <<'PY'
import glob, os, re, sys

audio, xiph, file_dir, meta_dir, frame_dir = sys.argv[1:6]

paths = sorted(glob.glob(os.path.join(audio, "flac-*.flac")))
for sub in ("subset", "uncommon", "faulty"):
    paths += sorted(glob.glob(os.path.join(xiph, sub, "*.flac")))
# Harness cap: seeds above 1 MiB are skipped. The xiph testbench has 87 MB and 42 MB files;
# one such input runs at a few exec/s and would dominate the RSS budget of a fuzz run.
MAX_SEED = 1 << 20
paths = [p for p in paths if os.path.getsize(p) <= MAX_SEED]
if not paths:
    sys.exit("seed.sh: no FLAC fixtures found")

def frames_after_metadata(data):
    """Bytes from the first frame on, or None when the file has no fLaC magic."""
    if data[:4] != b"fLaC":
        return None
    i = 4
    while i + 4 <= len(data):
        last = data[i] & 0x80
        n = int.from_bytes(data[i + 1:i + 4], "big")
        i += 4 + n
        if last:
            return data[i:]
    return None

def stem(path):
    rel = os.path.relpath(path, xiph) if path.startswith(xiph) else os.path.basename(path)
    return re.sub(r"[^A-Za-z0-9._-]+", "_", os.path.splitext(rel)[0]).strip("_")

count = 0
for path in paths:
    data = open(path, "rb").read()
    name = stem(path)
    open(os.path.join(file_dir, name), "wb").write(data)
    open(os.path.join(meta_dir, name), "wb").write(data)
    body = frames_after_metadata(data)
    if body is not None:
        open(os.path.join(frame_dir, name), "wb").write(b"\x00" + body)
        count += 1
print(f"seeded {len(paths)} decode_file and parse_metadata seeds, {count} decode_frame seeds")
PY
