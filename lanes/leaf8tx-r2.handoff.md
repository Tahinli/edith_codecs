# lane-leaf8tx r2 handoff (turn cap)

Branch `lane-leaf8tx`, tip `69e2933`.

## Merge state (done, committed)
- `48aab92` = merge of `lane-rectchroma2 48216c2` (square-buffer 4-tap MC kernel decision).
- parent commit = merge of `main 1176a16` (already contains lane-cdef `2d76270` and
  lane-golomb `173b793`; neither needed a separate merge).
- Conflict resolutions: refusal_inventory = UNION of both lists minus the lane's own
  OBMC string (main's "an OBMC neighbour whose switchable interp filter was never
  recorded" wins, matching main's decode.rs); decode.rs OBMC refusal = main's text, the
  lane's `ec_obmc_trace` lines kept; three rectchroma2 comment/`dump_stage_idx` hunks = HEAD.
  `cargo test -p ec-av1 --lib --no-run` builds clean.

## (2) sibling gate: DONE
`a_real_aomenc_stream_with_cdef_and_sub16_inter_leaves_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`) now continue-and-sweeps: the two per-attempt counter
asserts became `cdef_fired`/`leaf_fired` counters, every decoded attempt is pixel-compared,
and both counters are asserted once after the sweep (plus the existing `fired >= 2`).
NOT yet run — it is part of the still-owed full suite.

## (1) tx_depth split: REFUSAL KEPT, root cause localised to one symbol
The refusal is back in `decode.rs` (right after `read_block_tx_size` in the 8x8
intra-in-inter leaf) but is now bypassable: `EC_LEAF8TX_SPLIT=1`.

Measured this round (all under `systemd-run`, oracle `~/.cache/aom-oracle`):
- The merged rectchroma2 fix does NOT change the 10-bit residue: `EC_LEAF8TX_CONTROL=tx8`
  seed 68 cq19 still mismatches identically (frame 5 Y (160,96) got 664 want 670,
  1140/281/285 samples) -- `$HOME/.cache/leaf8tx-tx8-10bit-r2.log`.
- The 8-bit `tx8` arm is VACUOUS: 30/30 attempts refuse by name on other lanes' gaps
  (1:4 inter, rect residual, 128-root HORZ/VERT). Identical to r1's log, so r1's
  "8-bit is exact" claim for THIS arm is unsupported; the 8-bit evidence comes from the
  full-recipe arm only.
- Pinned stream: `~/.cache/leaf8tx-tmp/mk68.sh` -> `s68.obu`, md5 587b4f1d6fbc15249c0ddd0479d55fd7
  (hashed twice). Our decode: `EC_PROBE_OUT16=... decode_probe`; aomdec ref: `--rawvideo`.
- Pixel map (python diff of our.yuv vs ref.yuv): frames 0-4 EXACT; frame 5 differs in
  exactly 18 8x8 blocks, all inside the last 64x64 superblock -- (144,120),(152,120) plus
  the whole 32x32 quadrant x160..191 y96..127. The block at (144,120) is mi(30,36), which
  is a tx_depth=1 split leaf (`EC_TRACE_MODE_STEP` name=tx_depth val=1).
- Prediction is EXONERATED: our `EC_PRED` and aomdec's `EC_PRED/EC_PREDND` agree
  sample-for-sample at the leaf and at mi(24,40).
- ENTROPY LADDER (the decisive measurement): filtering both `EC_TRACE_COEFF` logs to the
  shared tags (all_zero/eob/base_eob/after_bases/base/sign/post_golomb) and diffing rng
  gives 454408 identical elements and the FIRST divergence at the split leaf's TU(0,0)
  luma `all_zero`: aomdec `plane=0 bc=0 br=0 ctx=6 rng=50428`, ours `side=4 ctx=6 rng=47740`,
  range-in 48136 identical, symbol VALUE identical (0). Same table (both TX_4X4 luma:
  1220 ctx-6 reads on each side, and our side/tx_size sequences agree for every all_zero
  in the stream), same ctx, same value -> the CDF ROW CONTENT differs.
  Ours at that read: `cdf=[32753, 32768, 32]` (saturated counter, P(sym0)=0.9995);
  aomdec's implied icdf[0] ~= 15640, i.e. P(sym0) ~= 0.52 -- a nearly unadapted row.

## Exact next step
Find why our `txb_skip_luma_4[6]` is far more adapted than libaom's at the 1217th read
while all 1216 earlier reads matched. Prime suspects, in order:
1. cross-frame CDF forwarding: `Cdfs::reset_counts` (cdf_state.rs:715 doc) is supposed to
   zero counters on save; check it is called for the slot frame 5 loads via
   `primary_ref_frame` (stream.rs:575-589) and that `disable_frame_end_update_cdf` frames
   save the STARTED-FROM state (`started_from`), not the adapted one.
2. our `update_cdf` rate vs libaom's for a count already at 32.
3. some read that updates row 6 in our decoder without libaom updating it (the row is only
   *read* 1216 times on both sides -- so an extra WRITE, not an extra read, is the shape).
Instrument: our `EC_COEFF_STEP tag=all_zero` now prints `side=` and `cdf=`; add the same
print on the libaom side ONLY in a private copy of the oracle (do not rebuild the shared
aomdec -- this round reverted its one-line edit unbuilt).

## Units / logs (r2)
- armed at handoff time: `systemctl --user status leaf8tx-tx8-r2` ->
  `$HOME/.cache/leaf8tx-tx8-r2.log` (both bit depths of the tx8 arm WITH
  `EC_LEAF8TX_SPLIT=1`; expected RED on 10-bit seed 68, that is the pin).
- `$HOME/.cache/leaf8tx-tx8-8bit-r2.log`, `leaf8tx-tx8-10bit-r2.log`, `leaf8tx-def-r2.log`
  (shipped recipe, 2 passed).
- scratch: `~/.cache/leaf8tx-tmp/` (mk68.sh, s68.obu, our.yuv, ref.yuv, our_coeff2.log,
  aom_coeff.log, aom_both.log, our_step.log).
- NOT RUN: the full suite. Owed by r3 -> `$HOME/.cache/leaf8tx-suite-r2.log`.

## Disposition
- deferred: the tx_depth-split lift -- one CDF-row adaptation divergence, localised to the
  element -- unblocked by suspect list above.
- fix-now for r3: run the full suite (the cdef/sub16 gate change is untested).
