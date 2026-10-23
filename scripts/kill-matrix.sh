#!/usr/bin/env bash
# kill-matrix.sh: the heavy-compute steps of a batched mutation kill matrix for one crate.
# Reusable for any crate; defaults target ec-av1. Run it on a server through scripts/remote-run.sh,
# from the workspace root (remote-run's cwd), with the EC_* environment set by the caller:
#
#   EC_MEMGUARD_MAX=12G EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom EC_REQUIRE_FIXTURES=1 \
#   EC_TOOL_CACHE_DIR=$HOME/.cache/ec-tool-cache \
#   scripts/remote-run.sh <lane> --host auto -- scripts/kill-matrix.sh <subcommand> [args]
#
# Subcommands (CRATE defaults to ec-av1; OUT is a directory under the workspace):
#   list   <OUT>                     mutants of $CRATE as JSON (cargo mutants --list --json)
#   cover  <TESTS> <OUT>             per-test region coverage: one instrumented run per test; TESTS
#                                    lines are "<ordinal>\t<name>"; writes OUT/<ordinal>.json (compact)
#   fastcov <OUT>                    coverage of the fast profile (the "fast suite")
#   mutrun <SAMPLE.json> <IDXFILE> <OUT> fast|heavy
#                                    apply each mutant index in IDX (one per line), run the fast
#                                    profile or the given heavy test list, record the failing tests
#                                    in OUT/<index>.fail, and revert the tree after each mutant.
#
# A mutant with a compile error is recorded as "unviable" and is not a kill or a survivor.
set -euo pipefail

CRATE=${CRATE:-ec-av1}
ROOT=$(pwd)
TMP=${TMPDIR:-$HOME/tmp}
mkdir -p "$TMP"
export EC_MEMGUARD_MAX=${EC_MEMGUARD_MAX:-12G}
export EC_REQUIRE_FIXTURES=${EC_REQUIRE_FIXTURES:-1}
export EC_AV1_AOM_DIR=${EC_AV1_AOM_DIR:-$HOME/fixtures/av1-aom}
export EC_TOOL_CACHE_DIR=${EC_TOOL_CACHE_DIR:-$HOME/.cache/ec-tool-cache}
NEXTEST_J=${NEXTEST_J:-4}
MUTATE="python3 $ROOT/scripts/mutate.py"

sub=${1:?subcommand}
shift

# Per-test profraw are kept per run; report merges what is on disk.
llvm_clean() { cargo llvm-cov clean --profraw-only -p "$CRATE" >/dev/null 2>&1 || true; }

# A child process that dies mid-write leaves a truncated .profraw, and llvm-cov report then fails
# for the whole merge. Delete every profile that llvm-profdata cannot read, and log the count.
prune_profraw() {
    local pd f n=0
    pd=$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-profdata
    local dir="${CARGO_TARGET_DIR:-target}/llvm-cov-target"
    while IFS= read -r f; do
        if ! "$pd" show "$f" >/dev/null 2>&1; then
            rm -f "$f"
            n=$((n + 1))
        fi
    done < <(find "$dir" -name '*.profraw' 2>/dev/null)
    echo "prune_profraw: deleted $n unreadable profraw under $dir" >&2
}

# Heavy mode: apply one mutant to this tree (scripts/mutate.py), run the listed heavy tests with
# --no-fail-fast, and record every failing test in OUT/<idx>.fail. cargo-mutants stops at the first
# failing test, so it cannot produce the full kill set that the matrix needs.
# A timeout records every listed test as killing (conservative: it keeps them).
heavy_one() {
    local json=$1 idx=$2 tests=$3 out=$4 expr="" name rc
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        expr+="${expr:+ | }test(=$name)"
    done < "$tests"
    rm -f "$out/$idx.fail" "$out/$idx.status"
    if [ -z "$expr" ]; then
        : > "$out/$idx.fail"; echo pass > "$out/$idx.status"; return 0
    fi
    python3 scripts/mutate.py apply "$json" "$idx" "$ROOT" > "$out/$idx.apply.txt" 2>&1
    set +e
    timeout 7200 cargo nextest run -p "$CRATE" --profile mut --no-fail-fast -j "$NEXTEST_J" -E "$expr" > "$out/$idx.log" 2>&1
    rc=$?
    set -e
    python3 scripts/mutate.py revert "$ROOT"
    if [ "$rc" -eq 124 ]; then
        cp "$tests" "$out/$idx.fail"; echo fail > "$out/$idx.status"
        echo "heavy_one: mutant $idx timed out; all covering tests recorded" >&2
        return 0
    fi
    if ! grep -q 'Summary' "$out/$idx.log"; then
        echo unviable > "$out/$idx.status"; return 0
    fi
    python3 scripts/kill-matrix-analyze.py failing "$out/$idx.log" "$tests" "$out/$idx.fail"
    if [ -s "$out/$idx.fail" ]; then echo fail > "$out/$idx.status"; else echo pass > "$out/$idx.status"; fi
}

# Fast mode: apply one mutant, run the fast profile (fail-fast is fine: any failure is a kill).
# Status: fail (a test failed, timed out or the test run was killed), pass, unviable (no test run).
# Baseline-failing test (unmutated fast run fails it), excluded so fail-fast cannot make every mutant "killed".
FAST_EXCL='not test(/aom_vectors_match_per_frame_md5$/)'
fast_one() {
    local json=$1 idx=$2 out=$3 rc
    rm -f "$out/$idx.status" "$out/$idx.fail"
    python3 scripts/mutate.py apply "$json" "$idx" "$ROOT" > "$out/$idx.apply.txt" 2>&1
    set +e
    timeout 7200 cargo nextest run -p "$CRATE" --profile fast -j "$NEXTEST_J" -E "$FAST_EXCL" > "$out/$idx.log" 2>&1
    rc=$?
    set -e
    python3 scripts/mutate.py revert "$ROOT"
    if [ "$rc" -eq 124 ]; then echo fail > "$out/$idx.status"; return 0; fi
    if ! grep -q 'Summary' "$out/$idx.log"; then echo unviable > "$out/$idx.status"; return 0; fi
    if grep -qE '^\s*(FAIL|TIMEOUT|SIG[A-Z]+|LEAK|ABORT)\b' "$out/$idx.log"; then echo fail > "$out/$idx.status"; else echo pass > "$out/$idx.status"; fi
}

case $sub in
fastbase)
    # Unmutated fast profile, no fail-fast: the baseline. Failing tests are listed in OUT/failing.txt.
    out=$1; mkdir -p "$out"
    set +e
    timeout 7200 cargo nextest run -p "$CRATE" --profile fast --no-fail-fast -j "$NEXTEST_J" > "$out/fast.log" 2>&1
    echo "rc=$?" > "$out/rc.txt"
    set -e
    grep -E '^\s*(FAIL|TIMEOUT|SIG[A-Z]+)\b' "$out/fast.log" | sed 's/\x1b\[[0-9;]*m//g' | sort -u > "$out/failing.txt" || true
    grep Summary "$out/fast.log" | tail -n 1 > "$out/summary.txt" || true
    ;;
list)
    out=$1; mkdir -p "$out"
    (cd "crates/$CRATE" && cargo mutants -p "$CRATE" --list --json) > "$out/mutants.json"
    echo "mutants: $(python3 -c "import json,sys;print(len(json.load(open('$out/mutants.json'))))")"
    ;;
cover)
    tests=$1; out=$2; mkdir -p "$out"
    # TESTS lines: "<ordinal><TAB><test name>". The ordinal names the output files, so shards
    # run on different hosts can be merged by ordinal.
    while IFS=$'\t' read -r i name; do
        [ -n "$name" ] || continue
        llvm_clean
        # A failing test still leaves its profile behind; the coverage is what matters here.
        cargo llvm-cov nextest --no-report --no-fail-fast -p "$CRATE" -j "$NEXTEST_J" \
            -E "test(=$name)" > "$out/$i.log" 2>&1 || {
            # Coverage of a failing run is still reported, but it is not a clean baseline.
            echo "cover: test $i exited non-zero: $name" >&2
            printf '%s\n' "$name" >> "$out/failed.txt"
        }
        prune_profraw
        # A missing report is not fatal: the test is recorded as "nocov" and the analysis keeps it.
        if cargo llvm-cov report --json --output-path "$TMP/cov-$i.json" >/dev/null 2>&1 \
            && python3 scripts/kill-matrix-analyze.py compact "$TMP/cov-$i.json" "$out/$i.json"; then
            printf '%s\t%s\n' "$i" "$name" >> "$out/index.tsv"
        else
            echo "cover: no coverage report for test $i: $name" >&2
            printf '%s\n' "$name" >> "$out/nocov.txt"
            printf '%s\t%s\n' "$i" "$name" >> "$out/index.tsv"
        fi
        rm -f "$TMP/cov-$i.json"
    done < "$tests"
    ;;
fastcov)
    out=$1; mkdir -p "$out"
    llvm_clean
    cargo llvm-cov nextest --no-report --no-fail-fast -p "$CRATE" --profile fast -j "$NEXTEST_J" > "$out/fast.log" 2>&1 || true
    prune_profraw
    if ! { cargo llvm-cov report --json --output-path "$TMP/fast-cov.json" >/dev/null 2>&1 \
        && python3 scripts/kill-matrix-analyze.py compact "$TMP/fast-cov.json" "$out/fast.json"; }; then
        echo "fastcov: no coverage report; see $out/fast.log" >&2
    fi
    rm -f "$TMP/fast-cov.json"
    ;;
mutrun)
    # json: the sample; idxfile lines: "<index>" (fast) or "<index><TAB><tests file>" (heavy).
    # Each run applies exactly one mutant (mutate.py, exact span) and runs the tests. cargo-mutants'
    # --re/-F does not filter by name in this version (it tests the first N mutants of the list), so
    # it is not used for selection here.
    json=$1; idxfile=$2; out=$3; mode=$4
    mkdir -p "$out"
    while IFS=$'\t' read -r idx tests; do
        [ -n "$idx" ] || continue
        if [ "$mode" = heavy ]; then
            heavy_one "$json" "$idx" "$tests" "$out"
        else
            fast_one "$json" "$idx" "$out"
        fi
        echo "mutrun: $mode mutant $idx status=$(cat "$out/$idx.status")" >&2
    done < "$idxfile"
    ;;
*)
    sed -n '2,20p' "$0" >&2
    exit 2
    ;;
esac
