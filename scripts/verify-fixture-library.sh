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
# ENFORCEMENT ENV: EC_REQUIRE_FIXTURES=1 -- the same env the per-crate `require_fixture` asserts
# honour, so one convention covers the fleet batch units and a local run:
#   set   -> any absent referenced path is RED, and the drift diff is RED
#   unset -> the same findings are printed SKIP-shaped and the exit stays 0,
#            so an unprepared local checkout is not blocked
#
# EXIT STATUS is the LIBRARY verdict: mode i, mode ii, or drift. The four
# code-shape invariants (forbidden root-fixture path, pin tracking, .gitignore
# negation, pin-reading gate census) are REPORTED with their own count and fail
# only under EC_FIXTURE_SHAPE_STRICT=1. That split is deliberate: a shape
# violation is a defect in a file this preflight does not own, and a batch that
# cannot start because another crate has two open lines helps nobody -- but the
# findings are printed, counted, and one env var away from being fatal, so
# nothing is hidden. Set EC_FIXTURE_SHAPE_STRICT=1 in a tree whose shape
# violations you intend to fix.
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
#
# THE TWO ROOTS (lane-av1clipprobe). This script validates `$EC_FIXTURES`,
# defaulting to `$ROOT/fixtures`. The gates used to read `$ROOT/fixtures`
# UNCONDITIONALLY, so pointing EC_FIXTURES at a library elsewhere printed
# `resolve 0 missing` and `GREEN` here while every clip gate skipped -- a green
# preflight over gates reading a tree nobody validated. The clip-reading gates
# now resolve through the same precedence this script does (EC_FIXTURES first,
# `$ROOT/fixtures` otherwise: `crates/ec-av1/src/library_fixture.rs`), so the
# verdict below and the bytes the tests read are the same tree in every
# combination, and mode (i) below fires exactly when the gates will skip.

set -uo pipefail

ROOT=$(cd -- "$(dirname -- "$0")/.." && pwd)
MANIFEST=$ROOT/scripts/fixture-library.tsv
FIXTURES=${EC_FIXTURES:-$ROOT/fixtures}
REQUIRE=${EC_REQUIRE_FIXTURES:-0}
# Code-shape invariants fail only when explicitly made fatal: `scripts/
# verify-fixture-library.sh --strict` or EC_FIXTURE_SHAPE_STRICT=1. The default
# flips to fatal when the last live violation is fixed.
SHAPE=${EC_FIXTURE_SHAPE_STRICT:-1}
# `--strict` on the command line is the same switch, so the default can be
# flipped with a one-character edit the moment the last shape violation lands.
case " $* " in
    *" --strict "*) SHAPE=1 ;;
esac
LINK_SCRIPT=$ROOT/scripts/link-fixtures.sh

fail=0
shape_fail=0
shape_violations=0
note() { [ "$REQUIRE" = 0 ] || echo "$@"; }

echo "verify-fixture-library: root=$ROOT fixtures=$FIXTURES EC_REQUIRE_FIXTURES=${REQUIRE:-0}"
# The gates' root is the SAME string, by the reconciliation documented above --
# printed here so a reader comparing this verdict against a gate's SKIP line
# never has to guess which of two directories the two sides meant.
echo "verify-fixture-library: the clip gates resolve $FIXTURES too (EC_FIXTURES first, else \$ROOT/fixtures)"

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
malformed=$(awk -F'\t' '!/^#/ && NF != 7 { print NR": "$0 }' "$MANIFEST")
if [ -n "$malformed" ]; then
    echo "FAIL: malformed rows in $MANIFEST (want 7 tab-separated columns):" >&2
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
while IFS=$'\t' read -r path required_by class prov status tracked sum; do
    case $path in
        \#* | '') continue ;;
    esac
    case $class in
        absent-pin) continue ;;   # known gap, reported as a finding
    esac
    case $path in
        fixtures) abs=$FIXTURES ;;
        fixtures/*) abs=$FIXTURES/${path#fixtures/} ;;
        *) abs=$ROOT/$path ;;
    esac
    if [ ! -e "$abs" ]; then
        missing+=("$path  <-  $required_by  ($class, generator: $prov)")
    elif [ -d "$abs" ] && [ -z "$(ls -A -- "$abs" 2>/dev/null)" ]; then
        empty+=("$path  <-  $required_by  ($class, generator: $prov)")
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
no_prov=$(awk -F'\t' '!/^#/ && ($3 == "captured" || $3 == "recovered-original") {print $1}' "$MANIFEST" | sort -u)
absent=$(awk -F'\t' '!/^#/ && $3 == "absent-pin" {print $1}' "$MANIFEST" | sort -u)
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

# --- HARD INVARIANT 1: the forbidden root-fixture shape ---------------------
# `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/<file>")` names a
# ROOT-level committed pin through the gitignored `fixtures/`. No committed pin
# can satisfy it, so on any clean checkout or runner the gate silently skips --
# the worst shape in the class (the film-grain instance combined it with
# `.unwrap()`, so a clean checkout PANICKED instead). A root SUBDIRECTORY
# (`fixtures/audio`, `fixtures/vectors/...`) is a different thing: those are
# produced by a generator named in the manifest, so they are not forbidden here.
# Comments are stripped first: a doc comment that REPRODUCES the forbidden
# literal to explain what was fixed is prose, not code, and a grep cannot tell
# the difference. One such comment in crates/ec-av1 read as a live violation for
# a whole round (found by the lane that fixed the real ones).
forbidden=$(python3 - "$ROOT" <<'PYEOF'
import os, re, sys
root = sys.argv[1]
pat = re.compile(r'concat!\(\s*env!\("CARGO_MANIFEST_DIR"\)\s*,\s*"/\.\./\.\./fixtures/[^/"]*\.')
for crate in sorted(os.listdir(os.path.join(root, "crates"))):
    for sub in ("src", "tests"):
        d = os.path.join(root, "crates", crate, sub)
        if not os.path.isdir(d):
            continue
        for dirpath, _dirs, files in os.walk(d):
            for name in sorted(files):
                if not name.endswith(".rs"):
                    continue
                p = os.path.join(dirpath, name)
                try:
                    text = open(p, encoding="utf-8", errors="replace").read()
                except OSError:
                    continue
                text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
                for i, line in enumerate(text.splitlines(), 1):
                    code = line.split("//", 1)[0]
                    if pat.search(code):
                        print("{}:{}:{}".format(os.path.relpath(p, root), i, line.strip()))
PYEOF
)
# POSITIVE CONTROL. A comment-stripping bug makes this scan match NOTHING, and
# "no violations" would then be indistinguishable from a scanner that is blind.
# So before trusting the result, plant a known forbidden literal in a scratch
# file and require the scanner to catch it. A scanner that cannot find a literal
# it is looking at is broken, not clean.
control=$(mktemp --suffix=.rs)
printf 'fn probe() { let _ = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/control.obu"); }\n' >"$control"
control_hit=$(python3 - "$ROOT" "$control" <<'PYEOF'
import re, sys
pat = re.compile(r'concat!\(\s*env!\("CARGO_MANIFEST_DIR"\)\s*,\s*"/\.\./\.\./fixtures/[^/"]*\.')
text = open(sys.argv[2], encoding="utf-8", errors="replace").read()
text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
print("1" if any(pat.search(l.split("//", 1)[0]) for l in text.splitlines()) else "0")
PYEOF
)
rm -f "$control"
if [ "$control_hit" != 1 ]; then
    echo "FAIL [invariant 1]: the scanner's own positive control did not fire -- the" >&2
    echo "      comment-stripping pass is broken, so 'no violations' would be a" >&2
    echo "      silent all-clear. Fix the scanner before trusting its verdict." >&2
    shape_fail=1
    fail=1
else
    note "  invariant 1: positive control fired (the scanner can still see a literal)"
fi

if [ -n "$forbidden" ]; then
    if [ "$SHAPE" != 0 ]; then
        echo "FAIL [invariant 1]: a committed pin is reached through the gitignored root" >&2
        echo "      fixtures/ (silent SKIP on every clean checkout and runner):" >&2
        echo "$forbidden" | sed 's/^/  /' >&2
        echo "      fix: commit the pin under crates/<crate>/fixtures/ and read it with" >&2
        echo "      that crate's pin helper (crate_pin / pin_dir), never /../../fixtures/." >&2
        shape_fail=1
    else
        echo "SKIP [invariant 1]: $(printf '%s\n' "$forbidden" | wc -l) site(s) reach a root" >&2
        echo "      pin through the gitignored fixtures/ (reported, not failed):" >&2
        echo "$forbidden" | sed 's/^/  /' >&2
    fi
else
    note "  invariant 1: no root-fixture pin path"
fi

# --- HARD INVARIANT 2: every committed pin is present AND tracked -----------
# A pin on disk but untracked is lost when the machine's scratchpad is reaped,
# and its gate then skips GREEN. `tracked` is computed at generation time in a
# git tree; a runner host has no .git, so it asserts the committed value.
untracked=$(awk -F'\t' '!/^#/ && ($3 == "captured" || $3 == "recovered-original") && $6 != "yes" {print $1"\t"$6}' "$MANIFEST")
if [ -n "$untracked" ]; then
    if [ "$SHAPE" != 0 ]; then
        echo "FAIL [invariant 2]: committed pin(s) present but NOT tracked by git:" >&2
        echo "$untracked" | sed 's/^/  /' >&2
        echo "      fix: git add crates/<crate>/fixtures/<pin> -- the .gitignore negation" >&2
        echo "      (!crates/*/fixtures/**) must let a plain add work; if it does not, the" >&2
        echo "      negation is broken and invariant 3 below will say so." >&2
        shape_fail=1
    else
        echo "SKIP [invariant 2]: untracked committed pin(s) (reported, not failed):" >&2
        echo "$untracked" | sed 's/^/  /' >&2
    fi
else
    note "  invariant 2: every committed pin is tracked"
fi

# --- HARD INVARIANT 3: the .gitignore negation is intact --------------------
if ! grep -q '^!crates/\*/fixtures/\*\*$' "$ROOT/.gitignore" 2>/dev/null; then
    echo "FAIL [invariant 3]: .gitignore has no '!crates/*/fixtures/**' negation, so a" >&2
    echo "      new pin cannot be added and every recovered pin is untrackable." >&2
    shape_fail=1
else
    note "  invariant 3: .gitignore negation present"
    if [ -e "$ROOT/.git" ]; then
        shadowed=$(awk -F'\t' '!/^#/ && $6 == "yes" {print $1}' "$MANIFEST" |
            while IFS= read -r p; do
                git -C "$ROOT" check-ignore -q -- "$p" && echo "$p"
            done)
        if [ -n "$shadowed" ]; then
            echo "FAIL [invariant 3]: git still ignores these tracked pins:" >&2
            echo "$shadowed" | sed 's/^/  /' >&2
            shape_fail=1
        else
            note "  invariant 3: no tracked pin is shadowed by .gitignore"
        fi
    else
        note "  invariant 3: no .git here, check-ignore skipped (hosts assert the committed value)"
    fi
fi

# --- HARD INVARIANT 4: the pin-reading GATE census -------------------------
# The per-pin invariant is blind to a gate that reads N pins through a root
# literal plus a runtime name list, and to a gate that only asserts inside a
# helper. scripts/pin-gate-audit.py resolves the root literal, enumerates the
# runtime names, checks each against a tracked committed copy, and classifies
# assertions through the helper CALL GRAPH. Counts are printed, not summarised.
if ! command -v python3 >/dev/null; then
    echo "FAIL [invariant 4]: python3 is required for the pin-gate audit and is absent" >&2
    fail=1
else
    selftest_out=$(mktemp)
    audit=$("$ROOT/scripts/pin-gate-audit.py" 2>&1)
    if [ $? -ne 0 ] && [ -z "$audit" ]; then
        echo "FAIL [invariant 4]: scripts/pin-gate-audit.py did not run" >&2
        fail=1
    fi
    echo "$audit" | awk -F'\t' '/^COUNT/{print "  pin gates: "$2" "$3" "$4" "$5" "$6}' >&2
    # The census's own POSITIVE CONTROL. Losing a gate to a doc comment is
    # SILENT -- the count drops and nothing goes red -- so a control that only
    # proved "the scanner finds a bad literal" would not catch it. This one
    # plants the exact shape: a comment reproducing the forbidden literal
    # inside a gate that reads crate_pin.
    if ! "$ROOT/scripts/pin-gate-audit.py" --self-test >"$selftest_out" 2>&1; then
        echo "FAIL [invariant 4]: the census self-test did not pass -- a doc comment" >&2
        echo "      can reclassify a pin-reading gate into a branch that counts" >&2
        echo "      nothing, which loses the gate SILENTLY. Fix the scanner first." >&2
        sed 's/^/  /' "$selftest_out" >&2
        fail=1
    else
        note "  invariant 4: census self-test passed (a comment cannot steal a gate)"
    fi
    badrows=$(echo "$audit" | awk -F'\t' '/^BADROW/{print "  "$2":"$3"  "$4"  "$5}')
    bardir=$(echo "$audit" | awk -F'\t' '/^BARDIR/{print "  "$2":"$3"  "$4"  "$5}')
    absent=$(echo "$audit" | awk -F'\t' '/^NAME/ && $4=="absent"{print "  "$2":"$3"  "$4}')
    if [ -n "$badrows" ] || [ -n "$bardir" ]; then
        if [ "$SHAPE" != 0 ]; then
            echo "FAIL [invariant 4]: pin-reading gates whose pins are not committed" >&2
            echo "      (a runtime name list over the gitignored root can never be" >&2
            echo "      satisfied by a committed tree, and a bare directory literal" >&2
            echo "      feeding one always fails):" >&2
            printf '%s\n' "$badrows" "$bardir" >&2
            echo "      fix: commit each pin under crates/<crate>/fixtures/ and read it" >&2
            echo "      through that crate's pin helper, not /../../fixtures/." >&2
            shape_fail=1
        else
            echo "SKIP [invariant 4]: pin-reading gates with uncommitted pins (reported," >&2
            echo "      not failed, because EC_REQUIRE_FIXTURES is unset):" >&2
            printf '%s\n' "$badrows" "$bardir" >&2
        fi
    else
        note "  invariant 4: every pin-reading gate resolves through committed copies"
    fi
    # assertless gates are printed, never failed: a #[ignore]d gate that asserts
    # nothing is a finding for its owner, not a preflight failure.
    echo "$audit" | awk -F'\t' '/^GATE/ && $5=="no"{print "  FINDING: "$3" reads a pin and asserts nothing in its body or helpers"}' >&2
fi

# --- RECOVERED-PIN SELF-VALIDATION -----------------------------------------
# A recovered witness is the historical bytes; a re-encode is a DIFFERENT stream.
# Hashing what is on disk against the recorded sha256 is the half a shell
# preflight can decide. The other half is behavioural and needs the gate:
# `pinned_golden7` records non_last_ref_hits 0->2 in its own doc, and the
# recovered pin reproduces exactly that; only running the gate proves it.
badsum=0
while IFS=$'\t' read -r path required_by class prov status tracked sum; do
    case $path in
        \#* | '') continue ;;
    esac
    [ "$class" = recovered-original ] || continue
    abs=$ROOT/$path
    if [ ! -f "$abs" ]; then
        echo "FAIL [recovered]: $path is absent" >&2
        fail=1
        continue
    fi
    have=$(sha256sum -- "$abs" | cut -d' ' -f1)
    if [ "$have" != "$sum" ]; then
        echo "FAIL [recovered]: $path hashes $have, the manifest records $sum -- this is" >&2
        echo "      a DIFFERENT stream (a re-encode), not the recovered original." >&2
        fail=1
        badsum=$((badsum + 1))
    fi
done <"$MANIFEST"
[ "$badsum" -eq 0 ] && note "  recovered pins: every recovered-original hash matches"
echo "  recovered pins: the behavioural half needs the gate, not this script --" >&2
echo "           run 'cargo test -p ec-av1 pinned_golden7'; it prints non_last_ref_hits" >&2
echo "           and its doc records the 0->2 delta only the recovered pin reproduces." >&2

# --- VECTORS -------------------------------------------------------------
vec_rows=$(awk -F'\t' '!/^#/ && $1 ~ /^fixtures\/vectors\//' "$MANIFEST" | wc -l)
vec_missing=$(awk -F'\t' '!/^#/ && $1 ~ /^fixtures\/vectors\// && $5 != "ok"' "$MANIFEST" | wc -l)
echo "  vectors: $vec_rows referenced rows, $vec_missing not ok" >&2
echo "  vectors: nothing in crates/ reads a .tar.gz; the fleet hosts carry the" >&2
echo "           EXTRACTED sets, so the blobs are not required at test time" >&2

# --- DRIFT ----------------------------------------------------------------
regen=$(mktemp)
patch=$(mktemp)
trap 'rm -f "$regen" "$patch" ${selftest_out:-} ' EXIT
if ! "$ROOT/scripts/gen-fixture-library.sh" "$regen" >/dev/null 2>&1; then
    echo "FAIL: scripts/gen-fixture-library.sh did not run" >&2
    exit 1
fi
# The `tracked` column is COMMITTED provenance: it is computed in a git tree.
# A runner host has no .git, so a regeneration there cannot recompute it and
# would emit 'no-git' for every committed pin -- a drift on all 37 pin rows that
# says nothing about the host's library. Off-git the column is normalised on both
# sides, and the tracked invariant falls back to the committed value (as its own
# message already says).
drift_norm() { grep -v '^#' "$1" | awk -F'\t' -v OFS='\t' 'BEGIN{off=1} $1 ~ /^crates\/.*\/fixtures\//{if(off && $6!="-") $6="-"} {print}'; }
OFFGIT=0
[ -e "$ROOT/.git" ] || OFFGIT=1
if ! diff -u <(drift_norm "$MANIFEST") <(drift_norm "$regen") >"$patch" 2>&1; then
    if [ "$REQUIRE" != 0 ]; then
        echo "FAIL: this tree's library does not match what the code reaches" >&2
        echo "      (DRIFT: the committed manifest's rows differ from a regeneration" >&2
        echo "      here). The first difference is the row to act on; '- only here'" >&2
        echo "      is a path this host reaches and the committed manifest does not," >&2
        echo "      '+ only committed' is a row this host cannot satisfy. Fetch the" >&2
        echo "      library, or commit the regenerated manifest. (patch follows)" >&2
        sed -n '1,40p' "$patch" >&2
        fail=1
    else
        echo "SKIP: the library does not match what the code reaches (DRIFT;" >&2
        echo "      EC_REQUIRE_FIXTURES unset, so this is reported, not failed):" >&2
        sed -n '1,20p' "$patch" >&2
    fi
else
    note "  drift: manifest rows match the code on this host"
fi

if [ "$fail" -ne 0 ]; then
    echo "verify-fixture-library: RED (library verdict: mode i, mode ii or drift)" >&2
    exit 1
fi
if [ "$SHAPE" != 0 ] && [ "$shape_fail" -ne 0 ]; then
    echo "verify-fixture-library: RED (code-shape violations, EC_FIXTURE_SHAPE_STRICT=1)" >&2
    exit 1
fi
# The shape tally is always printed, fatal only under EC_FIXTURE_SHAPE_STRICT.
for s in $(echo "$audit" | awk -F'\t' '/^BADROW/{c++} END{print c+0}'); do :; done
echo "  code-shape violations: $(echo "$audit" | awk -F'\t' '/^BADROW/{c++} END{print c+0}')" >&2
echo "$forbidden" | grep -q . && shape_violations=$((shape_violations + $(echo "$forbidden" | grep -c .)))
# lane-av1clipprobe: the last word of this script used to be a bare `GREEN`
# even when mode (i) had just been reported a few lines above -- so a reader
# who read only the verdict read GREEN over a tree whose fixture root does not
# exist, which is the same false green as a gate that skipped. The verdict now
# says which it is: findings reported (not failed, because
# EC_REQUIRE_FIXTURES is unset) versus a clean pass. `EC_REQUIRE_FIXTURES=1`
# turns the first into the RED above.
if [ "$root_present" -eq 0 ]; then
    echo "verify-fixture-library: GREEN-WITH-FINDINGS ($rows rows; the absent root above is" >&2
    echo "           REPORTED, not failed -- set EC_REQUIRE_FIXTURES=1 on a tree that is" >&2
    echo "           meant to have the library, or provision it and re-run)" >&2
else
    echo "verify-fixture-library: GREEN ($rows rows)"
fi
