# lane-av1-llpred2: `skip_chroma_above` must read the frame's own ss pair, not a rect-over-square ratio

## Ticket

Reviewer FAIL on 71f07edd's `obmc_plan`: both ss axes were derived from ONE
square chroma side, `ss = trailing_zeros(write_w|write_h / chroma_side)`.
Two defects, one root:

1. 4:2:0, a 16x8 strip: `write_h/chroma_side = 8/8 = 1`, `trailing_zeros(1)`
   = 0, so ss=(1,0) and the plane block read (8,8) — `skip_chroma_above`
   went FALSE. The old pre-llpred code skipped, and libaom's
   `av1_skip_u4x4_pred_in_obmc` (reconinter.c 829) skips on the PLANE block
   `(8,4)` (frame ss (1,1)). Every 4:2:0 16x8/8x16 OBMC strip blended a
   chroma above pass the reference never blends (and would desync a
   stream that carries one).
2. 4:4:4, a 16x8 strip: `write_h/chroma_side = 8/16 = 0`,
   `trailing_zeros(0)` = 64, `write_h >> 64` panics in a debug build.
   The parent lane's byte-exact claim was measured in a release build,
   where the out-of-range shift wraps instead of panicking.

## Change (this tree @ 71f07edd + the fix)

`obmc_plan` takes `ss_x`/`ss_y` — the FRAME's own pair, exactly what the
chroma window half in `obmc_run` shifts by (`ss_x(fctx)`/`ss_y(fctx)`) —
and the decision moved to a pure helper:

```rust
pub(crate) fn obmc_skip_chroma_above(write_w, write_h, ss_x, ss_y) -> bool {
    matches!((write_w >> ss_x, write_h >> ss_y), (4, 4) | (8, 4) | (4, 8))
}
```

All three callsites pass the frame pair: `decode_inter_block`,
`decode_inter_block8` (`ss_x(fctx), ss_y(fctx)`), and the encoder's
`obmc_prediction` (`crate::decode::ss_x(fctx)`, 4:2:0-only encoder). No
ratio over `chroma_side` survives; `trailing_zeros` of anything is gone.
The window-size half that reduces to `/2` at 4:2:0 is untouched, and the
reserved 4:2:0 group-tail chroma arm is untouched.

## Proof (debug build, `dev` profile throughout)

- Decision check (new `obmc_skip_chroma_tests`):
  `rect_strips_skip_above_chroma_at_420_and_shift_never_overflows` pins
  4:2:0 (16,8,1,1) and (8,16,1,1) → SKIP (with (8,8,1,1), and
  (16,16)/(32,16)/(16,32) → blend), and 4:4:4 → only true 4-wide/high
  shapes skip, (16,8,0,0)/(8,16,0,0)/(8,8,0,0) → blend with no shift
  out of `usize` range. No hit-counter gate was added: the skip
  decision itself is what is proven.
- 6-frame 4:4:4 stream, DEBUG decode probe: fixture
  `/tmp/llintra8/testsrc2_min8.obu`
  sha256 `04fb6d38de5382e647bfb70b6a17802f1fccc5502e6698c0394167891f2b27dc`,
  `$HOME/.cache/cargo-target-av1llpred2/debug/examples/decode_probe`
  → 6 frames, 221184 bytes, `cmp`-equal to fresh
  `$HOME/.cache/aom-oracle/build/aomdec --rawvideo`. Byte-exact, now in
  a build that would have panicked on the old code.
- 4:2:0 non-regression: `cargo test -p ec-av1 --lib -- obmc_skip_chroma
  a_lossless` → 7 passed, 0 failed (the new check + the 4:2:0 lossless
  libaom gates, the 444 min-partition-64 inter gate, the sb128 pair, the
  16x4 pair).
- `cargo check -p ec-av1 --all-targets`
  (`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1llpred2`): 0 warnings,
  0 errors.
