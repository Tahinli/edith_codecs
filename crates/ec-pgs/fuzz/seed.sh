#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's PGS fixture.
#
# fixtures/subs/pgs-1080p.sup is a real disc's display sets. segments gets the file's first 64 KiB
# as is. compose and rle get the same bytes too; those two read their own input layouts, so the
# seeds are mostly rejects, which still steer the mutator. Corpus lives under fuzz/corpus.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
SUBS="$ROOT/fixtures/subs"
[ -f "$SUBS/pgs-1080p.sup" ] || { echo "seed.sh: $SUBS/pgs-1080p.sup missing; run scripts/link-fixtures.sh" >&2; exit 2; }

CORPUS="$FUZZ_DIR/corpus"
mkdir -p "$CORPUS"/{segments,rle,compose}
head -c 65536 "$SUBS/pgs-1080p.sup" > "$CORPUS/segments/pgs-1080p"
head -c 4096 "$SUBS/pgs-1080p.sup" > "$CORPUS/compose/pgs-1080p"
head -c 4096 "$SUBS/pgs-1080p.sup" > "$CORPUS/rle/pgs-1080p"
echo "ec-pgs seeds written"
