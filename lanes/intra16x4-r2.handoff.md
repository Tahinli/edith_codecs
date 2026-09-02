# lane-intra16x4 r2 HANDOFF

## Root cause (found, fixed, minimal)

`overlappable_above` / `overlappable_left` (`crates/ec-av1/src/decode.rs` ~18092 / ~18130)
never ported libaom's `mi_step == 1` chroma-pair merge from
`av1/common/obmc.h` (`foreach_overlappable_nb_above` / `foreach_overlappable_nb_left`).
Round 1's doc comment declared it unreachable ("no block this decoder codes is narrower
than 8px") -- the 1:4 partitions made it reachable: a 16x4 / 4x16 strip has
`mi_size_high` / `mi_size_wide` == 1, so libaom snaps the walk back to the pair's EVEN
index, reads the ODD (chroma-carrying) half as the neighbour and steps by 2.

Witness `~/.cache/intra16x4-tmp/s5.obu`, frame 1, block mi(0,44) (16x16, right of a
16x16-level HORZ_4 whose strip 0 is inter and strips 1..3 intra):

* our walk read mi(0,43) = the INTER strip 0 -> 1 overlappable neighbour ->
  `motion_mode_allowed` true -> we READ a `motion_mode` symbol;
* libaom's pair merge reads mi(1,43) = the INTRA strip 1 -> 0 overlappable neighbours ->
  `read_motion_mode` returns SIMPLE_TRANSLATION WITHOUT reading a symbol.

Class `symbol-consumption-gap`: rng 39440 -> ours 34918, aomdec 38204 (its
`EC_ISTEP2 name=motion_mode` print is unconditional, so an equal rng there means the
symbol was NOT read). The tile desynced at the very next coefficient: the first TU of
mi(0,44) read `all_zero=1` where libaom reads `all_zero=0, eob=59`, which is the
"prediction only, no residual" smooth block at px(176..178, 0..15) the r1 pixel
measurement saw as "first luma diff (175,0), 8400 px".

The fix is inside the two walk functions only -- every caller (eligibility at ~20908 and
~24493, the OBMC blend pass at ~18716/18746) routes through them.

## Proof

* Entropy ladder AFTER the fix, `s5.obu`, ours `EC_TRACE_COEFF` vs instrumented aomdec
  `EC_TRACE_COEFF`: 620/620 `tag=all_zero` symbols identical in value AND range, up to the
  point our decoder stops on another lane's refusal ("an intra 8x4/4x8 block inside an
  inter frame's sub-8x8 HORZ/VERT partition"). Before the fix the first divergence was the
  4th block of frame 1's 3rd superblock row.
* New green witness (r2 96-run sweep `~/.cache/intra16x4-tmp/sweep_r2.sh`):
  `bar_h` source (a horizontal bar sweeping DOWN), cq 63, `--min-partition-size=8`,
  192x128, 8-bit -> 5 intra 16x4 strips (2 chroma-reference), 6 frames decoded and
  `cmp` EXACT against `ffmpeg -i ... -f rawvideo` (`~/.cache/intra16x4-tmp/g_8.obu`,
  md5 `2eaafd9eb56fbf9d597820ad74c9c4e3`). The 10-bit build of the same recipe decodes
  6 frames but fires 0 strips (out of scope).

## Gate + refusal state

`a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact`
gained that recipe as attempt 4 (`RECIPES` is now 5-wide with a per-arm
`--min-partition-size`). Run once with `--include-ignored --nocapture`: **RED, and not on
this lane's shape** -- attempt 0 (`noise`, cq60, 128x128) mismatches from (122, 13),
5466 px on frame 1, which r1 already measured as 99 rows ABOVE that stream's first strip
(strips at px(80,112)/(80,120)); it panics before attempt 4 runs. The test is therefore
still `#[ignore]`d, with that measurement in its reason string.

**The refusal is NOT lifted**: `EC_INTRA16X4_DECODE=1` is still the bypass,
`refusal_inventory.rs` / `gate_coverage.rs` untouched.

## Next step

1. Own attempt 0's defect (128x128 cq60 `noise`, first diff (122,13) frame 1, arms
   [2,0,0]) -- ladder it the same way; it is a DIFFERENT shape and it is the only thing
   between this lane and a green gate.
2. Then reorder/prune `RECIPES` (or make the mismatch of an out-of-scope-for-this-shape
   arm non-fatal only with a named owner), un-ignore, lift the refusal, update
   `refusal_inventory.rs` + `gate_coverage.rs`.
3. Not run this round (turn cap): the full `cargo test -p ec-av1 --lib` suite and the film
   probes.
