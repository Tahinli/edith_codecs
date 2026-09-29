#!/usr/bin/env bash
# gen-fixture-library.sh — GENERATE scripts/fixture-library.tsv from source.
#
# The tsv is committed output, never hand-typed. Regenerate with:
#     scripts/gen-fixture-library.sh && git diff --exit-code scripts/fixture-library.tsv
# so drift between "what the tests reach for" and "the manifest" is a
# one-command diff. `scripts/verify-fixture-library.sh` runs that same diff as
# part of its exit status.
#
# COLUMNS (tab separated)
#   1 path          repo-relative; `fixtures/...` is relative to $EC_FIXTURES
#   2 required-by   file:line of every reference (one row per reference)
#   3 provenance    the script/recipe that reproduces the bytes, or
#                   `none (prose provenance only)` — a FINDING, not a
#                   placeholder: a pinned fixture claim must cite the
#                   originating encoder program and the artifact sha256, or the
#                   pin is unreproducible (the IVF-container drift incident is
#                   why that rule exists)
#   4 status        ok | missing | empty-dir | absent-pin
#   5 sha256        of the artefact, when it is a regular file under 4 MiB
#
# ENUMERATION SHAPES the scan is derived from:
#   CARGO_MANIFEST_DIR      env!("CARGO_MANIFEST_DIR") / concat!(env!(...))
#   concat!(env!(           a fixture root assembled from the manifest dir
#   include_bytes!          include_bytes!/include_str! argument literals
#   "fixtures/              any string literal naming the fixture tree
#   ../../fixtures          crate-relative literals (CWD is the crate dir)
#   Path::new(               Path::new("...") argument literals
#   fs::read                fs::read / read_dir / File::open argument literals
#
# TWO PASSES:
#   1. fixture-root / fixture-path literals -> the row is the literal itself.
#   2. bare media-file literals ("aac-adts-5.1-44100.aac", "entry-tx3g.mp4")
#      joined onto each fixture directory literal seen in the SAME file, then
#      onto the standard top-level library directories. Only joins that
#      resolve are emitted; unresolved names are counted in the header as the
#      declared blind spot.
#
# BLIND SPOT (honest: this is a preflight, not a proof). The scan is literal-
# and grep-derived, so it cannot see a path assembled at runtime from a value
# that is not a literal in the same file -- a vector name read from
# scripts/vectors.sha256, a row from fixtures/real-library-manifest.tsv, a
# format! interpolation, a name in a table. For those, the parent DIRECTORY is
# still enumerated, and the verifier requires every listed directory to exist
# AND be non-empty -- that is what catches the "three hosts whose fixture
# libraries differed" class. A single missing member of a runtime-named corpus
# shows up as a regeneration diff, not as a resolve failure.

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
OUT=${1:-$ROOT/scripts/fixture-library.tsv}
FIXTURES=${EC_FIXTURES:-$ROOT/fixtures}

MEDIA_RE='\.(mp4|m4a|m4s|m4v|mkv|ivf|obu|flac|opus|ogg|oga|webm|265|264|h264|h265|hevc|y4m|yuv|wav|aac|ts|bin|ttml|srt|vtt|ass|json)'

# Pins that are KNOWN ABSENT and must not vanish from the manifest. Each is
# recorded as an `absent-pin` row naming the gate that wants it, so the gap is
# visible instead of silently skipped.
absent_pin() {
    case $1 in
        fixtures/ll444-lossless-key.obu)
            echo "absent pin -- gate a_real_aomenc_lossless_444_key_frame (converted to an EC_REQUIRE_FIXTURES hard fail; pin never committed)" ;;
        fixtures/golden3-pin.obu | fixtures/sbpart-pin.obu)
            echo "absent pin -- gate is #[ignore]d; no encoder recipe recorded" ;;
        *) return 1 ;;
    esac
}

# --- 1. collect the source files that can name a fixture -----------------
mapfile -t FILES < <(
    grep -rl --include='*.rs' \
        -e 'fixtures' -e 'include_bytes!' -e 'include_str!' \
        "$ROOT"/crates/*/tests "$ROOT"/crates/*/src 2>/dev/null | sort
)

is_fixture_literal() { # $1 = literal without quotes
    local lit=$1
    case $lit in
        *[[:space:]]* | *'{'* | *'}'* | *'$'* | *'%'* | *'*'* | *'?'* | \
        *'('* | *')'* | *','* | *'='* | *';'* | *':'*) return 1 ;;
        *.sh | *.py | *.md | *.tsv) return 1 ;;   # a script name, not a path
        /home/* | /Users/* | /tmp/*) return 1 ;;  # developer path, not the library
    esac
    [[ $lit == *fixtures* || $lit == */vectors/* ]] || return 1
    return 0
}

# Provenance: which script reproduces these bytes. An empty answer is a
# finding -- committed pins whose only provenance is prose in a gate comment.
generator_for() { # $1 = repo-relative path
    case $1 in
        fixtures/vectors/*)                 echo "scripts/fetch-vectors.sh" ;;
        fixtures/bitstreams/*)              echo "scripts/gen-bitstream-fixtures.sh" ;;
        fixtures/stills/*)                  echo "scripts/gen-still-fixtures.sh" ;;
        fixtures/subs/*)                    echo "scripts/gen-subtitle-fixtures.sh" ;;
        fixtures/real-library-manifest.tsv) echo "scripts/scan-real-library.sh" ;;
        fixtures/vp8/*)                     echo "scripts/gen_vp8_fixtures.sh" ;;
        fixtures/*.obu)                     echo "none (prose provenance only)" ;;
        crates/*/fixtures/*.obu)            echo "none (prose provenance only)" ;;
        fixtures/audio/mp3* | fixtures/audio/*mp3*) echo "scripts/gen-mp3-fixtures.sh" ;;
        *)                                  echo "scripts/gen-fixtures.sh" ;;
    esac
}

abs_path() { # $1 = manifest path -> absolute path
    case $1 in
        fixtures/*) printf '%s\n' "$FIXTURES/${1#fixtures/}" ;;
        *) printf '%s\n' "$ROOT/$1" ;;
    esac
}

# Resolve a source literal to a manifest path. Cargo runs tests with the crate
# directory as CWD, so `../../fixtures` and `CARGO_MANIFEST_DIR` both anchor at
# the crate; a bare `fixtures/...` is accepted from either anchor.
resolve() { # $1 = literal, $2 = crate dir; prints manifest path
    local lit=$1 crate=$2 base cand
    for base in "$crate" "$ROOT" "$ROOT/crates"; do
        if [ -e "$base/$lit" ]; then
            cand=$(realpath -m -- "$base/$lit")
            printf '%s\n' "${cand#"$ROOT"/}"
            return 0
        fi
    done
    # Unresolved: normalise against the repo root so the verifier reports the
    # row loudly instead of it silently vanishing from the manifest.
    cand=$(realpath -m -- "$ROOT/$lit")
    printf '%s\n' "${cand#"$ROOT"/}"
}

row() { # $1 path, $2 required-by, $3 provenance (optional), $4 status override
    local path=$1 required_by=$2 prov=${3:-} status_override=${4:-}
    local abs status sum
    abs=$(abs_path "$path")
    if [ -n "$status_override" ]; then
        status=$status_override
    elif [ ! -e "$abs" ]; then
        status=missing
    elif [ -d "$abs" ] && [ -z "$(ls -A -- "$abs" 2>/dev/null)" ]; then
        status=empty-dir
    else
        status=ok
    fi
    if [ -z "$prov" ]; then prov=$(generator_for "$path"); fi
    if [ -f "$abs" ] && [ "$(stat -c %s -- "$abs")" -lt 4194304 ]; then
        sum=$(sha256sum -- "$abs" | cut -d' ' -f1)
    else
        sum='-'
    fi
    printf '%s\t%s\t%s\t%s\t%s\n' "$path" "$required_by" "$prov" "$status" "$sum"
}

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
unnamed=0
seen_absent=()

for f in "${FILES[@]}"; do
    crate=$(dirname "$(dirname "$f")")
    rel=${f#"$ROOT"/}
    file_dirs=()

    # Pass 1 -- fixture-root / fixture-path literals.
    while IFS= read -r hit; do
        lineno=${hit%%:*}
        lit=${hit#*:}
        lit=${lit#\"}
        lit=${lit%\"}
        is_fixture_literal "$lit" || continue
        path=$(resolve "$lit" "$crate")
        prov=$(absent_pin "$path") && {
            row "$path" "$rel:$lineno" "$prov" absent-pin >>"$tmp"
            seen_absent+=("$path")
            continue
        }
        row "$path" "$rel:$lineno" >>"$tmp"
        [ -d "$(abs_path "$path")" ] && file_dirs+=("$path")
    done < <(grep -noE '"[^"]*"' "$f")

    # Pass 2 -- bare media names joined onto this file's fixture directories,
    # then onto the standard top-level library directories (a file may name
    # its corpus directory in a helper module rather than in this file).
    for d in audio video bitstreams subs stills realworld vp8 vp9 hbd-r5; do
        [ -d "$FIXTURES/$d" ] && file_dirs+=("fixtures/$d")
    done
    ((${#file_dirs[@]})) || continue
    while IFS= read -r hit; do
        lineno=${hit%%:*}
        name=${hit#*:}
        name=${name#\"}
        name=${name%\"}
        for d in "${file_dirs[@]}"; do
            if [ -e "$(abs_path "$d/$name")" ]; then
                row "$d/$name" "$rel:$lineno" >>"$tmp"
                continue 2
            fi
        done
        unnamed=$((unnamed + 1))
    done < <(grep -noE '"[^"/]*'"$MEDIA_RE"'"' "$f")
done

# Pass 3 -- committed pins that NO source literal reaches. A committed
# fixture nothing references is either dead weight or a reference built at
# runtime from a name the grep cannot see; either way it must be a row, not a
# gap. Scanned over the crate-local committed pin directories.
orphans=0
while IFS= read -r pin; do
    [ -n "$pin" ] || continue
    if ! grep -qP "^\Q$pin\E\t" "$tmp"; then
        row "$pin" "(committed pin: NO source reference found)" \
            "none (prose provenance only)" >>"$tmp"
        orphans=$((orphans + 1))
    fi
done < <(find "$ROOT"/crates/*/fixtures -maxdepth 1 -type f 2>/dev/null |
    sed "s#^$ROOT/##" | sort)

# A known-absent pin is annotated where a source literal actually REACHES for
# it (pass 1, above: `absent_pin` supplies the provenance and the `absent-pin`
# status). Nothing is injected here: a pin no code references any more is not
# a gap in the library, it is a deleted reference, and inventing a row for it
# would keep a stale claim alive. A pin that IS referenced and IS absent still
# gets a row, because pass 1 emits one for every literal.

# Which tests consume the fetched vector sets, and does anything need the
# .tar.gz blobs? The fleet sync excluded them deliberately, so a test that
# wanted a blob would be red on three hosts for a reason nobody wrote down.
vector_consumers=$(awk -F'\t' '$1 ~ /^fixtures\/vectors\// {split($2,a,":"); print a[1]}' "$tmp" | sort -u | tr '\n' ' ')
blob_users=$(grep -rl --include='*.rs' '\.tar\.gz' "$ROOT"/crates 2>/dev/null | tr '\n' ' ')

{
    cat <<EOF
# GENERATED by scripts/gen-fixture-library.sh -- do not hand-edit.
# Regenerate: scripts/gen-fixture-library.sh
# Verify:     scripts/verify-fixture-library.sh
# path <TAB> required-by file:line <TAB> provenance <TAB> status <TAB> sha256
#
# Enumeration is grep-derived. It cannot see a fixture name that is not a
# string literal in the same file (manifest rows, vectors.sha256 names,
# format! interpolation); such names are only covered through their parent
# directory, which the verifier requires to exist AND be non-empty.
# $unnamed media-name literal(s) in this tree resolved under no enumerated
# fixture directory and are therefore not listed individually.
#
# ROOT FIXTURE ROOT: $FIXTURES
#
# VECTORS consumers (fixtures/vectors/*): ${vector_consumers:-none}
# VECTORS .tar.gz blob referenced by: ${blob_users:-nothing}
#   The three fleet hosts carry the EXTRACTED vector sets (~470 files) and the
#   sync deliberately excluded the .tar.gz blobs; nothing in crates/ reads a
#   blob, so the extracted directories are the whole requirement.
#   Unreferenced committed pins carried as rows: $orphans
EOF
    sort -u "$tmp"
} >"$OUT"

rows=$(grep -cv '^#' "$OUT")
echo "gen-fixture-library: $rows rows -> ${OUT#"$ROOT"/} ($unnamed unnamed media literals, $orphans unreferenced committed pins)" >&2
