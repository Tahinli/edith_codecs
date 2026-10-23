#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's AAC fixtures (root fixtures/audio).
#
# decode_adts  gets each ADTS fixture as-is.
# decode_he    gets each HE-AAC v1/v2 ADTS fixture from tests/data/he as-is (no root fixtures needed).
# decode_raw   gets each ADTS fixture re-packed as raw access units: selector byte, then
#              u16 BE length + raw_data_block per ADTS frame. Mono/stereo/5.1 at 48 kHz pick
#              their matching config; the stereo fixtures are also replayed through the HE-AAC
#              config so SBR is reached.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
HE_DIR=$(cd -- "$FUZZ_DIR/../tests/data/he" && pwd)
HE_CORPUS="$FUZZ_DIR/corpus/decode_he"
mkdir -p "$HE_CORPUS"
for f in "$HE_DIR"/*.aac; do cp -f "$f" "$HE_CORPUS/"; done
echo "seeded $(ls "$HE_CORPUS" | wc -l) decode_he seeds"

FIXTURES=$(cd -- "$FUZZ_DIR/../../.." && pwd)/fixtures/audio
[ -d "$FIXTURES" ] || { echo "seed.sh: $FIXTURES missing; run scripts/link-fixtures.sh (decode_he is seeded)" >&2; exit 0; }

ADTS=("$FIXTURES"/aac-adts-*.aac)
[ -e "${ADTS[0]}" ] || { echo "seed.sh: no aac-adts-*.aac fixtures in $FIXTURES" >&2; exit 2; }

ADTS_DIR="$FUZZ_DIR/corpus/decode_adts"
RAW_DIR="$FUZZ_DIR/corpus/decode_raw"
mkdir -p "$ADTS_DIR" "$RAW_DIR"

python3 -I - "$FIXTURES" "$ADTS_DIR" "$RAW_DIR" <<'PY'
import glob, os, sys

fixtures, adts_dir, raw_dir = sys.argv[1], sys.argv[2], sys.argv[3]

def adts_frames(data):
    """Yield (body) for each ADTS frame; stops at the first malformed header."""
    i = 0
    while i + 7 <= len(data):
        if not (data[i] == 0xFF and (data[i + 1] & 0xF0) == 0xF0):
            break
        prot_absent = data[i + 1] & 1
        hdr = 7 if prot_absent else 9
        flen = ((data[i + 3] & 3) << 11) | (data[i + 4] << 3) | (data[i + 5] >> 5)
        if flen < hdr:
            break
        yield data[i + hdr:i + flen]
        i += flen

def selector(name):
    if "mono" in name:
        return 0
    if "stereo" in name:
        return 1
    return 3  # 5.1

for path in sorted(glob.glob(os.path.join(fixtures, "aac-adts-*.aac"))):
    name = os.path.basename(path)
    stem = os.path.splitext(name)[0]
    data = open(path, "rb").read()
    open(os.path.join(adts_dir, stem), "wb").write(data)

    # Raw AUs under the matching config, and (for stereo) under HE-AAC as well.
    sels = [selector(name)]
    if "stereo" in name:
        sels.append(2)
    for sel in sels:
        out = bytearray([sel])
        for body in adts_frames(data):
            body = body[:65535]
            out += len(body).to_bytes(2, "big") + body
        open(os.path.join(raw_dir, f"{stem}-sel{sel}"), "wb").write(bytes(out))
print(f"seeded {len(os.listdir(adts_dir))} decode_adts and {len(os.listdir(raw_dir))} decode_raw seeds")
PY
