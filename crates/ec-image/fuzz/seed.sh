#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's still-image fixtures.
#
# Every file under fixtures/stills (PNG, JPEG, WebP, GIF, BMP, TIFF) seeds the whole-file targets:
# decode, decode_with_limits, info, probe, decode_animation. Each format target gets only the files of its
# own format, so its corpus starts on the right signature. The VP8 frames under fixtures/vp8 are
# VP8 bitstreams, not stills, and are not used here.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
STILLS="$ROOT/fixtures/stills"
[ -d "$STILLS" ] || { echo "seed.sh: $STILLS missing; run scripts/link-fixtures.sh" >&2; exit 2; }

CORPUS="$FUZZ_DIR/corpus"
mkdir -p "$CORPUS"/{decode,decode_with_limits,info,probe,decode_animation,bmp,gif,jpeg,png,tiff,webp}

python3 -I - "$STILLS" "$CORPUS" <<'PY'
import glob, os, sys

stills, corpus = sys.argv[1], sys.argv[2]
by_format = {
    "png": (".png",), "jpeg": (".jpg", ".jpeg"), "webp": (".webp",),
    "gif": (".gif",), "bmp": (".bmp",), "tiff": (".tif", ".tiff"),
}
all_files = sorted(p for p in glob.glob(os.path.join(stills, "*")) if os.path.isfile(p))
if not all_files:
    sys.exit("seed.sh: no still-image fixtures found")

def write(target, name, data):
    with open(os.path.join(corpus, target, name), "wb") as f:
        f.write(data)

count = {}
def bump(t):
    count[t] = count.get(t, 0) + 1

for path in all_files:
    name = os.path.basename(path)
    ext = os.path.splitext(name)[1].lower()
    data = open(path, "rb").read()
    for t in ("decode", "decode_with_limits", "info", "probe", "decode_animation"):
        write(t, name, data)
        bump(t)
    for fmt, exts in by_format.items():
        if ext in exts:
            write(fmt, name, data)
            bump(fmt)
for t in ("decode", "decode_with_limits", "info", "decode_animation", "bmp", "gif", "jpeg", "png", "tiff", "webp"):
    print(f"{t}: {count.get(t, 0)} seeds")
PY
