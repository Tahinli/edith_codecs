#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ for the ec-riff targets.
#
# wav_read   gets every root fixtures/audio/*.wav as-is, plus an RF64 copy of each (the RIFF
#            magic swapped for RF64, so the unsupported-RF64 branch is reached too).
# avi_read   and avi_seek get the AVI files in fixtures/audio/*.avi when there are any, plus a
#            few synthetic AVIs written here (the repo has no AVI fixture yet). avi_seek gets
#            the same files behind its 10-byte control prefix.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
FIXTURES=$(cd -- "$FUZZ_DIR/../../.." && pwd)/fixtures/audio
[ -d "$FIXTURES" ] || { echo "seed.sh: $FIXTURES missing; run scripts/link-fixtures.sh" >&2; exit 2; }

WAVS=("$FIXTURES"/*.wav)
[ -e "${WAVS[0]}" ] || { echo "seed.sh: no *.wav fixtures in $FIXTURES" >&2; exit 2; }

WAV_DIR="$FUZZ_DIR/corpus/wav_read"
AVI_DIR="$FUZZ_DIR/corpus/avi_read"
SEEK_DIR="$FUZZ_DIR/corpus/avi_seek"
mkdir -p "$WAV_DIR" "$AVI_DIR" "$SEEK_DIR"

python3 -I - "$FIXTURES" "$WAV_DIR" "$AVI_DIR" "$SEEK_DIR" <<'PY'
import glob, os, struct, sys

fixtures, wav_dir, avi_dir, seek_dir = sys.argv[1:5]

# RF64 copy: "RIFF" -> "RF64" and a ds64 chunk after the WAVE tag. The sizes in the file
# stay as they were, so the reader still sees a fixed-size data chunk.
def rf64_copy(data):
    ds64 = b"ds64" + struct.pack("<I", 28) + bytes(28)
    return b"RF64" + data[4:8] + b"WAVE" + ds64 + data[12:]

for path in sorted(glob.glob(os.path.join(fixtures, "*.wav"))):
    stem = os.path.splitext(os.path.basename(path))[0]
    data = open(path, "rb").read()
    open(os.path.join(wav_dir, stem), "wb").write(data)
    open(os.path.join(wav_dir, stem + "-rf64"), "wb").write(rf64_copy(data))

def chunk(id_, body):
    pad = b"\0" if len(body) & 1 else b""
    return id_ + struct.pack("<I", len(body)) + body + pad

def lst(kind, body):
    return chunk(b"LIST", kind + body)

def avi(channels, rate, bits, nchunks, chunk_bytes, idx1=True, pcm_tag=1):
    align = channels * bits // 8
    wfx = struct.pack("<HHIIHH", pcm_tag, channels, rate, rate * align, align, bits)
    # strh: 56 bytes; scale at 20, rate at 24, length at 32, sampleSize at 44.
    strh = (b"auds" + bytes(4) + struct.pack("<IHHIIIIII", 0, 0, 0, 0, 1, rate, 0,
                                             nchunks * (chunk_bytes // align), 0)
            + struct.pack("<I", 0) + struct.pack("<I", align) + bytes(8))
    assert len(strh) == 56, len(strh)
    hdrl_body = (chunk(b"avih", bytes(56))
                 + lst(b"strl", chunk(b"strh", strh) + chunk(b"strf", wfx)))
    body = bytearray()
    entries = []
    for i in range(nchunks):
        payload = bytes((i * 37 + k) & 0xFF for k in range(chunk_bytes))
        entries.append((len(body), len(payload)))
        body += chunk(b"00wb", payload)
    movi = lst(b"movi", bytes(body))
    idx = b""
    if idx1:
        # idx1 offsets point at each chunk header, relative to the "movi" list's first chunk.
        for off, size in entries:
            idx += b"00wb" + struct.pack("<III", 0x10, off, size)
        idx = chunk(b"idx1", idx)
    payload = b"AVI " + chunk(b"LIST", b"hdrl" + hdrl_body) + movi + idx
    return b"RIFF" + struct.pack("<I", len(payload)) + payload

synth = {
    "synth-mono-8k-16": avi(1, 8000, 16, 6, 320),
    "synth-stereo-48k-16": avi(2, 48000, 16, 4, 1024),
    "synth-stereo-48k-16-noidx": avi(2, 48000, 16, 4, 1024, idx1=False),
    "synth-float-stereo": avi(2, 44100, 32, 3, 512, pcm_tag=3),
    "synth-empty-movi": avi(2, 48000, 16, 0, 0),
}
for name, data in synth.items():
    open(os.path.join(avi_dir, name), "wb").write(data)

# Real AVI fixtures, if the repo ever gets some.
for path in sorted(glob.glob(os.path.join(fixtures, "*.avi"))):
    stem = os.path.splitext(os.path.basename(path))[0]
    open(os.path.join(avi_dir, stem), "wb").write(open(path, "rb").read())

# avi_seek: control prefix (flag, stream, target) then the AVI bytes.
for name, data in list(synth.items()) + [
    (os.path.splitext(os.path.basename(p))[0], open(p, "rb").read())
    for p in sorted(glob.glob(os.path.join(fixtures, "*.avi")))
]:
    for flag, stream, target in [(0, 0, 0), (1, 1, 5), (0, 0, 1 << 40)]:
        out = bytes([flag, stream]) + struct.pack("<Q", target) + data
        open(os.path.join(seek_dir, f"{name}-f{flag}s{stream}"), "wb").write(out)

print(f"seeded {len(os.listdir(wav_dir))} wav_read, {len(os.listdir(avi_dir))} avi_read, "
      f"{len(os.listdir(seek_dir))} avi_seek seeds")
PY
