#!/usr/bin/env bash
# Copy the tiny AV1 seed streams (fuzz/seeds, each under 3 KB) into the corpus.
# The corpus is not committed (see .gitignore); run this before a fuzz run.
set -euo pipefail

here=$(cd -- "$(dirname -- "$0")" && pwd)

for target in parse_obu decode_stream; do
  dst="$here/corpus/$target"
  mkdir -p "$dst"
  cp -f "$here"/seeds/*.obu "$dst/"
  echo "seed.sh: $(ls "$dst" | wc -l) seeds in $dst"
done
