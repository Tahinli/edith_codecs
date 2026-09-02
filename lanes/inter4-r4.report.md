# lane-inter4 r4 — OBMC on a 16x8/8x16 inter leaf (refusal lifted, gated)

Branch `lane-inter4`, worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-inter4`,
off r3's `96847a3`, with `lane-interbis` (`8a6bede`, `build_motion_field`'s compound
slot walk) merged FIRST as chartered.

## TASK 1 — r2's ignored 32x16/16x32 gate: re-measured, STILL RED, blocker moved

    cargo test -p ec-av1 --lib coded_rectangular_residual -- --ignored --nocapture

`a_real_aomenc_inter_sequence_with_a_coded_rectangular_residual_decodes_pixel_exact`
still cannot prove itself: over its 16 8-bit attempts, **12 named refusals** — 10 of
them `"an intra-coded HORZ/VERT strip needs rectangular intra prediction this decoder
does not code yet"` and 2 `"an inter 16x16-level AB or 1:4 partition ..."` — and the
other 4 attempts decode with **zero** rect inter residual (`tu=0`, `split=0`, 0
mismatches). So the 16-level leaf landing in r3 did NOT unblock it: what stops these
streams now is rectangular *intra* prediction, another lane's refusal. Kept `#[ignore]`,
with the reason string replaced by that measurement (stream.rs:4902).

EVIDENCE: $HOME/.cache/inter4-t1.log | 16 aomenc encodes, decode + counter read | 12 named refusals (10 intra-rect-strip, 2 inter AB/1:4), 4 attempts with tu=0/split=0, 0 mismatches

## TASK 2 — the OBMC residue: no defect left; refusal lifted

r3 refused `"OBMC on a 16x8/8x16 inter leaf (blend mismatches the reference on this
shape)"` off a measured 5302-luma-pixel mismatch (attempt 10, 8-bit, cq 22, frame 5).
Re-measured on THIS tree before touching any code:

- Rebuilt that exact stream from the gate's attempt-10 recipe with `--enable-obmc=1`
  (`scratchpad/o10.obu`, md5 `b643a93a8db45e2a1b89b2d777147ee3`, 828 B).
- Added an `EC_OBMC` rung to the oracle `aomdec`
  (`dec_build_prediction_by_above_pred` / `_left_pred`, printing the block, the
  neighbour's rel position, `op_mi_size`, mv, ref, bsize, filter) and the matching
  rung to our `obmc_blend`, then compared.

Both decoders emit **exactly 9 OBMC neighbour invocations**, in the same order, with
identical block shape, `off4`/`rel`, `span4`/`op_mi_size`, neighbour mv, ref frame and
filter — including the three on `wh=(8x16)` leaves. And every one of the six
`EC_AV1_PREFILT_DUMP` frames is **byte-identical** between our decoder and aomdec
(luma diff 0 on f0..f5). There is nothing left to fix on this shape: the mismatch r3
saw is gone. The only decode-path change between r3's measurement and this one is the
lane-interbis merge (temporal MV candidates); not bisected further.

EVIDENCE: scratchpad/{ours.obmc,ref.obmc,ours.yuv.f0-5,ref.yuv.f0-5} | EC_OBMC + EC_AV1_PREFILT_DUMP on o10.obu, ours (release decode_probe) vs instrumented aomdec | 9 vs 9 identical neighbour lines; per-frame luma diff 0/0/0/0/0/0

### What shipped

- `crates/ec-av1/src/decode.rs` (`obmc_blend`, ~12843): the `(16,8)|(8,16)` refusal
  deleted; in its place the new `OBMC_RECT_LEAF_HITS` counter (+ `obmc_rect_leaf_hits()`
  at ~1355) that the gate reads.
- `crates/ec-av1/src/refusal_inventory.rs`: the OBMC-rect-leaf string removed.
- `crates/ec-av1/src/stream.rs` (`..._with_a_16_level_rect_leaf_...`, ~5090): the sweep
  runs **32 attempts** now — the same 16 (2 sources x 2 quantisers x 2 tx-size-search
  arms x 2 motion steps) with `--enable-obmc=0`, then repeated with `--enable-obmc=1`
  as the LAST flag. New hard assert per bit depth: `obmc_leaf_proved > 0`.
- `crates/ec-av1/src/stream.rs` ~1948: the hidden-frames gate's `--enable-obmc=0`
  comment no longer cites a refusal that does not exist.

### Gate — GREEN

    cargo test -p ec-av1 --lib 16_level_rect_leaf -- --nocapture

- 8-bit: 15 named refusals, 9 pixel-exact leaf-carrying attempts (16x8=5, 8x16=4),
  whole-block rect TUs=4, **OBMC blends on a rect leaf=10**, 8 attempts carried none
  (0 mismatched).
- 10-bit: 4 named refusals, 8 pixel-exact leaf-carrying attempts (16x8=4, 8x16=6),
  **OBMC blends on a rect leaf=4**, 20 carried none (0 mismatched).

EVIDENCE: $HOME/.cache/inter4-g4.log | 64 aomenc encodes (32 attempts x 2 depths), every decoded frame Y/U/V vs ffmpeg | test result: ok. 1 passed; 0 failed; OBMC-on-rect-leaf blends 10 (8-bit) / 4 (10-bit), 0 mismatches

## Verifier findings folded in (r4, after the OBMC commit)

1. **`size_group_wh` was wrong for a 4-px side** (decode.rs ~9126): libaom
   `size_group_lookup[BLOCK_SIZES_ALL]` (`common_data.h:60`) is **0** for
   BLOCK_4X4/4X8/8X4/4X16/16X4, and our `0..=8 => 1` row gave those group 1 --
   the `y_mode` / `interintra` / `interintra_mode` CDF row of every sub-8 leaf.
   Fixed, and pinned by a new unit test `size_group_wh_matches_libaom_size_group_lookup`
   that enumerates all 22 `BLOCK_SIZES_ALL` entries against the transcribed table
   (class enumerate-the-table-domain).

       cargo test -p ec-av1 --lib size_group_wh_matches -- --nocapture

2. **rect var-tx SPLIT and the 10-bit coded rect TU are shipped unexercised.**
   Swept cq {8,12,16,22} x both source axes x both depths with tx-size-search on,
   reading `decode_probe`'s new `rect_inter: tu=.. txsplit=.. obmc_leaf=..` line:
   `txsplit=0` in every single decoding attempt, and every 10-bit rect leaf is
   `skip` (`tu=0`) at every cq that decodes at all. Below cq 22 the attempts stop
   at another lane's refusal -- mostly `"a HORZ/VERT intra strip below 16x16 with
   a split transform (per-unit rect prediction is not ported)"`, plus the sub-16
   `HORZ_A/HORZ_B/VERT_A` and 32-level 1:4 ones. Deviation from the verifier's
   ask, stated: I did NOT add an `#[ignore]`d pin -- an ignored test that runs
   nothing carries the same information as the measurement now written into the
   gate's own comment (stream.rs ~5097) and into the residue below, at less code.
   The asserts stay where the feature does fire (8-bit `tu_total > 0`).

3. `stream.rs` "aomenc keeps the last occurrence of a repeated `--enable-*` flag"
   is CORRECT (class aomenc-first-flag-wins is about `--enable-*` repeats resolving
   to the last one, and it is what the r4 obmc arm relies on) -- left unchanged.

4. `cdf.rs` doc slip fixed: `EOB_PT_128_CHROMA_CLASS1_Q0` had lost its doc comment
   to the `EOB_PT_128_LUMA_CLASS1_Q0` block added in r3; moved back.

EVIDENCE: sweep output in this round's transcript | 16 aomenc encodes (cq 8/12/16/22 x 2 axes x 8/10-bit) decoded through release decode_probe | txsplit=0 in all 7 decoding attempts, 10-bit tu=0 in all 4, 8-bit cq22 tu=1 txsplit=0 obmc_leaf=1

## Refusals

Lifted: `"OBMC on a 16x8/8x16 inter leaf (blend mismatches the reference on this shape)"`.
None added.

## Suite

    $HOME/.cache/inter4-suite-r4.log

See the tail of that log for the totals (systemd user unit, `cargo test -p ec-av1 --lib`).

## Film

`scratchpad/hg-head.obu` through the release `decode_probe` on this lane still stops at
`unsupported: AV1 tile (filter intra on a HORZ/VERT strip (this decoder predicts
square-only))` — an intra refusal owned by another lane, unchanged by this round.

EVIDENCE: scratchpad/hg-head.obu | release decode_probe under a 6G scope | REFUSED: filter intra on a HORZ/VERT strip

## Residue

- deferred: r2's 32x16/16x32 rect-residual gate — still `#[ignore]`d — unblocked by
  rectangular INTRA prediction (`an intra-coded HORZ/VERT strip ...`) landing, not by
  anything in this lane.
- accepted: which change fixed r3's OBMC mismatch is not bisected (interbis merge is the
  only candidate); the shape is now proven exact against an instrumented aomdec, so the
  question is archaeology, not a defect.
- deferred: the rect var-tx SPLIT path and a coded 10-bit rect TU are unexercised --
  unblocked by the sub-16 intra-rect-strip refusal landing (every lower-cq attempt
  stops there before a rect inter leaf can carry a split tree).
- accepted: `16x16 SPLITs=0` under this recipe (aomenc picks NONE/HORZ/VERT at 16);
  covered by the 8x8-leaf sibling gate.
