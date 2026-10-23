#!/usr/bin/env bash
# Copy the tiny seed streams (fuzz/seeds, 64x64 libx265 encodes, each under 10 KB) into the corpus.
# The corpus is not committed (see .gitignore); run this before a fuzz run.
set -euo pipefail

here=$(cd -- "$(dirname -- "$0")" && pwd)

for target in parse_params_slice decode_annexb; do
  dst="$here/corpus/$target"
  mkdir -p "$dst"
  cp -f "$here"/seeds/*.bin "$dst/"
  echo "seed.sh: $(ls "$dst" | wc -l) seeds in $dst"
done
