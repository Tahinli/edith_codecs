#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ with TrueHD/MLP seeds.
#
# There are no TrueHD fixtures in the repo, so the seeds are generated with ffmpeg's
# truehd and mlp encoders: stereo, 2.1, 5.1 and mono MLP, 1 s of sine each.
# (Any *thd*, *truehd* or *mlp* file under the root fixtures/ is also used when present.)
#
# decode_stream gets each stream as-is (elementary stream of access units).
# parse_sync  gets the first access unit of each stream (its header and major sync).
#
# Corpus lives under fuzz/corpus, which is git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
FIXTURES="$ROOT/fixtures"

STREAM_DIR="$FUZZ_DIR/corpus/decode_stream"
SYNC_DIR="$FUZZ_DIR/corpus/parse_sync"
mkdir -p "$STREAM_DIR" "$SYNC_DIR"

GEN=$(mktemp -d "${TMPDIR:-$HOME}/ec-truehd-seed.XXXXXX")
trap 'rm -rf "$GEN"' EXIT

command -v ffmpeg >/dev/null || { echo "seed.sh: ffmpeg not found" >&2; exit 2; }
gen() { # name channels codec muxer
  ffmpeg -hide_banner -loglevel error -y -f lavfi -i "sine=frequency=440:duration=1" \
    -ac "$2" -c:a "$3" -strict -2 -t 1 -f "$4" "$GEN/$1"
}
gen stereo.thd 2 truehd truehd
gen 51.thd 6 truehd truehd
gen 21.thd 3 truehd truehd
gen mono.mlp 1 mlp mlp

# Repo fixtures, if the root fixtures/ directory is linked in this worktree.
if [ -d "$FIXTURES" ]; then
  find "$FIXTURES" -type f \( -iname '*thd*' -o -iname '*truehd*' -o -iname '*mlp*' \) \
    -exec cp {} "$GEN/" \; 2>/dev/null || true
fi

python3 -I - "$GEN" "$STREAM_DIR" "$SYNC_DIR" <<'PY'
import glob, os, shutil, sys

gen_dir, stream_dir, sync_dir = sys.argv[1], sys.argv[2], sys.argv[3]

def first_au(data):
    """Bytes of the first access unit: 12-bit word length, in 16-bit words."""
    if len(data) < 2:
        return b""
    n = ((data[0] << 8) | data[1]) & 0x0FFF
    return data[: n * 2]

count = 0
for path in sorted(glob.glob(os.path.join(gen_dir, "*"))):
    name = os.path.basename(path)
    data = open(path, "rb").read()
    shutil.copy(path, os.path.join(stream_dir, name))
    au = first_au(data)
    if au:
        open(os.path.join(sync_dir, name), "wb").write(au)
    count += 1
print(f"seeded {count} streams: {len(os.listdir(stream_dir))} decode_stream, {len(os.listdir(sync_dir))} parse_sync")
PY
