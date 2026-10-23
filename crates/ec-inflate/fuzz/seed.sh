#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ with DEFLATE and zlib streams.
#
# Streams come from python3's zlib at levels 0, 1, 6 and 9, over a few input shapes
# (text, zeros, random, a repeated block) so stored, fixed and dynamic blocks all appear.
# inflate_raw and inflate_diff take raw DEFLATE (zlib header and trailer stripped);
# inflate_zlib takes the full zlib stream; inflate_into gets both with the format byte.
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)

python3 -I - "$FUZZ_DIR/corpus" "$ROOT" <<'PY'
import os, random, sys, zlib

corpus, root = sys.argv[1], sys.argv[2]
targets = {t: os.path.join(corpus, t) for t in ("inflate_raw", "inflate_zlib", "inflate_into", "inflate_diff")}
for d in targets.values():
    os.makedirs(d, exist_ok=True)

rng = random.Random(0x1f1a7e)
shapes = {
    "text": (b"the quick brown fox jumps over the lazy dog\n" * 200),
    "zeros": bytes(64 * 1024),
    "random": bytes(rng.getrandbits(8) for _ in range(8192)),
    "mixed": bytes(rng.choice(b"abcdefgh  ") for _ in range(4096)) + b"0123456789" * 300,
}
# Real fixture bytes too, when linked: the first few KiB of each file under fixtures/.
fixtures = os.path.join(root, "fixtures")
if os.path.isdir(fixtures):
    for name in sorted(os.listdir(fixtures))[:6]:
        p = os.path.join(fixtures, name)
        if os.path.isfile(p):
            shapes["fx-" + name.replace("/", "_")[:40]] = open(p, "rb").read(8192)

n = 0
for shape, data in shapes.items():
    for level in (0, 1, 6, 9):
        z = zlib.compressobj(level, zlib.DEFLATED, -15)
        raw = z.compress(data) + z.flush()
        zl = zlib.compress(data, level)
        base = f"{shape}-l{level}"
        for t, payload in (("inflate_raw", raw), ("inflate_diff", b"\x00" + raw),
                           ("inflate_zlib", zl), ("inflate_into", b"\x00" + raw),
                           ("inflate_into", b"\x01" + zl), ("inflate_diff", b"\x01" + zl)):
            d = targets[t]
            # Several payloads per target share a name; hash-suffix to keep them apart.
            name = f"{base}-{len(payload)}-{payload[0]:02x}"
            open(os.path.join(d, name), "wb").write(payload)
            n += 1
print(f"seeded {n} streams under {corpus}")
PY
