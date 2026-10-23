#!/usr/bin/env bash
# seed.sh: build the Vorbis fuzz inputs from the repo's Ogg Vorbis fixtures.
#
# Writes two things:
#   headers/<set>.bin   the identification, comment and setup packets of one fixture per channel
#                       layout (mono, stereo, 5.1 at 48 kHz), each u16-BE-length-prefixed. The
#                       decode target include_bytes!s these, so they are committed; rerun this
#                       script only when the fixture set changes and commit the result.
#   corpus/<target>/    seeds: `setup` gets the three headers of every Ogg Vorbis file;
#                       `decode` gets a selector byte plus u16-prefixed audio packets (capped).
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
REPO=$(cd -- "$FUZZ_DIR/../../.." && pwd)
FIXTURES="$REPO/fixtures"
[ -d "$FIXTURES/audio" ] || { echo "seed.sh: $FIXTURES/audio missing; run scripts/link-fixtures.sh" >&2; exit 2; }

HEADERS_DIR="$FUZZ_DIR/headers"
SETUP_DIR="$FUZZ_DIR/corpus/setup"
DECODE_DIR="$FUZZ_DIR/corpus/decode"
mkdir -p "$HEADERS_DIR" "$SETUP_DIR" "$DECODE_DIR"

python3 -I - "$FIXTURES" "$HEADERS_DIR" "$SETUP_DIR" "$DECODE_DIR" <<'PY'
import glob, os, sys

fixtures, headers_dir, setup_dir, decode_dir = sys.argv[1:5]

def ogg_packets(path):
    """Yield the logical packets of an Ogg stream, reassembled across pages."""
    data = open(path, "rb").read()
    cur = bytearray()
    i = 0
    while i + 27 <= len(data):
        if data[i:i + 4] != b"OggS":
            # Lost sync: resync on the next capture pattern.
            j = data.find(b"OggS", i + 1)
            if j < 0:
                return
            i = j
            continue
        nseg = data[i + 26]
        seg = data[i + 27:i + 27 + nseg]
        body_len = sum(seg)
        body = data[i + 27 + nseg:i + 27 + nseg + body_len]
        p = 0
        for n in seg:
            cur += body[p:p + n]
            p += n
            if n < 255:
                yield bytes(cur)
                cur = bytearray()
        i += 27 + nseg + body_len

def first_packets(path, count):
    out = []
    for pkt in ogg_packets(path):
        out.append(pkt)
        if len(out) == count:
            break
    return out

def prefixed(packets):
    out = bytearray()
    for pkt in packets:
        pkt = pkt[:65535]
        out += len(pkt).to_bytes(2, "big") + pkt
    return bytes(out)

def is_vorbis(hdr):
    return len(hdr) >= 30 and hdr[0] == 1 and hdr[1:7] == b"vorbis"

files = sorted(glob.glob(os.path.join(fixtures, "audio", "*vorbis*.ogg")))
files += sorted(glob.glob(os.path.join(fixtures, "vectors", "vorbis-xiph", "*.ogg")))

# Fixed header sets: one fixture per layout, the 48 kHz ones.
layouts = {
    "mono-48k": "vorbis-ogg-mono-48000.ogg",
    "stereo-48k": "vorbis-ogg-stereo-48000.ogg",
    "surround-48k": "vorbis-ogg-5.1-48000.ogg",
}
by_name = {}
for path in files:
    by_name[os.path.basename(path)] = path
for stem, name in layouts.items():
    path = by_name[name]
    hdrs = first_packets(path, 3)
    assert is_vorbis(hdrs[0]) and hdrs[1][0] == 3 and hdrs[2][0] == 5, name
    with open(os.path.join(headers_dir, stem + ".bin"), "wb") as f:
        f.write(prefixed(hdrs))

# setup seeds: the three headers of every file, as they are.
setup_count = 0
for path in files:
    hdrs = first_packets(path, 3)
    if len(hdrs) < 3 or not is_vorbis(hdrs[0]):
        continue
    stem = os.path.splitext(os.path.basename(path))[0]
    with open(os.path.join(setup_dir, stem), "wb") as f:
        f.write(prefixed(hdrs))
    setup_count += 1

# decode seeds: one selector byte per channel count, then up to 200 audio packets, capped.
selector = {1: 0, 2: 1, 6: 2}
decode_count = 0
for path in files:
    hdrs = first_packets(path, 3)
    if len(hdrs) < 3 or not is_vorbis(hdrs[0]):
        continue
    sel = selector.get(hdrs[0][11])
    if sel is None:
        continue
    audio = []
    total = 0
    for pkt in ogg_packets(path):
        if len(audio) >= 3 + 200:
            break
        audio.append(pkt)
        total += len(pkt)
        if total > 65536:
            break
    audio = audio[3:]
    stem = os.path.splitext(os.path.basename(path))[0]
    with open(os.path.join(decode_dir, stem), "wb") as f:
        f.write(bytes([sel]) + prefixed(audio))
    decode_count += 1

print(f"seeded {setup_count} setup and {decode_count} decode seeds; headers in {headers_dir}")
PY
