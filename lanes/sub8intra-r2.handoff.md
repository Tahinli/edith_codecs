# lane-sub8intra r2 HANDOFF (tip = this commit)

## The band: NOT a band -- the mv-stack's second-outer COLUMN scan (`mvstack.rs`)

r1's suspects (above/left txfm, inter, ref, skip, y_mode, coeff, side-mi bands published by
the intra leaf) were ALL innocent. The first diverging symbol on the r1 witness
`~/.cache/sub8intra-tmp/x_1t_48_8_128x128.obu` is the `refmv` symbol of the INTER 8x4 leaf at
mi(23,14) -- the block whose LEFT neighbour is the intra leaf at mi(23,12..13). Same decoded
value (NEARESTMV), different CDF ROW (class `wrong-alphabet-same-value` / `cdf-row-held-constant`):

* instrumented aomdec (private build, see below): `mode_ctx=75` -> newmv 3, globalmv 1, **refmv 4**
* ours before the fix: newmv 3, globalmv 1, **refmv 3** -> rng 62378 where the reference has 47820

`refmv 4` vs `3` is `ref_match_count == 2` vs `1` in `setup_ref_mv_list`'s `case 1:` switch
(mvref_common.c:638): libaom's second-outer column scan (`col_offset = -3`) found an inter
match at mi(23,11), ours found none.

## Two defects fixed (crates/ec-av1/src/mvstack.rs, all FOUR scan functions)

Both are pure sub-8x8 (a side < 2 mi) defects; every block main decodes today has both sides
>= 8px in at least one of the two axes checked, which is why they survived until this lane.

1. **`scan_col_mbmi`'s odd-row snap was missing** (`scan_col` L~600, `scan_col_compound` L~1406):
   libaom does `row_offset = 1; if ((mi_row & 0x01) && xd->height < n8_h_8) --row_offset;`.
   For an 8x4 leaf on an ODD mi row our walk probed the row BELOW (undecoded), losing the
   match. Fixed as `col_offset.unsigned_abs() > 1 && !(mi_row % 2 == 1 && bh4 < 2)`.
   Transposed twin fixed in `scan_row`/`scan_row_compound`
   (`(mi_col & 1) && xd->width < n8_w_8`).
2. **the weight / `processed_rows|cols` guard dropped libaom's `>= n8_*_8` half**:
   libaom `if (xd->height >= n8_h_8 && xd->height <= n4_h)`; ours was `if (bh4 <= n4)`, so a
   1-mi-tall query block set `processed_cols` and a boosted weight libaom never sets (which can
   also suppress the second-outer scan entirely). Now `bh4 >= 2 && bh4 <= n4` (and `bw4` twin).

`cargo check -p ec-av1`: clean.

## Ladder result on x_1t_48_8_128x128.obu (EC_SUB8INTRA_DECODE=1)

* mi(24,0) EC_MODE: ref 39238, ours **39238** (was 44268) -- matches.
* `tag=all_zero` ladder: **2106/2106 units identical in value AND range, all 6 frames**
  (`diff rz2.l oz2.l` empty). r1's ladder stopped matching at 1405.
* `OK: 6 frames decoded, 128x128`, `sub8_intra_rect: horz8x4=19 vert4x8=2 chroma_ref=10 mixed=3`.
* Pixels: 3 residual clusters (frame 3 two 4x4 luma, frames 4/5 one +-1 pixel triple) --
  root-caused to the OBMC `mi_step == 1` chroma-pair merge, which is **already fixed on
  lane-intra16x4 as f403337** (`overlappable_above`/`overlappable_left`). NOT duplicated here
  (charter: those functions belong to that lane). With f403337's decode.rs hunk applied on top of
  this commit the witness is **cmp-EXACT vs `ffmpeg -pix_fmt yuv420p` on all 6 frames**
  (verified, then reverted out of this tree).

## Oracle instrumentation added (additive, env-gated `EC_SUB8CTX`)

`av1/decoder/decodemv.c` in `~/.cache/aom-oracle/src`: one print after `read_ref_frames`
(single-ref p1 ctx + rng) and one after `read_inter_mode` (mode_ctx split into
newmv/globalmv/refmv + ref_mv_count). Built into the PRIVATE dir
`~/.cache/sub8intra-tmp/aom-build` (`ninja aomdec`); `~/.cache/aom-oracle/build` untouched.
This print is what named the band in one step -- keep it.

## Next step (r3)

1. Sweep `~/.cache/sub8intra-tmp/sweep3.sh` (max-partition-size=8, 1:4 off, 8- and 10-bit,
   cq 48/55/63, 128x128 + 192x128) -- 5 of 12 arms already decode all 6 frames pixel-EXACT
   (`10 cq48 128x128`, `10 cq48 192x128`, `10 cq55 128x128`, `10 cq63 128x128`,
   `8 cq55 192x128`). Those are the gate arms.
2. The other arms still stop at a refusal naming a shape the encoder cannot have coded
   (`--max-partition-size=8` + `--enable-1to4-partitions=0`) -- class `refusal-from-own-desync`,
   one desync left. Bisected on `g_8_48_128x128.obu`: first divergence at all_zero unit 1581,
   block mi(25,2) -- the reference reads `use_filter_intra=1`, `filter_intra_mode=3`,
   `tx_depth=1` for an INTRA sub-8x8 leaf there (ra2.l:4370-4372) which OUR path never reads;
   ours continues the previous block's 4x4 units instead. So: filter_intra IS read on a
   sub-8x8 intra leaf in that stream, and the `tx_depth=1` split (two TX_4X4) path is the
   one to check first.
3. Then: gate + refusal lift (both sub-8x8 intra refusals), refusal_inventory.rs +
   gate_coverage.rs, suite, films. Also still open: the flagged all-inter `first_tx_type`
   rule (blockd.h:1288, LAST sub-block vs first) -- unmeasured this round.
