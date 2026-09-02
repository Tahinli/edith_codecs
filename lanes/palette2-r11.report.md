# lane-palette2 r11 -- merged main 096f4b8; the "regression" is a pre-existing defect the lifted refusal was hiding, localized to one block

VERDICT: AMBER. STEP 1 (merge) done and committed. STEP 2 (seed 46) NOT fixed:
root cause is *not* the lane's palette code -- the entropy stream of seed 46 is
bit-exact against libaom for every symbol of the frame, and the mismatch is a
pure reconstruction defect in the last superblock of a screen-content frame,
which main never decodes because it refuses that frame class outright. The gate
stays RED; the lane must not merge until it is green.

## STEP 1 -- merge main 096f4b8 (commit eb9472d)
`git merge --no-commit --no-ff main` -> one conflict only (`refusal_inventory.rs`);
`decode.rs`/`stream.rs`/`cdf.rs` auto-merged.
- `cdf.rs`: `git diff main -- crates/ec-av1/src/cdf.rs` = EMPTY, so main's NZ_MAP
  tables are byte-for-byte; main's deletion of
  `nz_map_ctx_offset_tables_match_the_rect_rule` taken (only
  `base_ctx_rect_offsets_match_the_transcribed_tables_over_the_whole_domain`
  remains, and it passes).
- `refusal_inventory.rs`: kept main's `"a superblock-level partition value
  outside PARTITION_NONE..PARTITION_VERT_4"` + the lane's two palette strings;
  dropped three strings no longer emitted by any `unsupported(` site (the
  superblock 1:4 partition, `LAST_FRAME (round 2)`, `palette block with a split
  luma transform (round 1)`) and main's screen-content strip string (the lane
  lifted it; only a comment mentions it now). `refusal_inventory` +
  `gate_coverage` green.
- `decode.rs` fix required by the merge (`decode_block_rect64`, main's SB-level
  1:4/HORZ-VERT path, ~decode.rs:5265): main's `RectStripModes` literal there
  lacked the lane's `palette_y`/`palette_uv` fields (compile error), and its
  split-tx early return had no palette neighbour stamp -- the SAME shape r10
  root-caused in `decode_block_rect`. Both applied, plus the
  `PALETTE_SPLIT_TX_HITS` bump, so the class is closed at both sites (`grep -n
  'return Ok(())'` in the rect decoders: 2 hits, both stamped).
- NOTE: main moved to `5d33392` (lane-sub8merge, mi-granular y/uv mode maps)
  WHILE this round ran. This lane is merged with 096f4b8, not 5d33392.
  `deferred: merge main 5d33392 -- appeared mid-round -- next round's STEP 1`.

## STEP 2 -- seed 46: what it actually is
Regenerated exactly (aomenc recipe copied from the gate, seed 46 gradients):
`s46.obu` 146 B, sha256 `a109adab439706035728f9348aba0801e06afa1f571540ce433a103a94377421`.

1. **The entropy stream is exact.** `EC_TRACE_MODE_STEP=1` ladders from ours and
   from the instrumented aomdec, aligned symbol by symbol (284 aomdec ISTEP
   lines vs our 270 -- the 14-line gap is only the 8 rect strips, whose
   `use_filter_intra`/`tx_depth` we print under `TRACE_RECT_*` instead):
   **every common (name, value, range) triple matches**, including the rect
   strips' own values read back from `TRACE_RECT_IMODE` (e.g. mi(8,40)
   tx_depth rng=50787 on both sides). So the lifted screen-content refusal did
   NOT introduce a desync: our rect palette syntax reads consume exactly what
   libaom consumes (8 strips, all `allow_screen_content_tools=1`, all
   `use_palette=0`).
2. **It is reconstruction, and it is not the loop filter.** `EC_AV1_PREFILT_DUMP`
   ours vs aomdec: 2165 luma samples already wrong before filtering (post-filter
   2242), bbox x136..191 y64..127 -- i.e. exactly the FRAME'S LAST SUPERBLOCK
   (128,64). Every other superblock, including the right-edge SB(128,0) and the
   bottom-row SB(0,64)/(64,64), is byte-exact.
3. **First wrong block pinned.** In decode order the first defective block is the
   TL 16x16 of a 32x32 `PARTITION_VERT_A` at px(128,64) (aomdec
   `EC_IMODE mi_row=16 mi_col=32 bsize=6`, mode 7 = D203_PRED, uv 7, skip=0,
   tx_depth=0). Its error is a 25-pixel diagonal wedge in rows 68..71,
   x136..143, deltas +7..-116; the two 16x16 above it and everything left of
   x128 are exact. The wedge then propagates: 16x32 strip at (144,64) 246/512,
   16x16 (160,64) 141/256, (176,64) 194/256, 32x16 (160,80) 512/512, 32x32
   (160,96) 1024/1024; the 32x32 at (128,96) is 0/1024.
4. Suspects RULED OUT this round: the `PALETTE_PRED` override wrapper and the
   per-TU palette window (no block in this stream uses a palette -- every
   `palette_y_mode`/`palette_uv_mode` symbol reads 0, so `set_palette_pred` is
   never called); filter intra (`use_filter_intra` = 0 on every strip here);
   any entropy-side cause (point 1); the loop filter (point 2). Availability was
   checked by hand against `has_bottom_left` in the oracle source
   (`~/.cache/aom-oracle/src/av1/common/reconintra.c`): for that TL block
   `blk_col_in_sb == 0`, so libaom takes the leftmost-column branch
   (`row_off_in_sb + 4 < 16` -> available) and never reaches the
   partition-keyed `has_bl_table`; our `Reach::bottom_left`'s `col == 0` branch
   returns the same, so the `VERT_AB_PARTITION` guard is NOT implicated for
   this block.
   `fix-now: r12 -- next step is to dump the TL block's prediction and residual
   separately (EC_DEBUG_EDGES + EC_TRACE_COEFF) against libaom's; the wedge
   shape with the entropy stream exact points at the inverse transform's
   tx_type/scan for a D203 16x16 in the corner superblock, or at the intra edge
   filter/upsample decision, not at availability.`

## Files changed
- `crates/ec-av1/src/decode.rs` -- `decode_block_rect64` split-tx arm: palette
  prediction fields + palette neighbour stamp at the early return.
- `crates/ec-av1/src/refusal_inventory.rs` -- merge resolution (list above).

## EVIDENCE
EVIDENCE: scratchpad/s46.obu (146 B, sha256 a109adab4397...) + aomstep.log/ourstep.log | `EC_TRACE_MODE_STEP=1` on instrumented aomdec and on `decode_probe`, aligned by (mi_row, mi_col, name) | every common triple identical; only the 14 rect-path lines we print under other names are missing -- entropy exact
EVIDENCE: scratchpad/ourpre.yuv.f0 vs aompre.yuv.f0 | `EC_AV1_PREFILT_DUMP` both decoders, same stream | 2165 luma samples differ, bbox x136..191 y64..127 (the last superblock only); post-filter 2242, so the loop filter is not the source
EVIDENCE: scratchpad (per-block diff map) | counted prefilter diffs inside every block aomdec reported for SB(128,64) | first defective block = TL 16x16 at px(128,64) of a 32x32 VERT_A, 25/256 wrong, wedge rows 68..71

## STEP 3 -- full suite on the merged tree
```
systemd-run --user --unit=palette2-suite-r11-1788310477 -p MemoryMax=10G --same-dir \
  bash -lc 'EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette2 \
            nice -n 10 cargo test -p ec-av1 --lib -j3 -- --test-threads=1 \
            > $HOME/.cache/palette2-suite-r11.log 2>&1'
```
-> **306 passed; 1 failed; 26 ignored** (735.05s). r10 was 294/2/27.
The ONE failure is this lane's known red gate,
`stream::tests::a_real_aomenc_stream_with_filter_intra_on_a_horz_vert_strip_decodes_pixel_exact`
(seed 46, message unchanged). r10's second failure,
`decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule`, is gone: main deleted that
stale test and the merge took the deletion. Both palette gates, `filter_intra_*`,
`split_transform_*`, `superblock_level_*`, the tiny sweep, `tx_select`, `refusal_inventory`,
`gate_coverage` and `base_ctx_rect_offsets_match_the_transcribed_tables_over_the_whole_domain`
all pass inside that run.

`fix-now: r12 -- the lane does not merge into main until seed 46 is pixel-exact (see STEP 2
for the localization; it is a reconstruction defect behind the refusal this lane lifted, not
an entropy or palette defect).`

EVIDENCE: $HOME/.cache/palette2-suite-r11.log | full `cargo test -p ec-av1 --lib -- --test-threads=1` under systemd unit palette2-suite-r11-1788310477 on the merged tree (b79b698) | test result: FAILED. 306 passed; 1 failed; 26 ignored; 735.05s -- the single failure is the seed-46 gate
