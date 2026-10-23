#!/usr/bin/env bash
# seed.sh: rebuild fuzz/corpus/ from the crate's test fixtures.
#
# Frames are cut out of each IVF file (the first 10 frames, frames over 256 KiB skipped)
# into corpus/decode_frame/. Whole IVF files up to 256 KiB go into corpus/decode_ivf/, and
# a 10-frame prefix of every larger file too. The corpus is generated, not committed.
#
# Usage: crates/ec-vp8/fuzz/seed.sh   (from any directory; needs python3 and the fixtures/ link)
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
rm -rf "$here/corpus"
mkdir -p "$here/corpus/decode_frame" "$here/corpus/decode_ivf"
exec python3 -I - "$here" "decode_ivf" "$repo/fixtures/vp8/*.ivf" <<'PY'
import glob, os, struct, sys

here, stream_target = sys.argv[1], sys.argv[2]
sources = sorted(sum((glob.glob(p) for p in sys.argv[3:]), []))
MAX_FRAMES, MAX_BYTES = 10, 256 * 1024

def split_ivf(data):
    hdr_len = struct.unpack_from('<H', data, 6)[0] or 32
    frames, pos = [], hdr_len
    while pos + 12 <= len(data):
        size = struct.unpack_from('<I', data, pos)[0]
        pos += 12
        if pos + size > len(data):
            break
        frames.append(data[pos:pos + size])
        pos += size
    return data[:hdr_len], frames

def ivf(header, frames):
    out = bytearray(header)
    for i, f in enumerate(frames):
        out += struct.pack('<IQ', len(f), i) + f
    return bytes(out)

nframes = 0
for path in sources:
    name = os.path.splitext(os.path.basename(path))[0]
    data = open(path, 'rb').read()
    header, frames = split_ivf(data)
    kept = [f for f in frames[:MAX_FRAMES] if len(f) <= MAX_BYTES]
    for i, f in enumerate(kept):
        with open(os.path.join(here, 'corpus', 'decode_frame', f'{name}-{i:02}.bin'), 'wb') as out:
            out.write(f)
        nframes += 1
    if len(data) <= MAX_BYTES:
        blob, dest = data, f'{name}.ivf'
    else:
        blob, dest = ivf(header, kept), f'{name}-prefix.ivf'
    with open(os.path.join(here, 'corpus', stream_target, dest), 'wb') as out:
        out.write(blob)
print(f'seeded {nframes} frames and {len(sources)} IVF files from {len(sources)} sources')
PY
