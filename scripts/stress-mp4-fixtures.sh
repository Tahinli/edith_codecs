#!/usr/bin/env bash
# stress-mp4-fixtures.sh: the multi-crate test command that once flaked on
# ec-mp4's fragmented-file test, run three times plus once at --test-threads=16.
# Every run starts from a cold fixture work directory (the derived mp4/mkv
# fixtures are made on first use), because the race only exists while those
# files are being written. Run it through scripts/remote-run.sh, which sets TMPDIR.
#
#   scripts/remote-run.sh mp4fixturerace --host auto -- scripts/stress-mp4-fixtures.sh
#
# Exit 0 only if every run passes. Logs go to $TMPDIR/stress-mp4-<n>.log.

set -uo pipefail
: "${TMPDIR:=/tmp}"
CRATES=(-p ec-h265 -p ec-vorbis -p ec-probe -p ec-ac3 -p oxideav-ac3 -p ec-mp4 -p ec-image -p ec-alac -p ec-mp3 -p ec-ogg)

fail=0
run() {
    local n=$1; shift
    rm -rf "$TMPDIR/ec-mp4-tests" "$TMPDIR/ec-matroska-tests"
    if cargo test --release "${CRATES[@]}" "$@" >"$TMPDIR/stress-mp4-$n.log" 2>&1; then
        echo "run $n ($*): PASS"
    else
        echo "run $n ($*): FAIL, see $TMPDIR/stress-mp4-$n.log"
        grep -E "panicked|packet [0-9]+|FAILED|failures:" "$TMPDIR/stress-mp4-$n.log" | head -10
        fail=1
    fi
}

run 1
run 2
run 3
run 4 -- --test-threads=16
exit $fail
