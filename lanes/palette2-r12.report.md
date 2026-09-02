# lane-palette2 r12 -- root cause of seed 46: a transform unit inside a block used the standalone-block reach tables

VERDICT: GREEN on the defect. Merge with main `4a29b4e` committed (`ed07bdc`),
seed-46 gate RED -> GREEN (`e8cc1c2`), full-suite totals below.

## STEP 1 -- merge main 4a29b4e (commit ed07bdc)
`git merge --no-commit --no-ff main` -> 5 conflicts, all in `decode.rs`
(`refusal_inventory.rs`/`stream.rs`/`cdf.rs` auto-merged clean).
All five are the same shape: main's sub8merge mi-granular neighbour maps
(`modes_above_left`, `record_mode_mi`, `record_uv_mode_mi`) against the lane's
palette reads/refusals/stamps in `decode_block_rect` and `decode_block_rect64`.
Resolution: BOTH sides at every hunk -- main's `modes_above_left(r, c)` replaces
the lane's `above_mode[c]`/`left_mode[r]` cell reads (that is exactly the
mi-granular fix), the lane's palette ctx/cache + refusals + neighbour stamps
kept, main's `EC_AV1_RECTX_TRACE` print kept.
`refusal_inventory` 3/3 and `gate_coverage` 9/9 green on the merged tree.

## STEP 2 -- seed 46 root cause (commit e8cc1c2)
Premise re-measured, not inherited: regenerated `s46.obu` from the gate recipe
(seed 46 gradients, sha256 `a109adab4397...`, 146 B, same hash as r11) and
re-ran the gate on the merged tree -- still RED, same 2242 samples.

r11 had ruled out entropy, the loop filter, palette and filter intra, and had
verified block-level `has_bottom_left` by hand. What it could not see is that
the block is NOT predicted as one 16x16: added an `EC_PRED` rung to the oracle
(`~/.cache/aom-oracle/src/av1/common/reconintra.c`, printing
`mi_row/mi_col/plane/row_off/col_off/txw/txh/mode/p_angle/have_*/n_top/n_left/n_tr/n_bl/bsize/partition`)
and aomdec reports the TL 16x16 as FOUR 8x8 luma TUs:

    EC_PRED mi_row=16 mi_col=32 plane=0 row_off=0 col_off=0 txw=8 txh=8 mode=7 p_angle=200 n_top=8 n_left=8 n_tr=-1 n_bl=8 bsize=6 part=6
    EC_PRED mi_row=16 mi_col=32 plane=0 row_off=0 col_off=2 ...                                                        n_bl=0
    EC_PRED mi_row=16 mi_col=32 plane=0 row_off=2 col_off=0 ...                                                        n_bl=8
    EC_PRED mi_row=16 mi_col=32 plane=0 row_off=2 col_off=2 ...                                                        n_bl=0

`n_bl=0` for both `col_off > 0` units: libaom's `has_bottom_left` returns 0
outright there. Our `decode_block` multi-TU branch built the unit's reach with
`Reach::of(logical_tx, tu_px, tu_py, ..)` -- the STANDALONE-BLOCK tables at the
unit's position (with `VERT_AB_PARTITION` set, `has_bl_vert_8x8` bit 1 = 1), so
the top-right 8x8 unit read 8 bottom-left samples libaom never gives it. That
is the 25-pixel diagonal wedge at rows 68..71 x136..143 (D203 = zone 3, reads
down-left), which then propagated across the last superblock.

FIX: `Reach::of_tu(bw, bh, col_off, row_off, tx, block_reach)` in `encode.rs`
(the TU branches of both libaom functions, one place), used at all three
per-TU sites:
- `decode.rs` `decode_block` multi-TU branch (the defect),
- `decode.rs` `decode_leaf8`'s 2x2 grid of 4x4 TUs (same shape, same fix),
- `decode.rs` `decode_rect_split` (had the rule inline since lane-rectsplit r1;
  now shares it -- one transcription, not three).
CLASS SWEEP: every other `Reach::of(...)` in `decode.rs` is a whole block or a
whole chroma block (4x4 leaf, 8x8 chroma group), not a unit inside one -- those
are correct and untouched.

## Files changed
- `crates/ec-av1/src/encode.rs` -- `Reach::of_tu` + `Debug` on `Reach`; new test
  `of_tu_matches_libaom_has_top_right_and_has_bottom_left_per_unit`.
- `crates/ec-av1/src/decode.rs` -- three per-TU reach sites through `of_tu`;
  merge resolution in `decode_block_rect`/`decode_block_rect64`.
- `crates/ec-av1/src/refusal_inventory.rs` -- merge (auto).

## Gates
- `a_real_aomenc_stream_with_filter_intra_on_a_horz_vert_strip_decodes_pixel_exact`
  (seed 46 is the regression pin): `cargo test -p ec-av1 --lib filter_intra_on_a_horz_vert_strip`
  -> `ok. 1 passed` (was FAILED with 2242 mismatching samples).
- `encode::tests::of_tu_matches_libaom_has_top_right_and_has_bottom_left_per_unit`:
  enumerates 10 block shapes x tx in {4,8,16,32} x every (row_off, col_off) x both
  block-level answers against an MI-unit transcription of libaom's
  `has_top_right`/`has_bottom_left` -> ok.

## EVIDENCE
EVIDENCE: scratchpad/aompred.log (EC_PRED rung added to reconintra.c) | `EC_PRED=1 aomdec s46.obu`, grep `mi_row=16 mi_col=32` | the 16x16 D203 block is four 8x8 luma TUs with n_bl = 8,0,8,0 -- the two `col_off>0` units get NO bottom-left
EVIDENCE: scratchpad/s46.obu (146 B, sha256 a109adab4397...) | regenerated from the gate recipe, `cargo test -p ec-av1 --lib filter_intra_on_a_horz_vert_strip` before/after the `of_tu` change | 2242 mismatching luma samples -> 0, test FAILED -> ok

## Full suite (merged tree, after the fix)
`systemd-run --user --unit=palette2-suite-r12-1788311147 ... cargo test -p ec-av1 --lib`
-> `$HOME/.cache/palette2-suite-r12.log`:
`test result: ok. 314 passed; 0 failed; 27 ignored; 0 measured; finished in 317.39s`
(r11 on the pre-merge tree: 306 passed / 1 failed / 26 ignored). Siblings named in
the charter -- both palette gates, superblock_level_ab, ab_partition,
split_transform_*, the tiny sweep, refusal_inventory, gate_coverage,
base_ctx_rect_offsets -- are all inside that run and all pass.

## Residue
- `deferred: a dedicated directional-mode AB-partition gate arm (D203/D157/D67 at SB
  column 0, --enable-ab-partitions=1, 8/10-bit) -- the seed-46 stream already pins the
  exact fixed path (VERT_A + D203 + split-TU at SB column 0) as a real-aomenc pixel gate
  and the new enumeration test covers the rule over its whole domain, so a second stream
  would add cost, not coverage -- unblocks by a round with budget for a new gate recipe`.
- `accepted: the EC_PRED rung now lives in ~/.cache/aom-oracle/src (oracle tree, not this
  repo); scripts/instrument-aom-oracle.sh does not yet install it`.
