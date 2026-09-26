# lane-av1422sqedge — the 4:2:2 square inter chroma MC row-MV axis fixed at
# source: every chroma MC site scaled the ROW motion component with the X
# subsampling (`mv_to_q4(cpy, mv.0, ss_x)`), so at ss (1,0) the vertical
# fetch position used half the row MV; the named frame-1 U (32,87) / V
# (32,80) residual is exact and chroma improved on every frame

## Outcome

1. **Reproduction at HEAD ce97ea17.** The residual_next_lane site reproduced
   exactly on the pinned t422 fixture (`/tmp/i422/t422_2f.obu`,
   sha256 d166ebf3...): frame 0 exact; frame 1 Y exact, U first diff
   (32,87) / V (32,80) with 3837 / 3879 diffs — byte-identical counts to the
   parent lane's report.
2. **Site correction (the parent lane's reading was wrong).** The named
   blocks are NOT "16x16→8x8→NONE leaves". The oracle's own partition trace
   (EC_PART, frame 1, tell=7910) reads `bsize=6 value=0` at mi(8,40): ONE
   16x16 PARTITION_NONE inter block spanning mi(8,40..43), whose 4:2:2
   chroma is ONE 8x16 MC call per plane at chroma (32,80) (oracle rung
   AOMIN48: `row=32 col=80 bw=8 bh=16 sx=0 sy=0`). Our decoder decoded the
   same block (partition value 5=HORZ_B-style check elsewhere; here both
   sides in sync, luma byte-exact) and made the same 8x16 chroma calls —
   with `y_q4=496` where the oracle fetches row 30 (`y_q4=480`).
3. **Root cause, class `chroma-mc-row-axis-ss`** (13 sites): the chroma
   MC's ROW component took `mv_to_q4(cpy, mv.0, ss_x(fctx))`. libaom scales
   each axis with its OWN subsampling (reconinter.h:139-141:
   `orig_pos_y += src_mv->row * (1 << (1 - ssy))`), so at 4:2:2 (ss_x=1,
   ss_y=0) the row shift was halved: mv=(-16,0) fetched chroma rows 31..46
   instead of 30..45 — ±1-class diffs hugging every reference value
   boundary (the "rightmost chroma column" pattern). 4:2:0/4:4:4 are
   value-identical (ss_x==ss_y) and provably untouched.
4. **Fix:** row axis now passes `ss_y(fctx)` at all 4:2:2-reachable chroma
   MC sites — `decode_inter_block`: single-ref U/V (unscaled 34398/34413,
   scaled 34445/34461), compound U/V mv0/mv1 (32920/32998, 32948/33026),
   the 1:4-strip sub8 piece's prev_mv call (34556); `decode_inter_block8`:
   compound U/V mv0/mv1 (39586/39633, 39601/39648). Sites left verbatim
   (already correct or unreachable at 4:2:2): the 422 sub8 piece arm
   (37085/37126 already `ss_y`), the 420-only group arm (37285, ss_x==ss_y
   there), the chroma_444 piece arms (36975/38484, ss 0/0), the OBMC
   neighbour predictor and both warp call families (per-axis args).

## Verification

- **Localization evidence** (oracle build `$HOME/.cache/aom-oracle/build`,
  env-gated rungs added to reconinter.c / reconinter_template.inc:
  `AOMIN48` inputs, `AOMOUT48` post-prediction dst, `AOMMV48` mv+anchor;
  all inert without `AOMIN48=1`): oracle `AOMMV48 row=32 col=80 bw=8 bh=16
  mv=(-16,0) sx=0 sy=0` vs our `OUR_SQEDGE`-rung capture cpx=80 cpy=32
  mv=(-16,0) — identical inputs; only our y_q4 differed (496 vs 480),
  pinning the defect to the axis argument, not the MV or position. The
  temp `EC_DBG_SQEDGE` rung in decode_inter_block was REMOVED before this
  commit (no trace remains).
- **t422 16-frame ladder** (`/tmp/i422/t422_16f.obu`, sha256 dda96300...,
  decode_probe + prefilt/postdeb/postcdef stage dumps vs the oracle's):
  * frame 0: EXACT at prefilt, postdeb AND postcdef (Y, U, V all 0);
  * LUMA: 0 diffs on frames 1–14 at prefilt, postdeb AND postcdef
    (f15 postcdef Y=5059 is the mi24 lane's pre-existing luma CDEF knock-on
    class: luma prefilt/postdeb f15 are 0 and this lane's edit only moves
    chroma fetch positions, which cannot alter luma filter decisions);
  * chroma improved on BOTH planes at EVERY frame vs the parent lane's
    tree (e.g. f1 U 3837→2231, V 3879→2197; f14 U 7381→5177,
    V 7545→5188); the named sites (rows 32..47, cols 80..87) are exact.
- **Frame exactness claimed ONLY where measured 0:** frame 0 all stages;
  frames 1–14 luma at prefilt/postdeb/postcdef. Frames 1–15 chroma are NOT
  claimed (reduced, not exact).
- **4:2:0/4:4:4 identity:** the lossless gates stay green —
  `a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact`,
  `a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact`
  (8 lossless gates green in total).
- `cargo check -p ec-av1` (target dir `~/.cache/cargo-target-av1422sqedge`):
  **0 warnings, 0 errors**. `git status` names only decode.rs + this
  report; the `EC_AV1_ALLOW_422_PROBE` stream.rs bypass was applied for the
  probe runs and REVERTED.

## State

- Commit on lane-av1-422sqedge (no push): the 13 `ss_x`→`ss_y` row-axis
  fixes in `crates/ec-av1/src/decode.rs` + this report.
- **The residual, site narrowed (next lane's first diff).** Frame-1 prefilt
  chroma now first diffs at U/V (row 88, col 104): the 8x8→HORZ pair of
  8x4 strips at mi(22,52) (luma rows 88..95, cols 208..215; chroma cols
  104..111). Oracle MC: one 4x4 chroma call per plane at chroma (88,104)
  for the FIRST strip (subpel 0, mv 0 — the prediction is a pure reference
  copy: 202,204,204,167) and NO strip-2 chroma MC call at (92,104). Ours
  makes the same (104,88) 4x4 call with the same inputs, but our written
  rows 88..91 (206,204,202,200) are NOT that call's output — the strip
  chroma write/anchor path (rect-strip chroma units at ss (1,0),
  `read_inter_rect_chroma` / pair-write territory) is the suspect, NOT MC
  input selection. `is_chroma_reference` (av1_common_int.h:1454) says BOTH
  8x4 strips are chroma references at ss (1,0); how the oracle fills rows
  92..95 needs pinning first (oracle rung window currently rows 84..100,
  cols 96..120).
