# lanes/av1pinselftest — the self-test now plants the shape that actually bit

Branch `lane-av1pinselftest`, base main `5da8b247` (the merge of
`lane-av1pinauditfix`). Scripts only.

This closes the one item `lanes/av1pinauditfix.report.md` declared not-done:
the audit's `--self-test` planted only the PROSE shape, so the new rule's
negative control rested on the live tree's 11→9 / 4→0 count change rather than
on a committed case.

## What the control now plants

A third synthetic gate whose body embeds a **raw string** containing a synthetic
gate that reproduces the forbidden root literal — the exact shape
`pin_inventory_tests` has in the real tree, and the shape that made the audit red:

```rust
#[test]
fn selftest_c_embeds_a_raw_string_input() {
    let synthetic = r#"
    #[test]
    fn selftest_phantom_inside_a_raw_string() {
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/phantom.obu");
        check(&fixtures);
    }
"#;
    let p = crate_pin("embedded-pin.obu");
    helper();
    assert!(!synthetic.is_empty() && !p.as_os_str().is_empty());
}
```

**The phantom carries the `selftest_` prefix on purpose.** `self_test` skips any
`fn` whose name does not start with `selftest_`, so a differently-named phantom
would be filtered out and the control would pass for the wrong reason — the
vacuity trap. With the prefix, a broken mask makes the phantom appear as a row
and the control fails.

Three assertions, all new:

1. `selftest_c_embeds_a_raw_string_input` is present and classified `crate_pin`
   (it is a real gate and must not be lost);
2. `selftest_phantom_inside_a_raw_string` is **absent** from the rows — `fn_bodies`
   must not resurrect it;
3. no row's pin names contain `phantom` — the name must not leak either.

## GREEN

```
$ python3 scripts/pin-gate-audit.py --self-test
SELFTEST	selftest_a_reads_a_pin	crate_pin	golden3-pin.obu
SELFTEST	selftest_b_reads_a_pin	crate_pin	sbpart-pin.obu
SELFTEST	selftest_c_embeds_a_raw_string_input	crate_pin	embedded-pin.obu
SELFTEST	PASS	a doc comment between two gates left both counted as crate_pin, and a raw-string input did not become a gate
selftest_exit=0
```

## RED — mutation: the mask disabled in `read_source`

```diff
-        return mask_embedded_source(fh.read())
+        return fh.read()  # MUTATED: mask disabled
```

```
$ python3 scripts/pin-gate-audit.py --self-test
SELFTEST	selftest_a_reads_a_pin	crate_pin	golden3-pin.obu
SELFTEST	selftest_b_reads_a_pin	crate_pin	sbpart-pin.obu
SELFTEST	selftest_c_embeds_a_raw_string_input	crate_pin	
SELFTEST	selftest_phantom_inside_a_raw_string	root-literal	embedded-pin.obu
SELFTEST	FAIL	phantom_gate_counted=[('selftest_phantom_inside_a_raw_string', 'root-literal', ['embedded-pin.obu'])] phantom_name_leaked=[] reclassified=[('selftest_phantom_inside_a_raw_string', 'root-literal', ['embedded-pin.obu'])] missing=[]
selftest_exit=1
```

The phantom is counted as a real `root-literal` gate — the original defect,
reproduced inside the control. A second symptom shows up in the same run:
`selftest_c_embeds_a_raw_string_input` now has an EMPTY name list, because
`fn_bodies` attributed the raw string's `fn` as the last `fn` and cut c's body
short. Restoring the mask restores both.

## The run

```
$ bash scripts/av1-suite-preflight.sh
    pin gates: total=9 committed=22 uncommitted=0 ignored=0 assertless=0
pin_audit_exit=0 state=GREEN
rungs_exit=0 ok_count=14 state=GREEN
VERDICT=GREEN
```

`total=9 uncommitted=0` is unchanged from `5da8b247` — the control adds no gate
to the census of the real tree; it runs entirely in the self-test's temp crate.

## Not done

1. **Invariant 1's per-line scan still misses a rustfmt-SPLIT real violation.**
   Pre-existing, unchanged here, and out of scope: invariant 4 catches that form.
2. The self-test still plants no case for invariant 1's own scan (the
   `verify-fixture-library.sh` positive control is a single-line literal). The
   new rule is shared by both, so a mask regression reds invariant 4's control;
   invariant 1's has no raw-string case of its own.
