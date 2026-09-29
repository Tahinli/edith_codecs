# lanes/av1pinauditfix — the pin-gate audit reads its own test data

Branch `lane-av1pinauditfix`, base main `79b2a73f`, worktree
`/home/tahinli/.cache/wt/av1pinauditfix`. Scripts only; no decoder and no
`gate_coverage.rs` text changed.

## Was the RED pre-existing? YES — introduced 2026-09-29, not tonight

```
$ git log --oneline -S'/../../fixtures' -- crates/ec-av1/src/gate_coverage.rs
35deab68 test(av1): commit the 14 warp/ii/rect pins — recovered originals, not re-encodes
$ git log --oneline -S'a_gate_using_the_old_shape' -- crates/ec-av1/src/gate_coverage.rs
35deab68 test(av1): commit the 14 warp/ii/rect pins — recovered originals, not re-encodes
$ git log --oneline -S'BARDIR' -- scripts/pin-gate-audit.py
116445f5 preflight rework: path-free generated header, reproducible artifact, split exit status
```

Bisected on real checkouts of both commits:

```
$ git worktree add /tmp/wt-pre35 35deab68 && cd /tmp/wt-pre35
$ bash scripts/verify-fixture-library.sh
FAIL [invariant 1]: a committed pin is reached through the gitignored root
  crates/ec-av1/src/gate_coverage.rs:1919:let c = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/x.obu"));
FAIL [invariant 4]: pin-reading gates whose pins are not committed
  crates/ec-av1/src/gate_coverage.rs:1916:shapes  concat!  1/1 pin(s) not committed+tracked, e.g. x.obu

$ git checkout 35deab68~1        # ec4e9528
$ bash scripts/verify-fixture-library.sh >/dev/null 2>&1; echo $?
0
```

`35deab68` (2026-09-29 05:17 +0300) is the commit that added the synthetic
capability-test gate; its parent `ec4e9528` is green. So the pin audit has been
RED for every tree since that commit, and the blocker noticed tonight is
pre-existing, not introduced tonight.

## The defect

`pin_inventory_tests` holds a scanner's own input: a `r#"..."#` whose body is a
synthetic gate reproducing the forbidden root literal, handed to `pins_read` as
DATA so the capability survives the tree being fixed. Two scanners read it as
CODE:

- `verify-fixture-library.sh` invariant 1 — a line regex over the whole file;
- `scripts/pin-gate-audit.py` invariant 4 — worse, `fn_bodies` finds `fn` lines
  BY REGEX, so `fn a_gate_using_the_old_shape()` inside the raw string is
  attributed as a real gate and its body is classified.

This is the mirror of the class the same script already fixed for prose
("a doc comment reproducing a root literal invented a gate"). Comments are
stripped; embedded SOURCE was not.

## The rule

**A match inside a string literal that SPANS LINES is not a gate.** Rust has no
line-spanning non-raw string, so a literal containing a newline is embedded
source — a scanner test's input or a doc blob. A real pin read names its path in
a single-line literal.

Implemented once, in `pin-gate-audit.py`:

- `mask_embedded_source(text)` — blanks the interior of every line-spanning
  literal, preserving newlines so line numbers survive;
- `read_source(path)` — read + mask, applied at FILE level, because
  `fn_bodies`'s regex `fn` scan is what resurrects the raw string as a gate;
- `code_of(body)` — the existing comment-strip plus the mask.

`verify-fixture-library.sh` invariant 1 now IMPORTS `mask_embedded_source` from
`pin-gate-audit.py` (importlib, by path) instead of carrying its own copy: the
two checks must not disagree about what counts as a gate, or fixing one leaves
the other red.

Why a general rule and not a path allowlist: an allowlist is a second list of
places to remember and the next scanner test lands somewhere else. The property
that actually separates the cases is structural and checkable — a real pin read
names a path in a one-line literal; only test data wraps Rust source across
lines.

Why it cannot hide a real violation: a non-raw literal aborts at the first
newline, so it can only blank within one line, and a one-line literal holds no
Rust source. A char literal is only recognised when it really closes within
three characters, so a LIFETIME (`&'a str`) is not mistaken for a string start
and cannot swallow the rest of a line.

## Control 1 (positive) — a REAL violation still reds, and is NAMED

`crates/ec-av1/src/zz_control_probe.rs` (scratch, removed after):

```rust
#[test]
fn a_real_gate_reading_through_the_gitignored_root() {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/control-obu.obu");
    let _ = std::fs::read(p);
}
```

```
$ bash scripts/av1-suite-preflight.sh
  FAIL [invariant 1]: a committed pin is reached through the gitignored root
    crates/ec-av1/src/zz_control_probe.rs:3:let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/control-obu.obu");
  FAIL [invariant 4]: pin-reading gates whose pins are not committed
    crates/ec-av1/src/zz_control_probe.rs:2:a_real_gate_reading_through_the_gitignored_root  concat!  1/1 pin(s) not committed+tracked, e.g. control-obu.obu
pin_audit_exit=1 state=FAIL
VERDICT=BLOCKED
```

### Control 1b — the rustfmt-SPLIT real violation, the case the rule must not hide

```rust
#[test]
fn a_real_gate_reading_through_the_gitignored_root_split_over_lines() {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/control-obu.obu"
    );
    let _ = std::fs::read(p);
}
```

```
  FAIL [invariant 4]: pin-reading gates whose pins are not committed
    crates/ec-av1/src/zz_control_probe.rs:2:a_real_gate_reading_through_the_gitignored_root_split_over_lines  concat!  1/1 pin(s) not committed+tracked, e.g. control-obu.obu
pin_audit_exit=1 state=FAIL
```

Invariant 4 still catches it (its `ROOT_RE` spans newlines). Invariant 1 does
NOT, because that scan is per-line — pre-existing behaviour, unchanged here and
out of scope for this fix.

## Control 2 (negative) — the scanner's own test data does not red

Restored tree, no probe file:

```
$ bash scripts/av1-suite-preflight.sh
=== PIN-GATE PREFLIGHT (EC_REQUIRE_FIXTURES=0)
    pin gates: total=9 committed=22 uncommitted=0 ignored=0 assertless=0
pin_audit_exit=0 state=GREEN
=== VERDICT
VERDICT=GREEN
```

Before the fix the same command read `total=11 … uncommitted=4` with four BADROW /
BARDIR lines naming `gate_coverage.rs:1927 a_gate_using_the_old_shape` and
`gate_coverage.rs:1966 shapes`. After it, the two invented gates are gone and
`uncommitted` is 0 — the census now counts only real gates.

## `--self-test` green (it is invariant 4's own control)

```
$ python3 scripts/pin-gate-audit.py --self-test
SELFTEST	selftest_a_reads_a_pin	crate_pin
SELFTEST	selftest_b_reads_a_pin	crate_pin
SELFTEST	PASS	a doc comment between two gates left both counted as crate_pin
selftest_exit=0
```

Unchanged before and after, and the preflight runs it (it is the check at
`verify-fixture-library.sh:332`).

## The run required for acceptance

```
$ bash scripts/av1-suite-preflight.sh
pin_audit_exit=0 state=GREEN
VERDICT=GREEN
rungs_exit=0 ok_count=14 state=GREEN
```

`rung_check` is GREEN here because the oracle source is present on this box; on a
box without it the state is `not-run`, which is the separate expected state
Main named and is unaffected by this change.

## Not done

1. The audit's own `--self-test` still plants only the PROSE shape (a comment
   between two gates). The new rule's own positive control — a scanner test whose
   `r#"..."#` input must NOT be counted — is proven here by the live tree
   (`total=11 → 9`, `uncommitted=4 → 0`), not by a committed self-test case. A
   committed case would need the self-test to build a raw string inside its
   synthetic crate; worth adding, not added here.
2. Invariant 1 remains a per-LINE regex and so misses a rustfmt-split real
   violation (Control 1b). Pre-existing, not introduced or widened by this change.
3. `crates/ec-av1/src/gate_coverage.rs` was NOT modified — the charter allowed it
   "only if strictly needed", and the general rule made it unnecessary. The
   synthetic gate is left exactly as the capability test wants it.
