#!/usr/bin/env bash
# seed.sh: fill fuzz/corpus/<target>/ from the repo's ASS fixture.
#
# fixtures/subs/real.ass seeds script whole. events and overrides get its Dialogue rows (the lines
# after the first Dialogue: prefix, which is what each of those targets takes as input). styles gets
# the Style rows. Corpus lives under fuzz/corpus, git-ignored.
set -euo pipefail

FUZZ_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(cd -- "$FUZZ_DIR/../../.." && pwd)
SUBS="$ROOT/fixtures/subs"
[ -d "$SUBS" ] || { echo "seed.sh: $SUBS missing; run scripts/link-fixtures.sh" >&2; exit 2; }

CORPUS="$FUZZ_DIR/corpus"
mkdir -p "$CORPUS"/{script,styles,events,overrides}

python3 -I - "$SUBS/real.ass" "$CORPUS" <<'PY'
import os, sys

ass, corpus = sys.argv[1], sys.argv[2]
data = open(ass, "rb").read()
lines = data.decode("utf-8", "replace").splitlines(keepends=True)
dialogue = [l.split(":", 1)[1].lstrip() for l in lines if l.startswith("Dialogue:")]
styles = [l.split(":", 1)[1].lstrip() for l in lines if l.startswith("Style:")]

def write(path, body):
    with open(path, "wb") as f:
        f.write(body)

write(os.path.join(corpus, "script", "real.ass"), data)
for i, row in enumerate(dialogue[:400]):
    write(os.path.join(corpus, "events", f"row{i:04}"), row.encode())
    write(os.path.join(corpus, "overrides", f"row{i:04}"), b"\x00" + row.encode())
for i, row in enumerate(styles[:200]):
    write(os.path.join(corpus, "styles", f"row{i:04}"), b"\x00" + row.encode())
print(f"ec-ass seeds: {len(dialogue)} dialogue rows, {len(styles)} style rows")
PY
