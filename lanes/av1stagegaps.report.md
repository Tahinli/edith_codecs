# lane-av1stagegaps — a staged VPS suite can no longer run with its preflight unverified

Worktree `/home/tahinli/.cache/wt/av1stagegaps`, branch `lane-av1stagegaps`, base
`edffad9e`. Scope: the staging recipe, not the decoder.

## 1. Host C's checker failure, reproduced with stderr visible

The driver log named no cause at all:

    === ORACLE RUNG CHECKER
    rungs_exit=1 ok_count=0
    === SUITE

`ok_count=0` is the tell: a checker that ran and failed prints its own `FAIL`
lines, so zero ok lines with zero detail means it never got as far as asserting
anything. The driver's `$(...)` capture also dropped stderr, which is where the
reason is printed.

Reproduction, on the host, from the staged tree, real paths, stderr visible:

    $ ssh tCloud@51.195.223.40
    $ cd /home/tCloud/gates/wave3d && git log --oneline -1
    48d8c21 staged
    $ nice -n 19 bash scripts/check-aom-oracle-rungs.sh; echo "rungs_exit=$?"
    no oracle source at /home/tCloud/.cache/aom-oracle/src/av1/decoder/decodeframe.c
    rungs_exit=1
    $ ls ~/.cache/aom-oracle/
    build
    $ ls ~/.cache/aom-oracle/src
    ls: cannot access '/home/tCloud/.cache/aom-oracle/src': No such file or directory
    $ git -C ~/.cache/aom-oracle/build rev-parse --show-toplevel
    fatal: not a git repository ...

**Named missing prerequisite:** the libaom **source tree** at
`$AOM_ORACLE_SRC` (default `~/.cache/aom-oracle/src`, a git checkout carrying
base ref `v3.13.3`). The host carries only `~/.cache/aom-oracle/build` — the
`aomdec`/`aomenc` **binaries** the suite itself needs. The checker derives
rungs from source and diffs them against pristine upstream files, so with no
source it asserts nothing.

Not a failing rung, not a path typo, not an env problem: an absent checkout. Fix
on the host is either `git clone -b v3.13.3 aom ~/.cache/aom-oracle/src` or the
explicit waiver in §3. The suite's own binary dependency is unaffected, which is
why the run was still meaningful — but its **rung coverage was not verified
there**, and the log said nothing about that.

## 2. The fix

Three states must be distinguishable, and two of them must be fatal.

### 2a. `scripts/check-aom-oracle-rungs.sh` — a distinct exit code for an absent prerequisite

`diff -u` against main:

    +# EXIT CONTRACT (lane-av1stagegaps). ...
    +#   0  GREEN   every assertion below held.
    +#   3  NOTRUN  a PREREQUISITE is absent (no oracle source tree, or the base ref
    +#              is not in it). Nothing was asserted: this is NOT a pass and NOT a
    +#              defect. scripts/av1-suite-preflight.sh maps it to `not-run` and
    +#              BLOCKS the suite unless a waiver names the host.
    +#   1  FAIL    a derived file violates the rung contract, or the instrument
    +#              script errored / was not idempotent.
    +die_prereq() { echo "PREREQ-ABSENT: $*" >&2; exit 3; }
    ...
    -[ -f "$DERIVED" ] || { echo "no oracle source at $DERIVED" >&2; exit 1; }
    -[ -f "$RECONINTRA" ] || { echo "no oracle source at $RECONINTRA" >&2; exit 1; }
    +[ -f "$DERIVED" ] || die_prereq "no oracle source at $DERIVED (provision it: git clone -b v3.13.3 --depth 1 aom at $SRC)"
    +[ -f "$RECONINTRA" ] || die_prereq "no oracle source at $RECONINTRA (provision it: git clone -b v3.13.3 --depth 1 aom at $SRC)"
     git -C "$SRC" rev-parse --verify "$BASE^{commit}" >/dev/null 2>&1 || {
    -  echo "base ref '$BASE' is not a commit in $SRC" >&2
    -  exit 1
    +  die_prereq "base ref '$BASE' is not a commit in $SRC"

Plus one more prerequisite of the same class: `systemd-run --setenv=TMPDIR=…`
points at a per-suite dir, and if staging created the unit but not the dir,
`mktemp -d` fails with something that reads like a checker bug. Now:

    +# systemd-run's --setenv=TMPDIR points at a per-suite dir; if staging created
    +# the unit but not the dir, mktemp fails with an error that reads like a
    +# checker bug. It is the same class as a missing oracle tree: not asserted.
    +probe="$(mktemp -d "${TMPDIR:-$HOME/.cache/tmp}/aom-rung-check.XXXXXX" 2>/dev/null)" ||
    +  die_prereq "scratch dir ${TMPDIR:-$HOME/.cache/tmp} does not exist (staging must mkdir it)"
    +rmdir "$probe"
    +WORK="$(mktemp -d "${TMPDIR:-$HOME/.cache/tmp}/aom-rung-check.XXXXXX")"

Existing `exit 1` paths are unchanged, so any caller that only checks non-zero
still sees a failure. `set -euo pipefail` is untouched.

### 2b. `scripts/av1-suite-preflight.sh` (new) — the gate

Runs both preflight pieces, prints every detail line of each (stdout and stderr
together, indented — the thing the old driver threw away), then one verdict:

    === PIN-GATE PREFLIGHT (EC_REQUIRE_FIXTURES=1)
      ...verify-fixture-library output...
    pin_audit_exit=0 state=GREEN
    === ORACLE RUNG CHECKER
      ok   legacy u8 row loops left in the derived file               0
      ...
    rungs_exit=0 ok_count=14 state=GREEN
    === VERDICT
    VERDICT=GREEN

States and the resulting verdict:

| pin_audit | rung_check | verdict | exit |
|---|---|---|---|
| GREEN | GREEN | GREEN | 0 |
| GREEN | NOT-RUN | WAIVED **iff** `SUITE_RUNG_WAIVER` is non-empty, else BLOCKED | 0 / 1 |
| GREEN | FAIL | BLOCKED | 1 |
| GREEN | ABSENT (no checker in the tree) | BLOCKED, **never waivable** | 1 |
| FAIL | any | BLOCKED | 1 |
| ABSENT | any | BLOCKED | 1 |

NOT-RUN vs ABSENT is the load-bearing distinction: NOT-RUN is a provisioning fact
(the checker ran and said so) and can be waived with a recorded reason; ABSENT
is the host-A defect — a staging with no checker at all — and has no waiver path,
because `SUITE_RUNG_WAIVER` only ever applies to the NOT-RUN branch. An empty
`SUITE_RUNG_WAIVER=` (a real shape, from a unit's `Environment=`) counts as
absent, so it cannot accidentally authorise a run.

### 2c. `scripts/av1-stage-suite.sh` (new) — a staging that cannot skip the gate

Wraps staging and runs the gate under `set -e` **before** `systemd-run`, so a
blocked verdict means no unit is ever created:

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

It also carries the two staging defects the fleet actually hit, as refuses rather
than as prose: a tree that is not a git checkout, a **populated** index (bare
`git init` makes every pin look uncommitted — the wave-3c-b correction), and
both `lanes/*.expected.txt` dumps.

## 3. Red-before / green-after

### Red — the hand-written driver shape, on the real missing-oracle-source case

    $ cat /tmp/av1stage-demo/old-driver.sh
    out=$(bash "$TREE/scripts/check-aom-oracle-rungs.sh" 2>/dev/null); rungs_exit=$?
    ok_count=$(printf '%s\n' "$out" | grep -c '^ok ' || true)
    echo "rungs_exit=$rungs_exit ok_count=$ok_count"
    echo "=== SUITE"
    echo "cargo test -p ec-av1 --lib   <-- THIS RAN ANYWAY in the wave-3d staging"
    $ AOM_ORACLE_SRC=/tmp/av1stage-demo/no-such-src bash /tmp/av1stage-demo/old-driver.sh
    === ORACLE RUNG CHECKER
    rungs_exit=3 ok_count=0
    === SUITE
    cargo test -p ec-av1 --lib   <-- THIS RAN ANYWAY in the wave-3d staging
    old_driver_exit=0

Same staging, through the new driver:

    $ AOM_ORACLE_SRC=/tmp/av1stage-demo/no-such-src SUITE_PIN_CHECKER=$STUB \
        bash scripts/av1-stage-suite.sh --name redemo --tree /tmp/av1stage-demo/tree --dry-run
    === PIN-GATE PREFLIGHT + ORACLE RUNG CHECKER
    === PIN-GATE PREFLIGHT (EC_REQUIRE_FIXTURES=1)
      ok  a
    pin_audit_exit=0 state=GREEN
    === ORACLE RUNG CHECKER
      PREREQ-ABSENT: no oracle source at /tmp/av1stage-demo/no-such-src/av1/decoder/decodeframe.c (provision it: git clone -b v3.13.3 --depth 1 aom at /tmp/av1stage-demo/no-such-src)
    rungs_exit=3 ok_count=0 state=NOT-RUN
    === VERDICT
    VERDICT=BLOCKED
    PREFLIGHT BLOCKED: pin_audit=GREEN rung_check=NOT-RUN.
      A staging with a failed or missing check must NOT reach === SUITE.
      rung_check=not-run (missing oracle source) is waivable only via
      SUITE_RUNG_WAIVER='<host and reason>'; an absent checker never is.
    gate_exit=1

`=== SUITE` never printed. Note the state is now readable from the log alone,
which is the actual fix to `rungs_exit=1 ok_count=0` with no detail.

### Green — waived, on the real wave-3d tree

    $ AOM_ORACLE_SRC=/tmp/av1stage-demo/no-such-src \
      SUITE_RUNG_WAIVER="host 51.195.223.40 carries oracle binaries only; rung coverage verified on the build host" \
      EC_FIXTURES=/…/fixtures \
      bash scripts/av1-stage-suite.sh --name redemo3d --tree /tmp/av1stage-demo/wave3d --dry-run
      …
      verify-fixture-library: GREEN (299 rows)
    pin_audit_exit=0 state=GREEN
    === ORACLE RUNG CHECKER
      PREREQ-ABSENT: no oracle source at /tmp/av1stage-demo/no-such-src/av1/decoder/decodeframe.c (…)
    rungs_exit=3 ok_count=0 state=NOT-RUN
    === VERDICT
    VERDICT=WAIVED (rung_check=not-run waived: host 51.195.223.40 carries oracle binaries only; rung coverage verified on the build host)
    === SUITE
    dry-run: would start unit redemo3d-suite …
    gate_exit=0

### Green — fully green, oracle source present

    $ EC_FIXTURES=/…/fixtures bash scripts/av1-stage-suite.sh --name redemo3d2 --tree … --dry-run
      ok   instrument-aom-oracle.sh derives the depth-correct, byte-checked rungs (base v3.13.3)
    rungs_exit=0 ok_count=14 state=GREEN
    === VERDICT
    VERDICT=GREEN
    === SUITE
    gate_exit=0

### The gate's own self-test (non-vacuous: red the checker, the gate must stop)

    $ bash scripts/av1-suite-preflight.sh --self-test; echo "selftest_exit=$?"
    ok   1 an all-green staging is GREEN
    ok   2 a FAILING rung checker blocks the suite
    ok   3 not-run without a waiver blocks the suite
    ok   4 not-run with a named waiver is WAIVED, reason recorded
    ok   5 an empty waiver does not authorise a run
    ok   6 a tree with NO rung checker is blocked even with a waiver
    ok   7 a tree with NO pin audit is blocked even with a waiver
    ok   8 a FAILING pin audit blocks the suite
    selftest_exit=0

Two traps hit while writing it, both worth recording:

- `set -o pipefail` makes `verdict | grep -q '^VERDICT=BLOCKED$'` return
  verdict's own non-zero status even when grep matched, so every BLOCKED case
  read as a self-test failure. Capture into a variable, then compare.
- The stub helper printed its `exit` code as the script body, so a "green" stub
  ran `exit echo "…"`. Argument order, not logic.

## 4. Per-host audit of the two running suites (read-only, `nice -n 19`)

Suite test counts taken immediately before and after each audit block: unchanged
on both hosts, so nothing was disturbed.

| host | tree | suite running | pin audit | pin self-test | rung checker | tree satisfies preflight? |
|---|---|---|---|---|---|---|
| C `51.195.223.40` | `~/gates/wave3d` @ `48d8c21` | `wave3d-suite`, `470 ok` before and after | `COUNT total=8 committed=21 uncommitted=0 ignored=0 assertless=0` → GREEN | `PASS`, exit 0 | **exit 1, no oracle `src/`** → now NOT-RUN | **no, without a waiver**: pin side satisfies it, rung side cannot be satisfied without provisioning or an explicit waiver |
| A `2.28.124.204` | `~/gates/wave3e` | `wave3e-suite`, `143 ok` before and after | `COUNT total=10 committed=0 uncommitted=25` → **RED** | `PASS`, exit 0 | **exit 1, no oracle `src/`** → now NOT-RUN | **no, twice over**: `git ls-files` = 0 entries (bare `git init`, no `git add -A`) so every pin reads uncommitted, and the rung check is unsatisfiable |

Raw, host C:

    $ cd ~/gates/wave3d
    $ grep -c '^test .* \.\.\. ok$' suite.log
    470
    $ nice -n 19 python3 scripts/pin-gate-audit.py | grep ^COUNT
    COUNT	total=8	committed=21	uncommitted=0	ignored=0	assertless=0
    $ nice -n 19 python3 scripts/pin-gate-audit.py --self-test | tail -1
    SELFTEST	PASS	a doc comment between two gates left both counted as crate_pin
    $ nice -n 19 bash scripts/check-aom-oracle-rungs.sh; echo "rungs_exit=$?"
    no oracle source at /home/tCloud/.cache/aom-oracle/src/av1/decoder/decodeframe.c
    rungs_exit=1
    $ grep -c '^test .* \.\.\. ok$' suite.log
    470

Raw, host A:

    $ cd ~/gates/wave3e
    $ grep -c '^test .* \.\.\. ok$' ~/gates/wave3e-suite.log
    143
    $ git ls-files | wc -l
    0
    $ nice -n 19 python3 scripts/pin-gate-audit.py | grep ^COUNT
    COUNT	total=10	committed=0	uncommitted=25	ignored=0	assertless=0
    $ nice -n 19 python3 scripts/pin-gate-audit.py --self-test | tail -1
    SELFTEST	PASS	a doc comment between two gates left both counted as crate_pin
    $ nice -n 19 bash scripts/check-aom-oracle-rungs.sh; echo "rungs_exit=$?"
    no oracle source at /home/tCloud/.cache/aom-oracle/src/av1/decoder/decodeframe.c
    rungs_exit=1
    $ grep -c '^test .* \.\.\. ok$' ~/gates/wave3e-suite.log
    143

Read on the audits: **neither running suite's tree satisfies the preflight, and
host A's pin-census red is a staging artefact, not a code defect** — the same
artefact the wave-3c-b report described, still reproducible. Host A's suite
results are not invalidated by it (the census reads pins, it does not gate
decoder behaviour), but a census claiming `committed=0` on 25 pins must not be
quoted as a pin-tracking measurement.

## 5. One sentence for the merge report

> The wave-3d run on `51.195.223.40` verified its fixture pins (census
> `committed=21 uncommitted=0`, self-test PASS) but **not** its oracle rung
> derivation — that host has `~/.cache/aom-oracle/build` and no `src/` tree, so
> the rung checker asserted nothing there and the driver's `rungs_exit=1
> ok_count=0` line was a missing prerequisite, not a failing check; the 14-ok
> figure remains a local (build-host) measurement only.

## 6. Skill update

`manage_skill update ec-av1stage-staging` — no such skill; the existing managed
skill `ec-av1-vps-suite-stage` is updated in place rather than duplicated, with
the new gate as the staging recipe and this trap added.

## 7. Not done

- No host was re-staged and no suite was restarted; §4 is read-only by design.
- Neither host was provisioned with `~/.cache/aom-oracle/src` — that is a fleet
  decision, and a `git clone` of libaom on two hosts was not in this lane's scope.
- The checker's `exit 1` callers (lane reports) keep working; no caller was
  migrated to the new `3`.
- `scripts/av1-stage-suite.sh` is exercised only with `--dry-run` on a scratch
  tree; it has not started a real unit.
