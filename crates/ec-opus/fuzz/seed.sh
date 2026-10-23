#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the RFC 6716 test vectors and the Ogg-Opus fixtures.
#
# decode_packet: each packet stream is re-packed as the target's layout (config byte, then
#   u16 BE length + packet). Test vectors are .bit records (u32 BE length, u32 BE final range,
#   payload). Each vector is written once as mono (config 8) and once as stereo (config 9). The
#   Ogg-Opus fixtures are stereo or mono (the 5.1 file is replayed as stereo). Only the first
#   MAX_PKTS packets go into each seed, so no seed is megabytes long.
# parse_toc: the first packet of each stream, as-is (its TOC byte is the first byte).
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
VECTORS="$ROOT/fixtures/vectors/opus-rfc6716/opus_testvectors"
AUDIO="$ROOT/fixtures/audio"
[ -d "$VECTORS" ] || { echo "seed.sh: $VECTORS missing; run scripts/link-fixtures.sh" >&2; exit 2; }

PKT_DIR="$FUZZ_DIR/corpus/decode_packet"
TOC_DIR="$FUZZ_DIR/corpus/parse_toc"
mkdir -p "$PKT_DIR" "$TOC_DIR"

python3 -I - "$VECTORS" "$AUDIO" "$PKT_DIR" "$TOC_DIR" <<'PY'
import glob, os, struct, sys

vectors, audio, pkt_dir, toc_dir = sys.argv[1:5]
MAX_PKTS = 40

def rfc_bit_packets(data):
    i, out = 0, []
    while i + 8 <= len(data):
        n, _final = struct.unpack(">II", data[i:i + 8])
        i += 8
        out.append(data[i:i + n])
        i += n
    return out

def ogg_packets(data):
    i, cur, out = 0, b"", []
    while data[i:i + 4] == b"OggS":
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
    return out[2:]  # drop OpusHead and OpusTags

def write_pkt_seed(path, cfg, packets):
    buf = bytearray([cfg])
    for p in packets[:MAX_PKTS]:
        p = p[:65535]
        buf += len(p).to_bytes(2, "big") + p
    open(path, "wb").write(bytes(buf))

def write_toc_seed(path, packets):
    first = next((p for p in packets if p), b"")
    open(path, "wb").write(first[:1500])

streams = []
for path in sorted(glob.glob(os.path.join(vectors, "testvector*.bit"))):
    streams.append((os.path.basename(path)[:-4], rfc_bit_packets(open(path, "rb").read())))
for path in sorted(glob.glob(os.path.join(audio, "opus-ogg-*.opus"))):
    streams.append((os.path.basename(path)[:-5], ogg_packets(open(path, "rb").read())))

for stem, pkts in streams:
    write_pkt_seed(os.path.join(pkt_dir, f"{stem}-mono"), 8, pkts)
    write_pkt_seed(os.path.join(pkt_dir, f"{stem}-stereo"), 9, pkts)
    write_toc_seed(os.path.join(toc_dir, stem), pkts)
print(f"seeded {len(os.listdir(pkt_dir))} decode_packet and {len(os.listdir(toc_dir))} parse_toc seeds")
PY
