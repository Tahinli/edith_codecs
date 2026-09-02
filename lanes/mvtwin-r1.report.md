# lane-mvtwin r1 — phase-by-phase diff of the two MV-stack twins vs libaom

Base: lane-sb128 5ea5ec8, merged main 7a47fc1 (clean; the 128x128-superblock
refusal main re-added at b51719f stayed deleted — grep confirms).
Oracle: `~/.cache/aom-oracle/src/av1/common/mvref_common.c` (`setup_ref_mv_list`,
`av1_find_mv_refs`). Ours: `crates/ec-av1/src/mvstack.rs`
(`find_mv_stack_with_sign_bias` = single-ref twin, `find_mv_stack_compound` = compound twin).

## The table (libaom line = mvref_common.c)

| # | phase (libaom) | single-ref ours | compound ours | defect? |
|---|---|---|---|---|
| 1 | `row_adj`/`col_adj`, `find_valid_row/col_offset` reach (:502-533) | mvstack.rs:809 | mvstack.rs:1610 | **DEFECT 3** — both clamped the reach to the FRAME edge, libaom clamps to `tile->mi_row_start`/`mi_col_start` |
| 2 | `scan_row_mbmi` immediate, `len`/`weight`/`inc`/`processed_rows` (:141-190, :534-538) | `scan_row` :471 | `scan_row_compound` :1290 | exact (availability gate was `mi_row > 0`, folded into defect 3) |
| 3 | `scan_col_mbmi` (:192-236, :540-543) | `scan_col` :553 | `scan_col_compound` :1347 | exact |
| 4 | `has_top_right` + top-right `scan_blk_mbmi` (:264-312, :496, :544-548) | `scan_top_right` :645 | `scan_top_right_compound` :1403 | no change — see "has_tr" below |
| 5 | `nearest_match`, `REF_CAT_LEVEL` boost over `nearest_refmv_count` (:550-555) | :836 | :1600 | exact |
| 6 | `add_tpl_ref_mv` main loop, `step_h`/`step_w`/`blk_*_end` (:557-589) | :868 | :1625 | exact (compound reads the field once per side; `motion_field.rs:500` makes the lookup offset-independent, so both sides are Some/None together — libaom's single lookup) |
| 7 | `tpl_sample_pos` 3 extension samples + `check_sb_border` (:591-600, :316) | :898 | :1670 (added lane-sb128 r3) | exact — the twin drift r3 found, now symmetric |
| 8 | `GLOBALMV_OFFSET` / `zero_mv_ctx` far-or-missing test (:375-380, :406-412, :592) | :955 | :1750 | exact |
| 9 | corner `scan_blk_mbmi(-1,-1)` with `dummy_newmv_count`, weight `2*len`=4 (:604-607) | :967 | :1758 | exact |
| 10 | extended row/col scan `idx = 2..=MVREF_ROW_COLS` (:609-622) | :990 | :1770 | exact |
| 11 | `ref_match_count` + `mode_context` switch / `REFMV_OFFSET` (:624-651) | :1140 | :1858 | exact |
| 12 | the TWO bubble passes (nearest region, then the rest) (:653-682) | single stable `sort_by_key` :1050 | same :1802 | not a defect — every nearest entry carries `+REF_CAT_LEVEL` (640) and no post-boost entry can reach it (max post-boost weight seen is `len*weight` <= 80), so the two-phase ranking and one stable global sort agree |
| 13 | insertion cap `*refmv_count < MAX_REF_MV_STACK_SIZE` (:100, :128, :388, :425) | `add_candidate` :341 | `add_compound_candidate` :1268 | **DEFECT 2** — both pushed unbounded then sorted+truncated, keeping a different set than libaom's drop-at-insertion |
| 14 | `mi_size` = min(64x64 span, block span, frame edge) (:684-689) | :1060 | :1815 | exact |
| 15 | `process_compound_ref_mv_candidate`, sign-bias inversion, `can_rf > INTRA_FRAME` (:434-470, :693-706) | n/a | :1478 | exact |
| 16 | `comp_list` combine, slots past both lists = `gm_mv_candidates[idx]` (:708-720) | n/a | `combine_compound_candidates` :1520 | **DEFECT 1 (twin drift)** — filled `(0, 0)`; the single-ref twin closed the same gm-fallback gap in r6 |
| 17 | compound stack merge, `refmv_count==1` dedupe vs empty (:722-742) | n/a | :1845 | exact |
| 18 | `clamp_mv_ref` + `MV_BORDER`, every entry (:745-751, :768-772) | :1105 | :1878 | exact |
| 19 | single-ref extension gated per step on `MAX_MV_REF_CANDIDATES` (:754-766) | :1065 | n/a | exact |
| 20 | `mv_ref_list` tail filled with `gm_mv_candidates[0]`, unclamped (:775-782) | :1128 | n/a (libaom has no compound `mv_ref_list`) | exact |

### has_tr (phase 4), why no change
Ours probes "is the grid cell at `(mi_row-1, mi_col+bw4)` populated", which for a
decoder is exactly "was it decoded". Walking libaom's mask logic: for SQUARE blocks
the mask result and "already decoded" coincide at both sb64 and sb128; for the
rect case that differs (`bs > 16` at a 64x128 block) libaom's own
`is_last_vertical_rect` refinement forces `has_tr = 1` again, agreeing with our
probe. Porting the mask WITHOUT the three shape refinements (which need the
partition type, not passed to this module) would REGRESS the VERT-rect case.
Recorded as the same corner-cut already documented at `mvstack.rs:588`;
upgrade path = thread the partition type in with the rect inter leaf.

## Fixes (all in crates/ec-av1/src/mvstack.rs)
- D1 `combine_compound_candidates` (:1520 + call site :1859): unfilled slots take
  `gm_mv(gm, ref_frame.side)`, not `(0, 0)` (mvref_common.c:718-719).
- D2 `add_candidate` (:341) / `add_compound_candidate` (:1268): drop the 9th
  distinct candidate AT INSERTION (mvref_common.c:100/128/388/425); the dedup
  weight bump still applies on a full stack.
- D3 both twins (:809, :1610) + new `MiGrid::tile_origin` (:216): reach and
  availability are tile-relative (mvref_common.c:512-533).

## Checks
`cargo test -p ec-av1 --lib mvstack::` -> 32 passed, 0 failed. Three new
deterministic tests, one per defect:
- `a_missing_compound_combine_slot_takes_that_sides_global_mv_not_zero`
- `the_ninth_distinct_candidate_is_dropped_at_insertion_not_after_sorting`
- `the_row_reach_is_clamped_to_the_tile_origin_not_the_frame_edge` (644 -> 648 vs 652)

New real-aomenc gate `a_real_aomenc_compound_mv_stack_gate` (stream.rs), the first
compound gate here that does NOT pin `--enable-ref-frame-mvs=0`:
`--enable-ref-frame-mvs=1 --enable-order-hint=1 --auto-alt-ref=1 --lag-in-frames=25`,
cq sweep 45/55/35, 30 seeds/arm, hard-asserting BOTH `comp_mode_hits` and
`tmv_hits` advanced, every decode-order frame compared (Y/U/V) against ffmpeg,
decode errors only tolerated when the message contains "unsupported".
Arms: 8-bit sb64 gm=0 · 8-bit sb64 gm=1 · 10-bit sb128 gm=1.

EVIDENCE: $HOME/.cache/mvtwin-suite-r1.log | cargo test -p ec-av1 --lib compound_mv_stack -- --test-threads=1 --nocapture | 3 arms FIRING pixel-exact: seed 44 comp_mode_hits +7 tmv_hits +56; seed 80 comp_mode_hits +1 tmv_hits +56; seed 122 (10-bit, sb128) comp_mode_hits +8 tmv_hits +55
EVIDENCE: crates/ec-av1/src/mvstack.rs tests | cargo test -p ec-av1 --lib mvstack:: | 32 passed 0 failed, each of the 3 defects asserted on its own values

## Open residue
- fix-now-elsewhere (NOT this lane's surface): a real aomenc stream 128 px wide
  with `--tile-columns=1` MIS-DECODES frame 1 luma vs ffmpeg (seed 200, recipe in
  `a_real_aomenc_compound_mv_stack_across_two_tile_columns_decodes_pixel_exact`,
  `#[ignore]`d with the repro command in its doc). ABLATION-PROVEN pre-existing:
  identical failure with r1's tile-origin clamp forced back to `(0, 0)`. That arm
  is also the only one that reaches defect 3 on a real stream —
  `mvstack::tile_reach_clips` advanced to 60 there.
- deferred(a stream whose compound pair names a non-IDENTITY ROTZOOM/AFFINE
  reference) — `comp_gm_fill_hits` reads 0 on all three green arms, so D1 is
  proven by its unit test, not yet by a real stream. aomenc at gradients content
  keeps global motion IDENTITY even with `--enable-global-motion=1`.
- deferred(same) — `stack_full_drops` reads 0 on all three arms: no real block
  here produced 9 distinct candidates. D2 is unit-test-proven only.
- accepted — `has_tr` corner-cut (phase 4 above), ceiling and upgrade path named.
