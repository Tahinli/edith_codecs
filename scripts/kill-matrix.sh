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

case $sub in
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
    # cargo-mutants applies, builds and runs each sampled mutant itself. Each one is selected by its
    # exact name (-F/--re over the names `--list` shows), so no span is applied by this script.
    json=$1; idxfile=$2; out=$3; mode=$4
    mkdir -p "$out"
    # idxfile lines: "<index>" (fast) or "<index><TAB><tests file>" (heavy, one list per mutant).
    while IFS=$'\t' read -r idx tests; do
        [ -n "$idx" ] || continue
        re=$(python3 scripts/kill-matrix-analyze.py regex "$json" "$idx")
        if [ "$mode" = fast ]; then
            sel=(--cargo-test-arg=--profile --cargo-test-arg=fast)
        else
            expr=""
            while IFS= read -r name; do
                [ -n "$name" ] || continue
                expr+="${expr:+ | }test(=$name)"
            done < "$tests"
            # --no-fail-fast: every covering heavy test runs, so the kill set is complete.
            sel=(--cargo-test-arg=--no-fail-fast --cargo-test-arg=-E "--cargo-test-arg=$expr")
        fi
        # Heavy runs get a hard per-mutant timeout (a hang counts as caught); fast runs keep the default.
        tmo=(--timeout-multiplier 3)
        [ "$mode" = fast ] || tmo=(--timeout "${HEAVY_TIMEOUT:-1200}")
        rm -rf "$out/$idx.d"
        set +e
        cargo mutants -p "$CRATE" --test-tool nextest --baseline skip "${tmo[@]}" \
            --re "$re" "${sel[@]}" --output "$out/$idx.d" > "$out/$idx.log" 2>&1
        rc=$?
        set -e
        mo="$out/$idx.d/mutants.out"
        if [ -f "$mo/caught.txt" ] && grep -qF "$(python3 scripts/kill-matrix-analyze.py name "$json" "$idx")" "$mo/caught.txt"; then
            echo fail > "$out/$idx.status"          # killed by the selected tests
        elif [ -f "$mo/timeout.txt" ] && grep -qF "$(python3 scripts/kill-matrix-analyze.py name "$json" "$idx")" "$mo/timeout.txt"; then
            echo fail > "$out/$idx.status"
        elif [ -f "$mo/unviable.txt" ] && grep -qF "$(python3 scripts/kill-matrix-analyze.py name "$json" "$idx")" "$mo/unviable.txt"; then
            echo unviable > "$out/$idx.status"
        elif [ -f "$mo/missed.txt" ] && grep -qF "$(python3 scripts/kill-matrix-analyze.py name "$json" "$idx")" "$mo/missed.txt"; then
            echo pass > "$out/$idx.status"          # survived the selected tests
        else
            echo "error rc=$rc" > "$out/$idx.status"
        fi
        # Validity evidence per mutant: which test binaries ran, and rustc vs runner errors.
        python3 scripts/kill-matrix-analyze.py evidence "$out/$idx.d" > "$out/$idx.evidence" 2>/dev/null || true
        if [ "$mode" = heavy ] && [ "$(cat "$out/$idx.status")" = fail ]; then
            python3 scripts/kill-matrix-analyze.py killers "$out/$idx.d" > "$out/$idx.fail"
        fi
        echo "mutrun: $mode mutant $idx status=$(cat "$out/$idx.status")" >&2
    done < "$idxfile"
    ;;
*)
    sed -n '2,20p' "$0" >&2
    exit 2
    ;;
esac
