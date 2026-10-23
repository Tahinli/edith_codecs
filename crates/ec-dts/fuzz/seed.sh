#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the committed ec-dts fixtures (corpus is git-ignored).
set -euo pipefail
D=$(cd -- "$(dirname -- "$0")" && pwd)
for t in decode_stream parse_header; do mkdir -p "$D/corpus/$t"; done
for f in "$D"/../fixtures/*.dts; do
  b=$(basename "$f")
  head -c 20000 "$f" > "$D/corpus/decode_stream/$b"
  head -c 256 "$f" > "$D/corpus/parse_header/$b"
done
