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

---

# r3 appendix: fix the two structural census defects

r2 corrected the false CLAIMS. r3 fixes the LOGIC that generated them, so it
stops generating them. `crates/ec-av1/src/gate_coverage.rs` only; no gate,
counter, fixture, or decoder behaviour touched.

## Evidence table — before / after

| measure | before (r2, spelling-only) | after (r3, call-resolving) |
|---|---|---|
| gate bodies seen by `gate_bodies()` | 146 | **230** (+84) |
| of those, classified 10-bit-or-higher | 83 | **107** (+24) |
| `enable-dual-filter` 8-bit | listed as a hole | **retired** (gate found) |
| `enable-flip-idtx` 8-bit | listed as a hole | **retired** (gate classified both-depths) |
| `NEVER_EXERCISED_8BIT` | 3 live entries | **1** (`enable-rect-tx`) |
| `NEVER_EXERCISED_10BIT` | 3 live entries | **2** (`enable-dual-filter`, `enable-rect-tx`) |
| tests | 9 | **10** (new `the_detector_sees_the_gates_the_spelling_filter_missed`) |

The three gates the fix had to find, all now found and asserted by the new test:

| gate | was invisible because | now found by |
|---|---|---|
| `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact` | recipe lives in `inter_sb_none_gate` | `encoder_fns` call closure |
| `a_real_obmc_stream_reads_a_recorded_switchable_filter_for_every_neighbour` | same | `encoder_fns` call closure |
| `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact` | `ten_bit` recipe, `is_ten_bit` filed it 10-bit-only | `covers_both_depths` pair-of-formats marker |

## The two fixes

**1. `gate_bodies()` — recognise a gate by what it CALLS, not only what it spells.**
New `fn_units` parses every `fn` in `stream.rs` as `(name, body)`; `encoder_fns`
computes the transitive closure of fns that reach one naming `aomenc_path()`.
The filter is now `legacy_gate_tokens(body) || body.contains("aomenc_path()")
|| enc.iter().any(|e| calls_fn(body, e))`. The call test is deliberately the
weak half of an OR, so the fix is **strictly additive** — it can add gates but
can never stop recognising one the old filter did. `calls_fn` requires the name
to be followed by `(` and not be the tail of a longer identifier, so
`inter_sb_none_gate` does not "call" `sb_none_gate`.

**2. `covers_both_depths` — the third spelling.** Added
`body.contains("\"yuv420p\"") && body.contains("\"yuv420p10le\"")`: a body
naming BOTH fixture formats builds streams at both depths, whatever the
parameter is called. The quotes matter — a bare `yuv420p` substring test also
matches `yuv420p10le` and would fire on every 10-bit gate, retiring entries on
no evidence. This is not a special case for the flip-idtx gate; it is the
lane-defon/troykf blind spot closed on its third spelling.

## Red-before (three mutations, all reverted; `stream.rs` byte-identical to HEAD)

A detector change can only be shown to find MORE, never to stop missing, so
each half was shown to be **causal** — neuter it and the retired entry must come
back.

**Mutation A — the helper-delegation half.** Set `--enable-dual-filter=1` to
`=0` in both helper-based gates. TWO tests red:
- `never_exercised_8bit_matches_the_gate_recipes`: *"no 8-bit gate passes `=1`
  for ["enable-dual-filter"], so no real 8-bit stream exercises them"* — the
  retired entry correctly RETURNS as a hole.
- `the_detector_sees_the_gates_the_spelling_filter_missed`: *"seen, but its
  --enable-dual-filter=1 spelling is not visible to flags_in"*, `left: Some('0')
  right: Some('1')`. The second red is the valuable one: it separates
  *recognition broke* from *the gate no longer enables the tool*.

**Mutation B — the legacy path still works.** Set `--enable-dist-wtd-comp=1` to
`=0` in the r1 gate, a gate the OLD filter already recognised (it spells
`--passes=1`). Red at BOTH depths: *"no 10-bit gate passes `=1` for
["enable-dist-wtd-comp"]"* and the 8-bit twin. The spelling path is intact.

**Mutation C — the both-depths half.** Reverted only the pair-of-formats
clause. TWO tests red: `never_exercised_8bit...`: *"no 8-bit gate passes `=1`
for ["enable-flip-idtx"]"*, and the new detector test: *"the flip-idtx gate
spells both fixture formats, so covers_both_depths must credit it to the 8-bit
bucket as well"*.

All three reverted; `git diff --stat crates/ec-av1/src/stream.rs` is empty, and
all 10 tests are green.

## `enable-dual-filter` 10-bit: the entry STAYS — a finding, not an oversight

After the fix the 8-bit entry retires and the 10-bit one does not, which is the
honest outcome and exactly the case Main said to report rather than paper over.
The only `=1` witness is `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`,
and it calls `inter_sb_none_gate(NAME, false, ...)` — `false` is the `ten_bit`
parameter, so it builds an 8-bit stream only. Its `ten_bit=true` siblings
(`..._10bit_inter_sequence_with_a_whole_superblock_block_...` and the 8x8-leaf
pair) do not spell `--enable-dual-filter=1`. **To retire it: add the 10-bit arm
— pass `true` to `inter_sb_none_gate` in that gate and assert
`dual_filter_diff_hits()` still moves there.**

## Row: `enable-tx-size-search` — the one claim this audit could not settle

A third detector limit, and the method for whoever takes it next. After the r3
fix, 113 of the 230 selected gates name it: **88 pass `=0`, 15 pass `=1`, and 9
build the value into a `format!`/`String` variable** that neither `flags_in` nor
`settings_in` can see — both scan for a literal `"--flag=value"` inside one
segment. So the r2 finding stands and sharpens: the historical "all 49 pass 0"
claim no longer holds in either direction, and 9 gates are unclassified in an
**unknown direction**, which is the exact failure mode this file exists to
prevent.

**What would settle it (method, not a guess):** extend `flags_in`/`settings_in`
to resolve a local binding the way `gate_bodies` now resolves calls. When a
segment contains `let <name> = format!("--enable-tx-size-search={tx_search}")`
(or the `String::from`/`to_string` equivalent), record the TEMPLATE and bind the
variable's value at each use site. Re-count, and only then decide whether the
tool is a live hole. Until that exists the 113/88/15 split is a floor, not a
measurement. This is recorded in-file above `DEFAULT_ON_TOOLS` as well.
