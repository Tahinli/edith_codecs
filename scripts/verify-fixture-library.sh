#!/usr/bin/env bash
# verify-fixture-library.sh — preflight: does the on-disk fixture library match
# what the tests actually reach for?
#
# CHECKS
#   SHAPE    the manifest exists, is non-empty, and every row has 5 columns
#   RESOLVE  every referenced path exists, and every referenced DIRECTORY is
#            non-empty (a present-but-empty corpus is the same false green as
#            an absent one)
#   DRIFT    regenerating the manifest from source reproduces the committed
#            file -- so a host whose library is behind the code fails here
#            instead of reporting a green suite full of SKIPs
#
# TWO FAILURE MODES ARE DISTINGUISHED, because they have different fixes:
#   (i)  the fixture ROOT itself is absent. Root `fixtures/` is .gitignore:2,
#        so no linked worktree ever has it: every vector gate skips GREEN in
#        every worktree run. Fix: link or provision the library, not the code.
#   (ii) a path under a PRESENT root is absent (or an empty dir). Fix: the
#        generator named in the row, or the pin is genuinely missing.
#
# ENFORCEMENT ENV: EC_REQUIRE_FIXTURES=1 -- the same env the per-crate
# `require_fixture` asserts honour, so one convention covers the fleet batch
# units and a local run:
#   set   -> any absent referenced path is RED, and the drift diff is RED
#   unset -> the same findings are printed SKIP-shaped and the exit stays 0,
#            so an unprepared local checkout is not blocked
#
# Usage:  scripts/verify-fixture-library.sh
# Env:    EC_FIXTURES=<dir>            fixture root, as every generator takes
#         EC_REQUIRE_FIXTURES=1|0      enforcement (see above)
#
# ENUMERATION BLIND SPOT: the scan is grep-derived, so a fixture name that is
# not a string literal in the same file (a row of
# fixtures/real-library-manifest.tsv, a name in scripts/vectors.sha256, a
# format! interpolation) is not enumerated individually. Its parent directory
# is, and RESOLVE requires that directory to exist and be non-empty. This is a
# preflight against silent drift, not a proof that every byte a test reads is
# present.

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
MANIFEST=$ROOT/scripts/fixture-library.tsv
FIXTURES=${EC_FIXTURES:-$ROOT/fixtures}
REQUIRE=${EC_REQUIRE_FIXTURES:-0}
LINK_SCRIPT=$ROOT/scripts/link-fixtures.sh

fail=0
note() { [ "$REQUIRE" = 0 ] || echo "$@"; }

echo "verify-fixture-library: root=$ROOT fixtures=$FIXTURES EC_REQUIRE_FIXTURES=${REQUIRE:-0}"

# --- SHAPE ---------------------------------------------------------------
if [ ! -f "$MANIFEST" ]; then
    echo "FAIL: $MANIFEST is missing -- run scripts/gen-fixture-library.sh" >&2
    exit 1
fi
rows=$(grep -cv '^#' "$MANIFEST")
if [ "$rows" -eq 0 ]; then
    echo "FAIL: $MANIFEST has no rows" >&2
    exit 1
fi
malformed=$(awk -F'\t' '!/^#/ && NF != 5 { print NR": "$0 }' "$MANIFEST")
if [ -n "$malformed" ]; then
    echo "FAIL: malformed rows in $MANIFEST (want 5 tab-separated columns):" >&2
    echo "$malformed" >&2
    exit 1
fi
note "  shape: $rows rows"

# --- ROOT MODE (i) --------------------------------------------------------
root_present=1
[ -d "$FIXTURES" ] || root_present=0
if [ "$root_present" -eq 0 ]; then
    tag=SKIP
    [ "$REQUIRE" != 0 ] && tag=FAIL
    echo "$tag [mode i]: fixture root $FIXTURES does not exist." >&2
    echo "      Root fixtures/ is .gitignore:2, so a linked worktree never has it --" >&2
    echo "      that is why the vector gates skip GREEN here. Fix the TREE, not the" >&2
    echo "      code: provision the library, or run the fixture-linking script once" >&2
    if [ -x "$LINK_SCRIPT" ]; then
        echo "      it lands: $LINK_SCRIPT"
    else
        echo "      it exists (scripts/link-fixtures.sh -- another lane; not in this tree yet)."
    fi
    [ "$REQUIRE" != 0 ] && fail=1
fi

# --- RESOLVE --------------------------------------------------------------
missing=()
empty=()
while IFS=$'\t' read -r path required_by provenance status sum; do
    case $path in
        \#* | '') continue ;;
    esac
    [ "$status" = absent-pin ] && continue   # known gap, reported as a finding
    case $path in
        fixtures/*) abs=$FIXTURES/${path#fixtures/} ;;
        *) abs=$ROOT/$path ;;
    esac
    if [ ! -e "$abs" ]; then
        missing+=("$path  <-  $required_by  (provenance: $provenance)")
    elif [ -d "$abs" ] && [ -z "$(ls -A -- "$abs" 2>/dev/null)" ]; then
        empty+=("$path  <-  $required_by  (provenance: $provenance)")
    fi
done <"$MANIFEST"

if ((${#missing[@]})) && [ "$root_present" -eq 1 ]; then
    if [ "$REQUIRE" != 0 ]; then
        echo "FAIL [mode ii]: ${#missing[@]} referenced fixture path(s) do not resolve," >&2
        echo "      inside a tree that HAS a fixture root -- the generator or the pin is" >&2
        echo "      genuinely missing:" >&2
        printf '  %s\n' "${missing[@]}" >&2
        fail=1
    else
        echo "SKIP: ${#missing[@]} referenced fixture path(s) do not resolve" >&2
        echo "      (EC_REQUIRE_FIXTURES unset, so this is reported, not failed):" >&2
        printf '  %s\n' "${missing[@]}" >&2
    fi
fi
if ((${#empty[@]})); then
    if [ "$REQUIRE" != 0 ]; then
        echo "FAIL [mode ii]: ${#empty[@]} referenced fixture DIRECTORY(ies) exist but are empty:" >&2
        printf '  %s\n' "${empty[@]}" >&2
        fail=1
    else
        echo "SKIP: ${#empty[@]} referenced fixture DIRECTORY(ies) exist but are empty:" >&2
        printf '  %s\n' "${empty[@]}" >&2
    fi
fi
note "  resolve: ${#missing[@]} missing, ${#empty[@]} empty"

# --- FINDINGS (not failures, but stated) ---------------------------------
no_prov=$(awk -F'\t' '!/^#/ && $3 ~ /^none/ {print $1}' "$MANIFEST" | sort -u)
absent=$(awk -F'\t' '!/^#/ && $4 == "absent-pin" {print $1"\t"$3}' "$MANIFEST" | sort -u)
if [ -n "$no_prov" ]; then
    n=$(printf '%s\n' "$no_prov" | wc -l)
    echo "FINDING: $n pinned fixture(s) have NO generator -- provenance is prose in a" >&2
    echo "         gate comment only. A pin claim must cite the originating encoder" >&2
    echo "         program and the artefact sha256 (the sha256 IS recorded in column 5):" >&2
    printf '  %s\n' $no_prov >&2
fi
if [ -n "$absent" ]; then
    echo "FINDING: $(printf '%s\n' "$absent" | wc -l) KNOWN-ABSENT pin(s), kept as rows so the gap stays visible:" >&2
    printf '  %s\n' "$absent" >&2
fi

# --- VECTORS -------------------------------------------------------------
vec_rows=$(awk -F'\t' '!/^#/ && $1 ~ /^fixtures\/vectors\//' "$MANIFEST" | wc -l)
vec_missing=$(awk -F'\t' '!/^#/ && $1 ~ /^fixtures\/vectors\// && $4 != "ok"' "$MANIFEST" | wc -l)
echo "  vectors: $vec_rows referenced rows, $vec_missing not ok" >&2
echo "  vectors: nothing in crates/ reads a .tar.gz; the fleet hosts carry the" >&2
echo "           EXTRACTED sets, so the blobs are not required at test time" >&2

# --- DRIFT ----------------------------------------------------------------
regen=$(mktemp)
patch=$(mktemp)
trap 'rm -f "$regen" "$patch"' EXIT
if ! "$ROOT/scripts/gen-fixture-library.sh" "$regen" >/dev/null 2>&1; then
    echo "FAIL: scripts/gen-fixture-library.sh did not run" >&2
    exit 1
fi
if ! diff -u "$MANIFEST" "$regen" >"$patch" 2>&1; then
    if [ "$REQUIRE" != 0 ]; then
        echo "FAIL: scripts/fixture-library.tsv is stale -- the code reaches fixtures" >&2
        echo "      this host does not have. Commit the regenerated manifest, or fetch" >&2
        echo "      the missing library. (patch follows)" >&2
        sed -n '1,40p' "$patch" >&2
        fail=1
    else
        echo "SKIP: scripts/fixture-library.tsv is stale (EC_REQUIRE_FIXTURES unset, so this" >&2
        echo "      is reported, not failed). The code reaches fixtures this host lacks." >&2
        sed -n '1,20p' "$patch" >&2
    fi
else
    note "  drift: manifest matches the code"
fi

if [ "$fail" -ne 0 ]; then
    echo "verify-fixture-library: RED" >&2
    exit 1
fi
echo "verify-fixture-library: GREEN ($rows rows)"
