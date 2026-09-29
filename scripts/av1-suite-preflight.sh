#!/usr/bin/env bash
# av1-suite-preflight.sh — the gate a VPS staging must pass BEFORE `=== SUITE`.
#
# WHY (lane-av1stagegaps, measured 2026-09-30). Three hosts, three different
# preflight realities, one identical green suite:
#
#   host C 51.195.223.40  the driver printed
#                           === ORACLE RUNG CHECKER
#                           rungs_exit=1 ok_count=0
#                           === SUITE
#                         and the suite ran. The real cause, with stderr
#                         visible, was a MISSING PREREQUISITE: the host has
#                         `~/.cache/aom-oracle/build` (aomdec/aomenc binaries)
#                         and no `src/` tree, so the checker asserted nothing.
#   host A 2.28.124.204   staged with a bare `cargo test -p ec-av1 --lib`. No
#                         pin-gate audit, no rung checker, no self-test: not a
#                         failure, an ABSENT check.
#   so "green suite" meant checker-passed, checker-failed and no-checker-ran
#   with equal confidence.
#
# WHAT THIS DOES. Runs both preflight pieces, prints one machine-readable state
# per piece, then a single VERDICT. Exit 0 only when every piece is GREEN or an
# explicitly waived NOT-RUN, so a `set -e` staging driver CANNOT reach
# `=== SUITE` on a failed or missing check.
#
#   pin_audit  = GREEN | FAIL | ABSENT   (verify-fixture-library.sh: shape,
#                                        resolve, drift, invariants 1-4 +
#                                        census self-test)
#   rung_check = GREEN | FAIL | NOT-RUN | ABSENT
#   VERDICT    = GREEN | WAIVED | BLOCKED
#
# NOT-RUN vs ABSENT is the distinction that matters. NOT-RUN means the checker
# RAN and reported a missing prerequisite (exit 3 from check-aom-oracle-rungs.sh)
# — a provisioning fact, not a code defect. ABSENT means the checker is not in
# this tree at all, which is exactly the host-A defect and is never waivable.
#
# WAIVER. not-run can be waived, but only with an explicit non-empty reason that
# is recorded in the verdict line, so a later reader can tell a deliberate
# waiver from a check that silently vanished:
#   SUITE_RUNG_WAIVER="host 51.195.223.40 carries oracle binaries only; rung
#                       coverage verified on the build host, see report"
# An empty waiver counts as absent, so a unit's Environment= carrying a bare
# SUITE_RUNG_WAIVER= cannot accidentally authorise a run.
#
# Usage:
#   scripts/av1-suite-preflight.sh [--tree DIR] [--self-test]
#
# Env:
#   EC_FIXTURES=<dir>            fixture root (default <tree>/fixtures)
#   EC_REQUIRE_FIXTURES=1        make an absent/empty fixture path fatal
#   SUITE_RUNG_WAIVER=<reason>   waive rung_check=not-run (see above)
#   SUITE_RUNG_CHECKER=<path>    override the rung checker (self-test stubs)
#   SUITE_PIN_CHECKER=<path>     override the pin preflight (self-test stubs)

set -uo pipefail

ROOT_DEFAULT=$(cd -- "$(dirname -- "$0")/.." && pwd)
TREE=$ROOT_DEFAULT
SELF_TEST=0
while [ $# -gt 0 ]; do
    case "$1" in
        --tree) TREE=$2; shift 2 ;;
        --self-test) SELF_TEST=1; shift ;;
        -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
PIN_CHECKER=${SUITE_PIN_CHECKER:-$TREE/scripts/verify-fixture-library.sh}
RUNG_CHECKER=${SUITE_RUNG_CHECKER:-$TREE/scripts/check-aom-oracle-rungs.sh}

pin_state=ABSENT
rung_state=ABSENT

run_pin() { # sets pin_state, prints every detail line
    pin_state=ABSENT
    if [ ! -f "$PIN_CHECKER" ]; then
        echo "pin_audit_state=ABSENT reason=pin preflight not in tree ($PIN_CHECKER)" >&2
        return
    fi
    local out rc
    out=$("$PIN_CHECKER" 2>&1)
    rc=$?
    printf '%s\n' "$out" | sed 's/^/  /'
    if [ $rc -eq 0 ]; then pin_state=GREEN; else pin_state=FAIL; fi
    echo "pin_audit_exit=$rc state=$pin_state"
}

run_rungs() { # sets rung_state, prints every detail line
    rung_state=ABSENT
    if [ ! -f "$RUNG_CHECKER" ]; then
        echo "rung_check_state=ABSENT reason=checker not in tree ($RUNG_CHECKER)" >&2
        return
    fi
    local out rc ok_count
    out=$("$RUNG_CHECKER" 2>&1)
    rc=$?
    ok_count=$(printf '%s\n' "$out" | grep -c '^ok ' || true)
    # The detail lines are the whole point: the 2026-09-30 driver logged
    # `rungs_exit=1 ok_count=0` and no reason at all. Every outcome here prints
    # the checker's own stdout and stderr together, indented.
    printf '%s\n' "$out" | sed 's/^/  /'
    case $rc in
        0) rung_state=GREEN ;;
        3) rung_state=NOT-RUN ;;
        *) rung_state=FAIL ;;
    esac
    echo "rungs_exit=$rc ok_count=$ok_count state=$rung_state"
}

verdict() { # echoes VERDICT; returns 0 iff GREEN or WAIVED
    # A pin audit that did not run or failed, and a rung check that is absent
    # or failed, are all fatal: no waiver exists for any of them. Only
    # rung_check=not-run (a missing prerequisite) is waivable, and only with a
    # non-empty reason.
    local v=BLOCKED
    local waived=""
    if [ "$pin_state" = GREEN ] && [ "$rung_state" = GREEN ]; then
        v=GREEN
    elif [ "$pin_state" = GREEN ] && [ "$rung_state" = NOT-RUN ]; then
        if [ -n "${SUITE_RUNG_WAIVER:-}" ]; then
            v=WAIVED
            waived=" (rung_check=not-run waived: ${SUITE_RUNG_WAIVER})"
        fi
    fi
    echo "VERDICT=$v$waived"
    if [ "$v" = BLOCKED ]; then
        {
            echo "PREFLIGHT BLOCKED: pin_audit=$pin_state rung_check=$rung_state."
            echo "  A staging with a failed or missing check must NOT reach === SUITE."
            echo "  rung_check=not-run (missing oracle source) is waivable only via"
            echo "  SUITE_RUNG_WAIVER='<host and reason>'; an absent checker never is."
        } >&2
        return 1
    fi
    return 0
}

self_test() {
    local d rc=0 out
    d=$(mktemp -d "${TMPDIR:-/tmp}/av1-preflight-selftest.XXXXXX")
    # shellcheck disable=SC2064
    trap "rm -rf '$d'" RETURN

    stub() { # name, exit-code, body
        printf '#!/usr/bin/env bash\n%s\nexit %s\n' "$3" "$2" >"$d/$1"
        chmod +x "$d/$1"
    }
    # `set -o pipefail` is on, so `verdict | grep -q ...` returns verdict's own
    # non-zero status even when grep matched, and every BLOCKED case would read
    # as a self-test failure. Capture first, then compare.
    v_of() { verdict 2>/dev/null; }
    expect() { # expected-verdict, label
        out=$("${@:3}" 2>/dev/null || true)
        :
    }

    # 1. all green -> GREEN
    stub pin-ok 0 'echo "verify-fixture-library: GREEN"'
    stub rungs-ok 0 'echo "ok   derives the rungs"'
    PIN_CHECKER=$d/pin-ok run_pin >/dev/null 2>&1
    RUNG_CHECKER=$d/rungs-ok run_rungs >/dev/null 2>&1
    if [ "$(v_of)" = "VERDICT=GREEN" ]; then
        echo "ok   1 an all-green staging is GREEN"
    else
        echo "FAIL 1 an all-green staging is GREEN"; rc=1
    fi

    # 2. rung checker FAILS (exit 1) -> BLOCKED. The mutation: red the checker
    #    and the gate must stop.
    stub rungs-bad 1 'echo "FAIL derived file violates the rung contract"'
    PIN_CHECKER=$d/pin-ok run_pin >/dev/null 2>&1
    RUNG_CHECKER=$d/rungs-bad run_rungs >/dev/null 2>&1
    if [ "$(v_of)" = "VERDICT=BLOCKED" ]; then
        echo "ok   2 a FAILING rung checker blocks the suite"
    else
        echo "FAIL 2 a FAILING rung checker blocks the suite"; rc=1
    fi

    # 3. rung checker NOT-RUN (exit 3), no waiver -> BLOCKED.
    stub rungs-notrun 3 'echo "PREREQ-ABSENT: no oracle source at ..."'
    PIN_CHECKER=$d/pin-ok run_pin >/dev/null 2>&1
    RUNG_CHECKER=$d/rungs-notrun run_rungs >/dev/null 2>&1
    if [ "$(v_of)" = "VERDICT=BLOCKED" ]; then
        echo "ok   3 not-run without a waiver blocks the suite"
    else
        echo "FAIL 3 not-run without a waiver blocks the suite"; rc=1
    fi

    # 4. not-run WITH a waiver -> WAIVED, and the reason lands in the verdict.
    if [ "$(SUITE_RUNG_WAIVER='host C carries oracle binaries only' v_of)" \
         = "VERDICT=WAIVED (rung_check=not-run waived: host C carries oracle binaries only)" ]
    then
        echo "ok   4 not-run with a named waiver is WAIVED, reason recorded"
    else
        echo "FAIL 4 not-run with a named waiver is WAIVED, reason recorded"; rc=1
    fi

    # 5. an EMPTY waiver authorises nothing: a unit Environment= carrying a bare
    #    SUITE_RUNG_WAIVER= is a real staging shape.
    if [ "$(SUITE_RUNG_WAIVER='' v_of)" = "VERDICT=BLOCKED" ]; then
        echo "ok   5 an empty waiver does not authorise a run"
    else
        echo "FAIL 5 an empty waiver does not authorise a run"; rc=1
    fi

    # 6. an ABSENT checker is never waivable: the host-A defect.
    PIN_CHECKER=$d/pin-ok run_pin >/dev/null 2>&1
    RUNG_CHECKER=$d/does-not-exist run_rungs >/dev/null 2>&1
    if [ "$(SUITE_RUNG_WAIVER='a waiver' v_of)" = "VERDICT=BLOCKED" ]; then
        echo "ok   6 a tree with NO rung checker is blocked even with a waiver"
    else
        echo "FAIL 6 a tree with NO rung checker is blocked even with a waiver"; rc=1
    fi

    # 6b. an ABSENT pin audit is fatal too, and not waivable by the same knob.
    PIN_CHECKER=$d/does-not-exist run_pin >/dev/null 2>&1
    RUNG_CHECKER=$d/rungs-ok run_rungs >/dev/null 2>&1
    if [ "$(SUITE_RUNG_WAIVER='a waiver' v_of)" = "VERDICT=BLOCKED" ]; then
        echo "ok   7 a tree with NO pin audit is blocked even with a waiver"
    else
        echo "FAIL 7 a tree with NO pin audit is blocked even with a waiver"; rc=1
    fi

    # 8. pin audit FAILING -> BLOCKED even when the rungs are green.
    stub pin-bad 1 'echo "FAIL [invariant 4]: census self-test did not pass"'
    PIN_CHECKER=$d/pin-bad run_pin >/dev/null 2>&1
    RUNG_CHECKER=$d/rungs-ok run_rungs >/dev/null 2>&1
    if [ "$(v_of)" = "VERDICT=BLOCKED" ]; then
        echo "ok   8 a FAILING pin audit blocks the suite"
    else
        echo "FAIL 8 a FAILING pin audit blocks the suite"; rc=1
    fi

    return $rc
}

if [ "$SELF_TEST" = 1 ]; then
    self_test
    exit $?
fi

echo "=== PIN-GATE PREFLIGHT (EC_REQUIRE_FIXTURES=${EC_REQUIRE_FIXTURES:-0})"
run_pin
echo "=== ORACLE RUNG CHECKER"
run_rungs
echo "=== VERDICT"
verdict
exit $?
