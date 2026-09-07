# lane-lossless3 — AV1 lossless decode: the two screen-content residues

Branch `lane-lossless3`, base `c40432fc` (the lane-lossless2 merge). Fix commit
`6c9c36d3`.

Both residues lane-lossless2 deferred are ONE finding: the crf-5 screen KEY
point of `bd_rate_screen_native` and the 640x384 `tune-content=screen -crf 0
-g 3` inter desync are the same two defects. The gate reports only the FIRST
differing sample, which is why "frame 0 plane Y sample 625: ours 41, ffmpeg 40"
read like a ±1 prediction-edge rule; reproduced on the gate's own stream it is a
2.8-million-sample entropy desync in every one of the 12 frames (class
[[last-block-desync-reads-as-reconstruction]] inverted: a FIRST-sample report
hides the size of the desync behind it).

## Defect 1 — a skipped 8x8 leaf restored the previous block's luma level band
`decode_inter_block8` / `decode_leaf8` save plane 0 across the whole-block
`record_mi` write (`saved_luma_ctx`), because a var-tx split leaf has already
published its per-transform-unit levels and `record_mi` would flatten them.

- **First divergence**: 640x384 screen `-g 3`, `all_zero` symbol #26551, block
  mi (2,158): our `txb_skip_ctx` 3, aomdec's 1. `EC_ECDUMP` showed `left[2]=7`
  where the block at mi (2,156) had just published 0.
- **Root cause**: the save/restore is predicated on `split8` alone. A SKIP block
  publishes nothing per unit — and on a LOSSLESS frame `read_block_tx_size`
  returns 4x4 leaves for a skip block too (libaom `read_tx_size` returns TX_4X4
  before it looks at anything), so `split8` is true over an UNTOUCHED band. The
  restore then put the PREVIOUS block's levels back over the zeros `record_mi`
  had just written, where libaom runs `av1_reset_entropy_context`.
- **Class**: [[override-slot-on-one-arm]] — a save/restore is only valid on the
  arm that actually wrote the state it saves.
- **Fix**: `(split8 && !skip).then(..)` at both sites (the compound early-return
  arm and the shared fall-through/intra tail).
- **Sweep**: those are the only two `saved_luma_ctx` sites. The fix also closes
  the same latent shape on a NON-lossless intra 8x8 leaf whose `tx_depth` split
  and whose `skip` is set (`read_block_tx_size`'s `!is_inter && tx != side` arm
  returns leaves regardless of `skip`); the non-lossless INTER path was never
  exposed because its var-tx tree is gated on `is_inter && !skip`.

## Defect 2 — a rect sub-8x8 leaf read one TX_4X8 where lossless codes two TX_4X4
- **First divergence** (after defect 1 was fixed): `all_zero` #26912, a 4x8 leaf
  at mi (22,10): we read ONE rect unit with `txb_skip_ctx` 0 (the lone-TU rule),
  aomdec read a TX_4X4 with ctx 1.
- **Root cause**: these readers split only when the `tx_depth` symbol is
  nonzero, and a lossless frame codes no `tx_depth` symbol at all (`tx_select`
  is off, `tx_mode == ONLY_4X4`), so `split` stayed false and the leaf was read
  as one TX_4X8/TX_8X4.
- **Class**: the ledger's own `constraint|lane-lossless` — "the rect strip
  readers only call `depth_to_tx_wh` when the tx_depth symbol is nonzero, so
  forcing TX_4X4 also needs their `if depth != 0` guards widened". Five sites
  already carried `|| lossless(fctx)`; three did not.
- **Fix + sweep** (every remaining `depth != 0` split guard in `decode.rs`):
  `decode_intra_sub8_leaf` (`|| (lossless && bw != bh)` — a BLOCK_4X4 leaf must
  stay unsplit), `decode_leaf_rect8`, and the 16x4/4x16 rect4 reader's
  `if depth != 0` unit walk. The other `depth != 0` occurrences are hit
  counters and `EC_ISTEP` prints, not split decisions.

## Fixtures
- The inter gate `a_lossless_libaom_inter_frame_decodes_sample_exact` grows a
  seventh row: **640x384, 6 frames, `-g 3`, `tune-content=screen`, `-crf 0`** —
  the recipe that reproduces both defects. Before the fix frames 1/2/4/5 differ
  in 228k–333k samples (keys 0/3 exact); after, all six frames are exact.
- The gate's own crf-5 screen stream (OBS capture, `crop=1920:1024:320:208`,
  12 frames, `-cpu-used 6 -b:v 0 -crf 5`) decodes sample-exact in all 12 frames;
  with the fix reverted it is 2.80M/2.81M samples wrong per frame.

## Results
| gate | result |
|---|---|
| `a_lossless_libaom_inter_frame_decodes_sample_exact` (7 rows) | see below |
| `bd_rate_screen_native` (deciding) | see below |
| crate suite `-p ec-av1 --release --lib` | see below |
| `cargo check --workspace --all-targets -j4` | see below |

## Deferred
- `deferred: a frame mixing lossless and lossy segments — still refused by name; unchanged from lane-lossless2 (no libaom recipe found that emits one).`
