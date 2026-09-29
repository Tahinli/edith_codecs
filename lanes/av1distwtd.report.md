# lane-av1distwtd

Two rounds on one branch (`lane-av1distwtd`, base `a21f3680`).

- **r1** — close the `enable-dist-wtd-comp` coverage hole.
- **r2** — audit every CLAIM in `gate_coverage.rs` against the code and correct
  the false ones (appended below).

---

## r1: `enable-dist-wtd-comp`

`PRODUCIBLE`. The census entry's stated reason — "distance-weighted compound is
unimplemented" — was **false**: `compound::dist_wtd_comp_weight_assign`
(lane-av1comp) and `mc::combine_compound`'s weighted path have implemented it for
a long time. What was missing was a stream *carrying* the tool, so no gate ever
proved it fired.

Closed with a witness gate `a_distance_weighted_compound_stream_decodes_pixel_exact`
(25 oracle frames, 21 distance-weighted blocks, pixel-exact) plus the
`gate_coverage.rs` entry deletion, commit `9cca1096`. See that commit message for
the full red-before evidence.

**12-bit / single-ref:** unaffected. 12-bit reaches the same arm independently
(15 blocks measured); single-ref never reads `compound_idx` at all.

---

# r2 appendix: gate_coverage.rs claim audit

Every claim checked against the code, not against prose. Method per claim:
find the deciding symbol and follow the call path from the feature-flag parse to
the reconstruction site; for encoder claims, read `~/.cache/aom-oracle/src`
(aom 3.13.3, `CONFIG_REALTIME_ONLY 0`) rather than `--help`; where a stream was
needed, encode one and read the **parsed** header, never the flag.

## Evidence table

| # | entry / claim (file:line) | stated reason | verdict | deciding code | what it should say |
|---|---|---|---|---|---|
| 1 | `enable-dist-wtd-comp` 8-bit (retired r1, commit `9cca1096`) | "unimplemented" | **FALSE** (r1) | `compound.rs:36 dist_wtd_comp_weight_assign`; `mc.rs:2075 combine_compound` | retired; combine long implemented, witness gate added |
| 2 | `enable-dist-wtd-comp` encoder default (`:5`) | "off in 11 gates" implies encoder default off | **FALSE** | `av1_cx_iface.c` `default_extra_cfg` = 1; `bitstream.c:2681` writes it | default is ON; the 11 gates pinned it `=0` themselves |
| 3 | `enable-dual-filter` 8-bit (`:270`) | "never spelled by any gate … no stream proven to carry per-direction interp filters" | **FALSE** | gate `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact` spells `=1` and asserts `dual_filter_diff_hits()` (`decode.rs:35135`); default parses `dual_filter=true` | EXERCISED; census can't see it (helper-delegation filter) |
| 4 | `enable-dual-filter` 10-bit (`:374`) | "hole at both depths" | **FALSE** | same gate as #3 | same as #3, both depths |
| 5 | `enable-flip-idtx` 8-bit (`:291`) | "never spelled" | **FALSE** | gate `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact` spells `=1`, asserts `tx_class1_hits` | EXERCISED at 8 bits (6 of 12 measured decodes are 8-bit) |
| 6 | `enable-flip-idtx` 10-bit numbers (`:366`) | "six 10-bit decodes", "10 rect TUs 1D" | **TRUE** (re-measured) | ran the gate: 12 decodes, 6 ten-bit, 219 rect TUs, 10 of them 1D | keep (now backed by an arrival assert) |
| 7 | `enable-rect-tx` 8-bit (`:320`) | "never spelled; reached only via partition shape" | **TRUE** | 0 gates spell it; aom has no header bit — `av1_cx_iface.c:2125/1483` is search-only, read only in `tx_search.c`; decoder gate is `is_rect_tx_allowed` | keep; now proven, not asserted |
| 8 | `enable-rect-tx` 10-bit (`:395`) | "hole at both depths" | **TRUE** | same | keep |
| 9 | `enable-cfl-intra` 8-bit comment (`:190`) | "gate passes `=1` at 8 AND 10 bits, pixel-compares both arms" | **TRUE** | gate `a_real_aomenc_sb128_stream_whose_skipped_cfl_and_1to4_chroma_pairs_...` loops `for depth in [8usize, 10]`, spells `=1`, asserts `dir_pairs > 0` | keep |
| 10 | same gate, name says "skipped_cfl" | — | **NOTE** | that gate asserts directional 1:4 pairs, **not** skipped-CfL; skipped-CfL evidence is a *different* gate (`a_real_film_key_frame_with_a_skipped_cfl_block_...`) | name overstates; comment doesn't repeat the error |
| 11 | `enable-intrabc` 10-bit comment (`:~336`) | "hard-asserts a decoded intrabc block per depth" | **TRUE** | `an_sb128_screen_stream_with_intrabc_...` asserts `blocks_total > 0 && fired_arms > 0` | keep |
| 12 | `enable-intrabc` NEVER_EXERCISED header (`:45`) | "counter asserted > 0" | **TRUE** | `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips` asserts `blocks_total > 0 && fired_arms > 0` | keep |
| 13 | `enable-restoration` 10-bit comment (`:311`) | "asserts a real Wiener/SGR unit fired" | **TRUE** | `a_real_aomenc_10bit_restoration_stream_...` asserts `lr_stripe0_hits`/`lr_last_stripe_hits` | keep |
| 14 | `enable-1to4-partitions` 10-bit comment (`:317`) | "10-bit arm asserts both orientations and coded strips" | **TRUE** | `a_real_aomenc_10bit_..._1to4...` gates assert `rect4_coeff_hits`/orientation counters | keep |
| 15 | `enable-global-motion` 8-bit comment (`:264`) | "loops `for depth in [8u32, 10u32]`" | **TRUE** | `a_real_affine_global_motion_stream_decodes_pixel_exact`; `covers_both_depths` recognises that exact spelling | keep |
| 16 | `DEFAULT_ON_TOOLS` doc (`:123`) | "all 49 gates that name it pass `--enable-tx-size-search=0`" | **STALE** | 113 of 146 name it: 88 `=0`, 15 `=1`, 9 via `format!` | claim no longer holds either way; count removed, not replaced |
| 17 | module doc (`:5`) | "a flag off the line takes aomenc's default (on …)" | **TRUE but misleading** | default is 1, but palette/intrabc are content-gated (`encoder.c:2079`) and `speed_features.c` re-masks per cpu-used | reworded: default = *allowed*, not *arrived* |
| 18 | module doc TILING (`:25`) | "13 gates … assert the parsed `tile_info`" | **STALE** | 19 gates spell nonzero tile log2; 9 assert in-body, 10 delegate to a helper that asserts | reworded to the measured split |
| 19 | `ALIASES` (`:194`) | "all three superres gates" | **STALE (undercount)** | 6 gates spell nonzero `--superres-mode`; 5 read `superres_hits`/`predict_scaled_hits` | count removed (rots); alias marked healthy |
| 20 | 10-bit list header (`:335`) | "Only 4 of the 45 real-aomenc gates encode at 10 bits" | **STALE** | 83 of 146 selected bodies are 10-bit-or-higher | reworded with the measured figure |
| 21 | `DEFAULT_ON_TOOLS` default column (`:135-181`) | `1` for tx-size-search / directional-intra / smooth-interintra / interintra-wedge / diff-wtd-comp / onesided-comp | **TRUE** | `default_extra_cfg` (non-RT array) is 1 for all six; `CONFIG_REALTIME_ONLY 0` | keep |
| 22 | `DEFAULT_ON_TOOLS` `deblocking`/`multi-tile`/`fwd-kf` defaults (`:181-189`), `multi-tile` `0`, `fwd-kf` `0` | — | **TRUE** | `LOOPFILTER_ALL` (non-zero), `tile_columns/rows = 0`, `fwd_kf = 0` | keep |
| 23 | NEVER_EXERCISED doc (`:46`) | "palette and intrabc only come on for screen content" | **TRUE** | `encoder.c:2079` `allow_screen_content_tools` from block counts; `seq_force_screen_content_tools` adaptive | keep |

## Claims-a-gate spot check (class: a passing gate that never reaches the feature)

Main's "one level down" class. Each entry naming a gate was checked against that
gate's own asserts:

| entry | gate named | gate exists | gate asserts the firing | verdict |
|---|---|---|---|---|
| dist-wtd (r1) | `a_distance_weighted_compound_stream_...` | yes | yes — `dist_wtd_comp_hits() > 0` | **VERIFIED** |
| dual-filter | `a_real_aomenc_dual_filter_obmc_8x8_...` | yes | yes — `dual_filter_diff_hits() > before` | **VERIFIED** (census blind to it) |
| flip-idtx | `a_real_aomenc_stream_with_a_1d_tx_class_...` | yes | yes — `compared_class1 > 0` | **VERIFIED** |
| cfl-intra | `a_real_aomenc_sb128_stream_whose_skipped_cfl_...` | yes | yes — `dir_pairs > 0` (not skipped-CfL) | **VERIFIED for 1:4 pairs; GAP for the name's skipped-CfL** |
| intrabc | `an_sb128_screen_stream_with_intrabc_...` | yes | yes — `blocks_total > 0 && fired_arms > 0` | **VERIFIED** |
| restoration | `a_real_aomenc_10bit_restoration_stream_...` | yes | yes — `lr_*_hits` | **VERIFIED** |
| superres (alias) | 5 superres gates | yes | yes — `superres_hits`/`predict_scaled_hits` | **VERIFIED** |
| global-motion | `a_real_affine_global_motion_...` | yes | yes — affine GM counter | **VERIFIED** |

**GAP (one):** no gate in the tree names `enable-rect-tx`, because the tool has no
header bit and no flag-shaped decoder gate — a *structural* gap, correctly
described by entry #7, not a broken gate.

## The two structural census defects this audit exposed

Both are stated in the corrected comments but are **census logic**, deliberately
NOT changed in this lane (the brief forbids behaviour changes; the entry deletions
are left in place so the file's own tests stay green):

1. **`gate_bodies()` helper-delegation blind spot** (`gate_coverage.rs:421`).
   The filter keeps only a `#[test]`-shaped segment containing `--passes=1` /
   `ten_bit_tool_gate(` / `encode_10bit_gradients`. A gate that hands its recipe
   to a shared helper keeps the flags in the *helper*, so the whole gate is
   invisible. Measured: 165 of 311 segments are filtered out, and 6 of those
   spell at least one `--enable-X=1` — including both gates that prove
   `enable-dual-filter` (#3, #4). This is why two live entries are wrong.

2. **`covers_both_depths` blind spot, third spelling** (`gate_coverage.rs:599`).
   It recognises only `if bit_depth == 10` / `if depth == 10` /
   `for depth in [8usize, 10]`. `a_real_aomenc_stream_with_a_1d_tx_class_...`
   builds both depths from a `ten_bit` recipe, so `is_ten_bit` sees the
   `yuv420p10le` and files it 10-bit-ONLY — hiding its 8-bit arm and keeping the
   8-bit `enable-flip-idtx` entry alive (#5). This is the lane-defon/troykf blind
   spot the function's own doc comment describes, reached through a new spelling.

**Loud, for the coverage matrix:** entries `enable-dual-filter` (8-bit `:270`,
10-bit `:374`) and `enable-flip-idtx` (8-bit `:291`) are **stale coverage holes**
— the tools are exercised by gates with hard arrival asserts. The census is
wrong, not the coverage. If another lane's matrix counts these as open cells,
that matrix overstates the holes. Correcting the two structural defects above
would let all three entries be deleted and this file's own test stay green.

## What changed in this lane

`crates/ec-av1/src/gate_coverage.rs` only, and the diff is **comment/reason-text
only**: verified — the only non-comment lines changed are 3 entry *reason
strings* (`gate_coverage.rs:272, 293, 376`). No flag name, entry membership,
counter, gate, or decoder behaviour was touched. All 9 `gate_coverage` tests
green after the change. No stream, gate, or counter behaviour altered, per brief.
