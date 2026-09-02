# lane-frame80 r2 HANDOFF -- dedup verdict, two root causes, wall now at decode frame 170

Branch `lane-frame80`, worktree `edith_codecs-frame80`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-frame80`. Scratch `~/.cache/frame80-tmp`.
Every probe on this stream needs `EC_INTRA16X4_DECODE=1 EC_INTRA128_IN_INTER=1`.

## STEP 0 -- merge of lane-t900 `1bd6f815` (commit `c9e88f74`) + DEDUP VERDICT

Merge was a clean auto-merge (decode.rs + stream.rs), no conflicts.

**My r1 tx-context delegation is REDUNDANT -- dropped (commit `d79559ab`).**
Measured, not argued: with both fixes in, the 10 s head stops at decode frame 128 on
`EC_GOLOMB_LONG w=32 h=32 pos=366 base=15 length=21 rng=38664`; recompiled with my two
delegations forced to `if false`, the head stops at the SAME decode frame 128 with the SAME
golomb signature. So t900's per-call-site switch (`INTRA_IN_INTER_MODE.is_some()` ->
`tx_size_context_txfm_rect`) covers every case my thread-local `TX_SELECT_INTER` delegation did.
KEPT from r1: (a) the `EC_TRACE_COEFF_FRAME` rung, (b) `decode_key_frame_tile_with_cdfs`
clearing `TX_SELECT_INTER` (an independent thread-local leak -- ~15 other sites read that flag).
NOTE: the charter's dedup question ("does frame 62 mi(108,528) still read ctx=2, head still
reach 84") is stale twice over -- after the merge the head is far past both; the equivalent
check is the wall-signature comparison above.

Named tests after the merge, all green: `hidden_arf` 1/1, `rect64_corner_tus` 1/1,
`interintra` 4/4, `refusal_inventory` 3/3, `gate_coverage` 9/9. No suite this round.

## ROOT CAUSE (commit `7ab8915b`) -- sub-8x8 chroma inherited the WRONG sub-block's tx_type

`av1_get_tx_type` (blockd.h:1291) reads `xd->tx_type_map`, and `set_mi_offsets`
(av1_common_int.h:1680) / `decodeframe.c:1232` anchor that pointer at the mi of the block
*currently being decoded*. For a sub-8x8 group that block is the chroma-REFERENCE sub-block
(the last one, `is_chroma_reference`), so scaling the group's one 4x4 chroma unit back to luma
(0,0) lands on the LAST sub-block's first TU -- not the first sub-block's. Both sub-8x8 inter
readers passed `first_tx_type` (decode.rs ~23608 quad-4x4 reader, ~24586 4x8/8x4 rect reader);
now every sub-block overwrites, so the chroma-reference one wins.

Evidence: decode frame 98, frame-local coefficient-ladder element 7129, chroma TX_4X4
`eob=1` ours rng 59440 vs aomdec 52288 with an identical entry rng 43100 -- same value, wrong
CDF row (`eob_flag_cdf16[1][eob_multi_ctx]`, row 0 vs row 1) because the inherited class was 2D
instead of HORIZ/VERT. Head after the fix: decode frames 0..**169** (was 0..127).

New rungs (kept, both gated by `coeff_trace_on()`): `EC_INTERPLANE plane= side= inh= from=`,
and `from=<call site>` on the coefficient `eob` rung (`#[track_caller]` on `read_coeffs` and
`read_inter_plane`) -- that is what named the caller in one run.

## STATE / LADDER RESULTS (all on `~/.cache/frame80-tmp/t265.obu`, 265 frame-carrying OBUs,
## 170 decode frames; cut `t195.obu` sha256 a4567b39...698d8 was the 128-frame one)

- Coefficient ladder (`lad5.py <lo> <hi> a265.f oall265.f`, tags {all_zero,eob,after_bases}+rng,
  head-aligned by the frame's first 30 elements): decode frames 60..98 match ELEMENT-EXACTLY
  (frame 97 = 117451 elements, frame 98 = 79245). Frames 63..85 named in the charter are all in
  that clean range -- nothing left there.
- First divergence now: decode frame **99**, element 11933.

## NEXT DEFECT -- LOCALISED, DO NOT RE-BISECT (this is the exact next step)

Block **mi_row=40 mi_col=637** of decode frame 99: the SECOND sub-block of a vertical sub-8x8
group (two 4x8), mode GLOBALMV, ref0=5.

| | ours | aomdec |
|---|---|---|
| after mv | rng 60160 | rng 60160 |
| next symbol | `txfm_split ctx=18` -> 60039 | `motion_mode` (no bits) then **`interp`** -> 46640 |
| first coefficient TU entry | 60039 | 46545 |

We never read the block's interp-filter symbol. The first sub-block mi(40,636) DOES read it
(our `EC_IF ... rng=51838` == aomdec `EC_ISTEP2 name=interp rng=51838`), so the suppression is
mode-dependent: mi(40,637) is GLOBALMV and our sub-8x8 readers compute
`gm_nontrans = is_globalmv && gm_ref.model != Translation` (decode.rs:23526 and :24442) and feed
it to the `av1_is_interp_needed` suppression. libaom's `is_nontrans_global_mv`
(mvref_common.h) has an extra guard our two sub-8x8 sites lack:
`AOMMIN(mi_size_wide[bsize], mi_size_high[bsize]) < 2 -> return 0`, i.e. a block with a 4-px
side is NEVER "non-translational global" and always reads the interp filter. Check the same
`gm_nontrans` shape at decode.rs:20172, :21400, :25204, :25903 (sweep the class) -- only the
sub-8x8 sites (4-px side) can hit the missing guard, but grep each for the size term.
Repro: `~/.cache/frame80-tmp/oall99.g` line 1075947 vs `aall156.g` line 6037639
(cut `t156.obu`, 156 OBUs, both decoders MODE+MODE_STEP+COEFF, base/br filtered out).

## AFTER THAT (unchanged, none of it done this round)

Ladder frames 99..169 the same way; then the pixel compare of all shown frames vs ffmpeg
(census7/streamcmp.py); frame 57's tx_depth divergences (re-verify -- likely gone with t900's
fix); the witness fixture + gate; the full suite as a systemd unit.

## Artifacts (`~/.cache/frame80-tmp/`)

`lad4.py` (one frame vs the aom trace), `lad5.py <lo> <hi> <aom.f> <ours.f>` (frame sweep,
prints the first differing frame with 4 lines of context) -- both take PREFILTERED traces:
`grep -E '^EC_COEFF_STEP tag=(all_zero|eob|after_bases)|^EC_COEFF plane=|^EC_COEFF_FRAME'`
(a 4K 170-frame aomdec trace is 6.9M lines filtered, unusable unfiltered).
Cuts: `t156.obu` (100 frames), `t195.obu` (128), `t265.obu` (170).
