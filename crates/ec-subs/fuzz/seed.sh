#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's subtitle fixtures.
#
# fixtures/subs/real.srt seeds srt, markup and ir. A WebVTT twin of it (commas become dots, a
# WEBVTT header goes on) seeds webvtt, markup and ir. Corpus lives under fuzz/corpus, git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
SUBS="$ROOT/fixtures/subs"
[ -d "$SUBS" ] || { echo "seed.sh: $SUBS missing; run scripts/link-fixtures.sh" >&2; exit 2; }

CORPUS="$FUZZ_DIR/corpus"
mkdir -p "$CORPUS"/{srt,webvtt,markup,ir}

python3 -I - "$SUBS/real.srt" "$CORPUS" <<'PY'
import os, sys

srt, corpus = sys.argv[1], sys.argv[2]
data = open(srt, "rb").read()
vtt = b"WEBVTT\n\n" + data.replace(b",", b".")
for target in ("srt", "markup", "ir"):
    open(os.path.join(corpus, target, "real.srt"), "wb").write(data[:65536])
for target in ("webvtt", "markup", "ir"):
    open(os.path.join(corpus, target, "real.vtt"), "wb").write(vtt[:65536])
print("ec-subs seeds written")
PY
