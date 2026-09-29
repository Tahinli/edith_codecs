#!/usr/bin/env bash
# av1-stage-suite.sh — stage an ec-av1 suite on a VPS host, behind the preflight
# gate. A staging whose pin audit or rung check FAILED, or whose checkers never
# ran, MUST NOT reach `=== SUITE`.
#
# WHY THIS EXISTS (lane-av1stagegaps, 2026-09-30). The hand-written staging
# driver logged the checker outcome and continued regardless:
#
#   === ORACLE RUNG CHECKER
#   rungs_exit=1 ok_count=0
#   === SUITE
#
# so a green suite on a host meant nothing about its preflight. Three states —
# passed, failed, never ran — all looked the same. This driver runs
# scripts/av1-suite-preflight.sh under `set -e` and stops there.
#
# Usage:
#   scripts/av1-stage-suite.sh --name <suite> --tree <dir> [--tarball <tgz>]
#                              [--skip <cargo-test-arg>]... [-- command...]
#
# Defaults: no cargo-test-arg means
#   cargo test -p ec-av1 --lib -- --test-threads=1
#
# Env (all passed to the unit):
#   CARGO_TARGET_DIR, TMPDIR       defaulted under $HOME/gates/<name>-*
#   EC_AV1_REQUIRE_AOMENC=1, EC_AV1_REQUIRE_FFMPEG=1, EC_REQUIRE_FIXTURES=1
#   SUITE_RUNG_WAIVER=<reason>     required to accept rung_check=not-run
#
# Exit: 0 the unit was started; non-zero the preflight BLOCKED and no unit
# exists. `systemctl --user is-active <name>-suite` is the only thing that
# starts a suite, so a blocked staging leaves the host idle on purpose.

set -uo pipefail

NAME=""
TREE=""
TARBALL=""
declare -a SKIPS=()
declare -a CMD=()
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --name) NAME=$2; shift 2 ;;
        --tree) TREE=$2; shift 2 ;;
        --tarball) TARBALL=$2; shift 2 ;;
        --skip) SKIPS+=(--skip "$2"); shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --) shift; CMD=("$@"); break ;;
        -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "$NAME" ] && [ -n "$TREE" ] || { echo "usage: --name <suite> --tree <dir> [--tarball <tgz>] [--skip <arg>]... [-- cmd...]" >&2; exit 2; }
[ -d "$TREE" ] || { echo "no such tree: $TREE" >&2; exit 2; }

# 1. The staged tree needs a real git checkout with a POPULATED index. A bare
#    `git init` leaves the index empty and pin-gate-audit.py's
#    `git ls-files --error-unmatch` then reports every pin as uncommitted — a
#    staging artefact indistinguishable from the defect invariant 4 exists to
#    catch (wave-3c-b correction). Test rev-parse, not `[ -d .git ]`: a worktree
#    copy and a `git archive` untar both leave a `.git` FILE.
git -C "$TREE" rev-parse --git-dir >/dev/null 2>&1 || {
    echo "staging defect: $TREE is not a git checkout; memguard-runner.sh dies with exit 128" >&2
    exit 2
}
if [ -z "$(git -C "$TREE" ls-files | head -1)" ]; then
    echo "staging defect: $TREE has an empty index; run git add -A after the untar" >&2
    exit 2
fi

# 2. Both gitignored lanes dumps: missing them fails exactly 2 tests that are
#    not a decoder regression.
for f in intrarect_dump.expected.txt wedge_libaom.expected.txt; do
    [ -f "$TREE/lanes/$f" ] || { echo "staging defect: $TREE/lanes/$f missing" >&2; exit 2; }
done

TARGET=${CARGO_TARGET_DIR:-$HOME/gates/target-$NAME}
TMP=${TMPDIR:-$HOME/gates/tmp-$NAME}
mkdir -p "$TARGET" "$TMP"

echo "=== host=$(hostname) commit=$(git -C "$TREE" rev-parse HEAD) date=$(date -Is)"
echo "=== cargo=$(command -v cargo || echo MISSING) aomenc=${EC_AV1_AOMENC:-unset} aomdec=${EC_AV1_AOMDEC:-unset} ffmpeg=$(command -v ffmpeg || echo MISSING)"
if [ -n "$TARBALL" ]; then
    echo "=== tarball=$(sha256sum "$TARBALL" | cut -c1-16) -> $TREE"
fi

# 3. THE GATE. `set -e` is the whole mechanism: a non-zero verdict aborts this
#    script before systemd-run is ever reached, so no unit exists and no suite
#    runs. There is no path from a blocked verdict to `=== SUITE`.
set -e
echo "=== PIN-GATE PREFLIGHT + ORACLE RUNG CHECKER"
EC_REQUIRE_FIXTURES=${EC_REQUIRE_FIXTURES:-1} \
EC_FIXTURE_SHAPE_STRICT=${EC_FIXTURE_SHAPE_STRICT:-1} \
    bash "$TREE/scripts/av1-suite-preflight.sh" --tree "$TREE"
set +e

echo "=== SUITE"
if [ "$DRY_RUN" = 1 ]; then
    echo "dry-run: would start unit $NAME-suite in $TREE (target $TARGET, tmp $TMP)"
    exit 0
fi

if [ ${#CMD[@]} -eq 0 ]; then
    CMD=(cargo test -p ec-av1 --lib -- --test-threads=1)
fi
declare -a quoted=()
for a in "${SKIPS[@]}"; do quoted+=("$a"); done

LOG=$HOME/gates/$NAME-suite.log
mkdir -p "$HOME/gates"
systemd-run --user --unit="$NAME-suite" -p MemoryMax=5G \
    -p WorkingDirectory="$TREE" \
    --setenv="CARGO_TARGET_DIR=$TARGET" \
    --setenv="TMPDIR=$TMP" \
    --setenv=EC_NOMEMGUARD=1 \
    --setenv=EC_AV1_REQUIRE_AOMENC=1 \
    --setenv=EC_AV1_REQUIRE_FFMPEG=1 \
    --setenv=EC_REQUIRE_FIXTURES=1 \
    --setenv="EC_FIXTURES=${EC_FIXTURES:-$TREE/fixtures}" \
    ${EC_AV1_AOMENC:+--setenv=EC_AV1_AOMENC=$EC_AV1_AOMENC} \
    ${EC_AV1_AOMDEC:+--setenv=EC_AV1_AOMDEC=$EC_AV1_AOMDEC} \
    --setenv="PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
    /usr/bin/bash -lc "cd '$TREE' && ${CMD[*]} ${quoted[*]-} > '$LOG' 2>&1"
echo "started $NAME-suite; real test output: $LOG"
