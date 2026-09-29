# lane-av1dualfilter10 — 10-bit `enable-dual-filter` witness

Base: `1aaf7e0b` (lane-av1distwtd, the census detector fix). Branch: `lane-av1dualfilter10`.
Touches `crates/ec-av1/src/stream.rs` (tests only). `gate_coverage.rs` is NOT edited.

## What the census left open

`NEVER_EXERCISED_10BIT` still carried `enable-dual-filter`, and the reason it carried was
structural, not incidental:

- the only `=1` witness is `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
- that gate calls `inter_sb_none_gate(NAME, false, ...)` and that `false` IS the `ten_bit`
  parameter of the helper, so it builds an **8-bit** stream
- the helper's `ten_bit = true` siblings build 10-bit streams but spell no dual-filter flag

So the tool was proven at 8 bits and unproven at 10. This lane adds the missing arm.

## The change (stream.rs only)

1. `inter_sb_none_gate` now returns `Vec<u8>` — the encoded stream — so a caller can assert on
   the **parsed** sequence header rather than on the flag it asked for. SKIP paths return an
   empty vec. All six existing callers ignore the value; none needed edits.
2. New gate `a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
   (`stream.rs:17719`), the 10-bit twin of the 8-bit witness: same recipe, same four
   `--enable-*` flags including `--enable-dual-filter=1`, `ten_bit = true`, same compound-8x8
   arrival counter, same 16-frame sequence, same 64-wide frame.

### What makes it a witness rather than a spelling

Three independent facts, in this assert order:

1. **arrival, first** — `decode::dual_filter_diff_hits() > before_dual`, a DELTA taken around
   this arm alone. The counter increments only inside `resolve_interp_filter`
   (`decode.rs:35136`) and only when the header bit is set *and* the block's two directions
   differ. Deliberately asserted before the header checks so the mutation below reds on the
   arrival, not on a shape/pixel assert downstream.
2. **10-bit from the parsed header** — `seq.color_config.bit_depth == 10` read through
   `Av1Parser` on the encoded bytes. Not the filename, not the flag, not the fixture name.
3. **the tool is in the header** — `seq.enable_dual_filter` true, same parse.

`inter_sb_none_gate` also pixel-compares every frame against ffmpeg's own 10-bit decode
(`ffmpeg_decode_sequence_10bit`), so the arm is pixel-exact end to end, not merely parseable.

## Red-before (in the right direction)

Mutation: `--enable-dual-filter=1` -> `=0` in the 10-bit arm only.

```
panicked at crates/ec-av1/src/stream.rs:17753:
a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact:
no block read two DIFFERENT dual-filter directions in the 10-bit stream --
--enable-dual-filter=1 did not arrive or aomenc declined it
```

The **arrival** assert, as required. First attempt had the header asserts first and reds on
`the parsed sequence header says enable_dual_filter = false` — correct but the wrong assert,
so the block was reordered and the red re-proven. Reverted after; both arms green:

```
test stream::tests::a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact ... ok
test stream::tests::a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact ... ok
2 passed; 0 failed
```

Sibling `inter_sb_none_gate` callers re-run green (whole-superblock 8/10-bit, 8x8-leaf-split
8/10-bit, across-tile-columns: 3 passed, 0 failed).

## Census effect (measured, not claimed)

`cargo test -p ec-av1 --lib gate_coverage`, run on this branch:

```
the 10-bit list still names ["enable-dual-filter"], but a 10-bit gate now passes `=1` for
them -- delete those entries, the coverage hole is closed        <- FAILS, correctly

gate_coverage: 231 real-aomenc gates, 108 of them 10-bit
NEVER_EXERCISED_8BIT  (1 of 26):  --enable-rect-tx
NEVER_EXERCISED_10BIT (1 of 26):  --enable-rect-tx
NEVER_ON_8BIT (0 of 10, over 192 8BIT gates)
NEVER_ON_10BIT (0 of 10, over 110 10BIT gates)
```

That single red is the deliverable's receipt, and it is why this lane stops here:

**`gate_coverage.rs:374-377`, `NEVER_EXERCISED_10BIT` — `enable-dual-filter`** is now
retirable. Its reason string ("no 10-bit gate spells `=1` -- that gate calls
`inter_sb_none_gate` with `ten_bit=false`") is exactly what this lane closed. Deletion is the
census owner's call; the merge owner sequences it. `enable-rect-tx` stays in both lists
(its premise is PROVEN: aom has no sequence-header bit for it, `enable_rect_tx` is read only
in `tx_search.c`).

Gate counts moved 230 -> 231 and 107 -> 108 10-bit (the new arm), from lane-av1distwtd's
before-numbers of 146 -> 230 gates / 83 -> 107 at the detector fix.

## Third detector limit, cited for report agreement (NOT fixed here)

`--enable-tx-size-search` is built into a `format!`/`String` variable in a number of gates, so
`flags_in` — which matches the literal `"--enable-` — cannot see it. Any count of "gates
naming it" is therefore a FLOOR. lane-av1distwtd measured 113 of 230 naming it (88 `=0`,
15 `=1`, 9 invisible via `format!`); my independent recount on this branch (now 231 gates,
so not directly comparable) gives 116 segments spelling the literal (94 `=0`, 21 `=1`) plus
15 more that name the tool only inside a `format!`. Same class, same direction, magnitudes
differ only with the extra gate. Method: the method is the `format!` construction itself —
`args.push(format!("--enable-tx-size-search={}", ...))` leaves no `"--enable-` literal for
`match_indices` to find, so a census that wants a measurement rather than a floor has to
resolve the helper's `String` before counting, or match on the flag name without the leading
`"` and then classify the value from the format argument. Open question, not a fix.

## Verification actually run

- `cargo build -p ec-av1 --tests` clean.
- new gate green, both arms green, mutation red on the arrival assert, mutation reverted
  (grep confirms 4 `--enable-dual-filter=1` sites: 3926, 17668, 17736 — plus the two `=0`
  sites 18414/18680 untouched).
- sibling `inter_sb_none_gate` gates green.
- `gate_coverage` 10-bit test red for the intended reason; all other 9 green.
