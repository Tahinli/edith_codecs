# lane-hbdinter round 1 report

Branch `lane-hbdinter`, worktree `edith_codecs-hbdinter`, off main `3808cf8`.
Commit `d6b5912`.

## Premise re-measured (charter said: 10-bit stream mismatches at frame 2)

NOT reproduced as stated. Built the 10-bit counterpart of the 8-bit inter
gate (`a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact`'s
recipe + `--input-bit-depth=10 --bit-depth=10 --aq-mode=0 --deltaq-mode=0`):

- 64x64, 4 frames, cq 55: **all 4 frames pixel-exact** (first run).
- 128x64, 16 frames (the charter's geometry), cq 55: **all 16 exact** once
  `--min-partition-size=16` keeps aomenc out of the pre-existing
  "an inter partition below 16x16" refusal (which is a different lane).

So with that tool set 10-bit inter reconstruction was already exact. The
real defect only appears once **loop restoration** is enabled (cq-level=15,
`--enable-restoration=1`, the LR gate's own recipe), and it is NOT
inter-specific: the first mismatch is on **frame 0, the key frame**.
lane-hbd's 10-bit gate never saw it because its recipe leaves LR off.

EVIDENCE: /tmp/ec-av1-10bit-inter-gate-fail.obu | cargo test -p ec-av1 --lib a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact (compute_ab scaling reverted) | frame 0 V plane, sample 52: ours 289, ffmpeg 288

## What changed (all four verified line-for-line against ~/.cache/aom-oracle/src)

1. `crates/ec-av1/src/restoration.rs:~649` `compute_ab` -- box sums are now
   scaled back to the 8-bit range before `p` is formed
   (`a = Round2(A, 2*(bd-8))`, `b = Round2(B, bd-8)`, libaom
   `restoration.c:660-662`; `B[k]` keeps the RAW sum, as libaom does).
   **This is the site the gate attributes the mismatch to.**
2. `crates/ec-av1/src/restoration.rs:~545` `apply_wiener_stripe` -- the
   horizontal intermediate clamp was the fixed `[0, 8191]`
   (`WIENER_CLAMP_LIMIT(3, bd=8) - 1`, `convolve.h:43`) expressed in this
   crate's *unbiased* domain. Now `[-(1<<(bd+3)), (1<<(bd+5)) - 1 - (1<<(bd+3))]`,
   the libaom bound shifted by libaom's own `1 << (bd + 3)` bias (the
   `rounding` term of `highbd_convolve_add_src_horiz_hip`; the rest cancels
   against `WienerInfo`'s derived centre tap, as the existing doc comment
   derives). At 8-bit neither end is reachable -- that is why every 8-bit
   gate passed; at 10-bit the unbiased maximum is `128*1023 >> 3 == 16368`,
   so most Wiener intermediates were silently truncated at 8191.
3. `crates/ec-av1/src/mc.rs` `diffwtd_mask` -- round is
   `INTER_POST_ROUND + (bd - 8)` (libaom `reconinter.c:307`), was the
   constant 4.
4. `crates/ec-av1/src/warp.rs:450` `warp_affine` -- `const BD: i32 = 8`
   replaced by `decode::bit_depth()` in `offset_bits_horiz`,
   `offset_bits_vert` and the final `sum - (1<<(bd-1)) - (1<<bd)`
   (`av1_warp_affine_c`). Tip from lane-gmaffine, confirmed at the source.

Ruled out, at the source, no change needed: non-compound and compound
sub-pel MC rounding. libaom only moves `round_0`/`round_1` when
`bd + FILTER_BITS - round_0 + 2 > 16` (`convolve.h:83`), i.e. 12-bit only;
10-bit keeps 3/11 (single) and 3/7 (compound). `aom_highbd_blend_a64_d16_mask_c`'s
`round_bits` is likewise bit-depth-independent, so `blend_masked_compound`
is correct as written.

New counter: `mc::inter_pred_hits` (incremented in `predict_with_filters`,
`predict_scaled`, `predict_compound_intermediate`), re-exported as
`decode::inter_pred_hits`.

## Gate

`a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact` (crates/ec-av1/src/stream.rs).
Real aomenc, `yuv420p10le`, 128x64, 16 frames, `--lag-in-frames=0
--auto-alt-ref=0 --kf-max-dist=1000 --error-resilient=1 --aq-mode=0
--deltaq-mode=0 --enable-restoration=1 --enable-cdef=1 --cq-level=15`,
sequence header asserted `bit_depth == 10`, all 16 frames compared against
ffmpeg's own `yuv420p10le` decode, hard-asserting three firing counters
(inter predictions, deblock edges, a non-`RESTORE_NONE` LR filter). Decode
errors and mismatches panic; nothing turns into SKIP.

```
EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hbdinter \
  cargo test -p ec-av1 --lib -j3 a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact -- --nocapture
```

EVIDENCE: /tmp/claude-.../scratchpad/hbdinter-suite.log | the command above, with the fix | "inter_pred 759 deblock 2592 cdef_idx 0 wiener 1 sgrproj 3", test result: ok. 1 passed

## Test totals

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3`:
**268 passed, 0 failed, 23 ignored, 0 filtered out, 664.37s.**

EVIDENCE: scratchpad/hbdinter-suite.log | cargo test -p ec-av1 --lib -j3 | test result: ok. 268 passed; 0 failed; 23 ignored

## Refusals

None lifted this round -- these were silent wrong-pixel defects behind
already-permitted code paths, not refusals. `refusal_inventory.rs` and
`gate_coverage.rs` unchanged and green in the suite above.

## Film check

`cargo run -p ec-av1 --example decode_probe` on the 3s extracts already in
this worktree (`fixtures/hbd-r5/{hunger,troy}.obu`):

- Hunger Games: `REFUSED: unsupported: AV1 tile (a partition below 8x8 ...)`
- Troy: `REFUSED: unsupported: AV1 tile (a 32x32 partition type this decoder does not code (value=4))`

Unchanged from lane-hbd r5 -- both films still stop at partition-coverage
refusals that sit *before* any of the four fixed sites, so no frontier
movement is claimable from this lane. The fixes matter for what happens
after those two lanes land.

## Residue

- deferred: 10-bit gate for masked compound (`diffwtd_mask`) — needs a real
  aomenc `--enable-masked-comp=1` 10-bit stream that actually lands a
  DIFFWTD block; the 8-bit masked-compound gate exists, its 10-bit twin does
  not — unblocked by cloning that gate's recipe at `--bit-depth=10`.
- deferred: 10-bit gate for warp/global motion (`warp.rs` bd) — same shape;
  lane-gmaffine owns the warp gates, this fix should be re-verified there
  once its 10-bit stream exists.
- deferred: 10-bit gate proving the Wiener clamp bound specifically — the
  gate's stream fired Wiener once and the mismatch it catches is attributed
  to `compute_ab` (ablated separately); the clamp fix is source-verified
  against `convolve.h:43` but not attributed by a failing-then-passing
  pixel — unblocked by a fixture whose LR RD picks `RESTORE_WIENER` at
  10-bit (higher-detail content than `gradients`).
- accepted: `combine_compound` folds libaom's `>> DIST_PRECISION_BITS`
  then `Round2(.., round_bits)` into one `Round2(.., 8)`. Bit-depth
  independent, so out of this lane's scope, but the two are not
  algebraically identical for odd intermediates -- flagged for whoever owns
  compound.
