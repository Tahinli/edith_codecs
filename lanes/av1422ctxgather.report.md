# lane-av1422ctxgather — the 4:2:2 strip chroma context gather and the
# clobbering pair write fixed at source; 4:2:0/4:4:4 byte-identical; the
# 4:2:2 header refusal STAYS

## Outcome

1. **The clobber (the anchor's root cause).** `decode_inter_block`'s
   `pair_chroma` write hardcoded the 4:2:0 pair span `(pw, ph) = (16, 8)`
   for every horz strip. At 4:2:2 (ss 1,0) a horz 16x4 strip is its OWN
   chroma reference (`is_chroma_reference`'s `!(subsampling_y)` row
   clause) — no pair merge ever happened, `pair_mi` is the strip itself,
   and the two-row span overwrote one row it does not own. Measured
   (t422 frame 1, EC422WATCH probe, since removed): the inter strip at
   mi(8,4) zeroed `left[9][1]` under the mi(8,2) 8x8 leaf's level 5
   before the mi(9,4) strip's own U `all_zero` read it one ctx row low
   (ours base 0 + 7, oracle base 1 + 7 — the parent lane's anchor
   facts, reproduced). The span is now `(16, 4)` at ss (1,0); 4:2:0
   keeps `(16, 8)`; VERT keeps `(8, 16)` in every format.
2. **The inter strip's chroma gather.** `decode_inter_block`'s
   `around_c` gathered the horz strip's context with
   `around_mi_rect(pair_mi, 16, 8)` — the pair span AND the plain
   per-mi above sampling (one chroma 4-px column spans two luma mi
   columns at ss_x 1, so every column's whole-unit dc sign is counted
   twice). At ss (1,0) it now reads `(16, 4)` through
   [`Neighbours::around_mi_422_chroma`]; the 4:2:0 pair merge keeps the
   (16, 8) span and the per-mi gather, as do 4:4:4 and every VERT shape.
3. **The intra strip's chroma gather** (the charter's named site,
   `decode_rect4_16_strip` ~15331): the else arm's
   `around_mi_rect(pair_mi, pw, ph)` is routed through
   [`around_mi_422_chroma`] at ss (1,0) — the same measured rule as the
   other chroma gathers (10754/13774/15297/17862/18061). Shapes at other
   subsamplings keep the original gather.
4. **Class sweep found one more instance, fixed:**
   `read_inter_rect_chroma`'s chunked units (`multi` arm) gathered each
   unit with `around_mi_rect(cu_mi, span_x, span_y)`; at ss (1,0) the
   unit's above span covers `2*uw` luma mi columns and the per-mi gather
   double-counts every column. Now routed through [`around_mi_422_chroma`]
   at ss (1,0) only; the `else` (4:2:0/4:4:4) arm is the original
   expression verbatim.
5. Untouched, byte-identical: the intra strip's own chroma write (its
   pair is already `(16, 4)` at 4:2:2), `decode_rect4_16_intrabc`'s pair
   write (intrabc counters are 0 on the pinned fixture and intrabc has
   its own inventory discipline), the 4:2:0 group-tail chroma SKIP arm
   (reserved for its owning lane), `stream.rs`, and the 4:2:2 header
   refusal. The `EC_AV1_ALLOW_422_PROBE` bypass was applied to stream.rs
   for the probe runs and reverted; `git status` names only decode.rs.

## Verification

- **The anchor, before/after (probed, EC422WATCH + EC_DCDUMP):** before,
  `inter_pair` wrote `left[9][1] 5 -> 0` and the mi(9,4) strip's U gather
  read `left=[None/0]` -> base 0; after, nothing writes row 9 between the
  leaf's stamp and the gather, the gather reads `left=[Some(true)/5]` ->
  base 1 = the oracle's row. First U divergence at the parent's anchor
  (pre-all_zero rng 49672) is gone.
- **t422 inter ladder re-measured** (decode_probe + EC_AV1_{PREFILT,
  POSTDEBLOCK,POSTCDEF}_DUMP vs the parent's oracle dumps,
  /tmp/i422/ladder/oracle, our dumps /tmp/i422/av1422ctxgather/final):
  frame 0 is **exact at all three stages (0 diffs per plane per frame)**.
  Frame 1 pre-fix Y=23948 U=13513 V=13544 -> post-fix Y=1739 U=4346
  V=4974 (first Y diff moved from the anchor's ctx row to y=96 x=240;
  first chroma diffs at chroma (0,44)/(2,44)). Frames 2-15 improved on
  every plane (no frame regressed) but remain nonzero.
- **Frame exactness claimed ONLY where measured 0:** frame 0 at all
  stages. Frames 1+ are NOT claimed. The residual is downstream of this
  lane's class: frame 1's first Y diff sits at (96, 240) (mi(24,60)) and
  the first U/V diffs at chroma (2,44)/(0,44) — neither is a strip
  gather nor a pair write; attributing them is the next lane's first
  diff.
- **4:2:0/4:4:4 identity:** all four edits are gated
  `ss_x(fctx) == 1 && ss_y(fctx) == 0` with the non-4:2:2 arm the
  original expression verbatim; the lossless 4:4:4 stream gates stay
  green — `a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact`,
  `a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact`:
  3 passed, 0 failed.
- `cargo check -p ec-av1` (target dir `~/.cache/cargo-target-av1422ctxgather`):
  **0 warnings, 0 errors**.
- Probes: the temporary EC422WATCH write-watch was removed before the
  final build and commit; the final ladder was re-run on the clean tree
  and matches the probed run exactly (the watch was eprintln-only).

## State

- Commit on lane-av1422ctxgather (no push): the four gated edits in
  `crates/ec-av1/src/decode.rs` + this report.
- Next stop named for the owning lane: the frame-1 Y diff at mi(24,60)
  and the chroma diffs at chroma (0,44)/(2,44) — re-run the stage ladder
  and the EC_ISTEP/EC_COEFF pairing from there.
