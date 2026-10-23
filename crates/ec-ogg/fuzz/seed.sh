#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's Ogg fixtures.
#
# The Ogg files are the Opus-in-Ogg and Vorbis-in-Ogg fixtures under fixtures/audio and the
# Xiph Vorbis vectors under fixtures/vectors/vorbis-xiph. The RFC 6716 vectors under
# fixtures/vectors/opus-rfc6716 are .bit records, not Ogg, and are left to ec-opus's seeds.
#
# page_parse, demux, demux_raw each get every file as-is. mapping gets the first 256 bytes of
# each file, which is where the identification headers sit. roundtrip gets its own layout:
# a 4-byte header and u16-BE-length-prefixed Opus packets taken from each file's audio pages.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
AUDIO="$ROOT/fixtures/audio"
VORBIS="$ROOT/fixtures/vectors/vorbis-xiph"
[ -d "$AUDIO" ] || { echo "seed.sh: $AUDIO missing; run scripts/link-fixtures.sh" >&2; exit 2; }

CORPUS="$FUZZ_DIR/corpus"
mkdir -p "$CORPUS"/{page_parse,demux,demux_raw,mapping,roundtrip}

python3 -I - "$AUDIO" "$VORBIS" "$CORPUS" <<'PY'
import glob, os, sys

audio, vorbis, corpus = sys.argv[1], sys.argv[2], sys.argv[3]
files = sorted(glob.glob(os.path.join(audio, "*.ogg")) + glob.glob(os.path.join(audio, "*.opus")))
files += sorted(glob.glob(os.path.join(vorbis, "*.ogg")))
if not files:
    sys.exit("seed.sh: no Ogg fixtures found")

def ogg_packets(data):
    """Every complete packet of the file, in order, across all serials (header packets included)."""
    i, cur, out = 0, b"", []
    while data[i:i + 4] == b"OggS" and i + 27 <= len(data):
        nseg = data[i + 26]
        segtab = data[i + 27:i + 27 + nseg]
        pos = i + 27 + nseg
        for s in segtab:
            cur += data[pos:pos + s]
            pos += s
            if s < 255:
                out.append(cur)
                cur = b""
        i = pos
    return out

def opus_roundtrip_seed(packets):
    # Opus packets only: the first two (OpusHead, OpusTags) go; the rest are audio.
    buf = bytearray([64, 0, 0, 0])  # page target 64 + 1, mono, pre-skip 0
    for p in packets[2:2 + 64]:
        p = p[:65535]
        if p:
            buf += len(p).to_bytes(2, "big") + p
    return bytes(buf)

def write(path, data):
    with open(path, "wb") as f:
        f.write(data)

count = {"page_parse": 0, "demux": 0, "demux_raw": 0, "mapping": 0, "roundtrip": 0}
for path in files:
    stem = os.path.splitext(os.path.basename(path))[0]
    data = open(path, "rb").read()
    write(os.path.join(corpus, "page_parse", stem), data)
    write(os.path.join(corpus, "demux", stem), data)
    write(os.path.join(corpus, "demux_raw", stem), data)
    write(os.path.join(corpus, "mapping", stem), data[:256])
    count["page_parse"] += 1
    count["demux"] += 1
    count["demux_raw"] += 1
    count["mapping"] += 1
    packets = ogg_packets(data)
    if len(packets) >= 3:
        write(os.path.join(corpus, "roundtrip", stem), opus_roundtrip_seed(packets))
        count["roundtrip"] += 1
for name, n in count.items():
    print(f"{name}: {n} seeds")
PY
