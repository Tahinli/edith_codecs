# av1split: module split plan for crates/ec-av1

Lane: `av1splitplan` (analysis only, no code changed). Base: `main` @ 382b318b.
Source of the requirement: `DONE.md` (no source file over 5,000 lines before further AV1 work).
Line numbers below are as of base 382b318b. They shift after every step, so each step also
names its region by first and last item.

## 0. Size table (crates/ec-av1, all .rs under src/, examples/; no tests/ or benches/ dir exists)

| File | Lines | Over 5,000 |
|---|---:|---|
| src/decode.rs | 61,650 | yes |
| src/stream.rs | 57,178 | yes |
| src/encode.rs | 23,771 | yes |
| src/tile.rs | 11,516 | yes |
| src/qm.rs | 7,046 | yes |
| src/encoder.rs | 6,503 | yes |
| src/cdf.rs | 5,519 | yes |
| src/mvstack.rs | 3,820 | no |
| all other src/*.rs | < 3,600 | no |
| examples/*.rs (largest decode_probe.rs) | 532 | no |

Seven files need splitting: decode, stream, encode, tile, qm, encoder, cdf.

## 1. Cross-cutting constraints (apply to every step)

**1a. Source-scanning tests read the files by path.** These break when a file moves or
shrinks, so they must be repointed before any move:
- `src/gate_coverage.rs`: `include_str!("stream.rs")` at 548, 1076, 1993, 2488, 2583, 2663;
  and `("stream.rs", include_str!("stream.rs"))`, `("decode.rs", ...)`, `("encode.rs", ...)` at 1584-1586.
- `src/decode.rs` itself: `include_str!("decode.rs")` at 13697, 13796, 31180, 31850, 57769, 58221, 58912, 60134.
- `src/stack_budget.rs` `FILES` table, lines 445-~500: one `include_str!` per src file (the
  `every_by_value_module_is_scanned` test requires that new modules are listed).
- `refusal_inventory::pins_refusal(src, ..)` consumes the text returned by those includes.
Total: about 70 `include_str!` sites in gate_coverage.rs, stack_budget.rs and decode.rs.

Fix (Step P0, before any move): add one test helper that concatenates every `*.rs` under a
module directory (`env!("CARGO_MANIFEST_DIR")/src/<name>/` plus `<name>.rs` if it exists) and
replace each `include_str!("X.rs")` with that helper's output. The helper must cover the
new `decode/`, `stream/`, `encode/`, `tile/`, `qm/`, `cdf/`, `encoder/` directories. The same
step adds the new files to `stack_budget::FILES`. Check the 5 probes that use
`ec_av1::decode::` / `ec_av1::encode::` (examples/*.rs, tools/ec-bench, crates/ec-hw/tests/gpu.rs):
public paths must not change.

**1b. File-to-directory rename.** `decode.rs` -> `decode/mod.rs` (same for stream, encode, tile, qm,
encoder, cdf). `lib.rs` keeps `pub mod decode;`, so paths stay the same.

**1c. Inline `mod x { ... }` -> `mod x;` with a file.** Child modules see the parent's private
items, so moving a test module to a file needs no visibility change. Do this first; it is the
cheapest large move.

**1d. Production code moved to a sibling module.** Every item it uses from another new sibling
must go from private to `pub(super)`. Every item that was `pub` must be re-exported with
`pub use <child>::*;` (or an explicit path) in `mod.rs`, so `crate::decode::X` and
`ec_av1::decode::X` keep working. Items are listed under section 4.

**1e. Imports.** Each new module needs `use super::*;` (or the explicit names) plus
`use crate::hits::{hit, hit_do};` where it uses the hit macros.

**1f. Macros.** `decode.rs` has no top-level `macro_rules!`. Its three `istep!` definitions
(12148, 20844, 26638) are function-local, so they need no ordering or `#[macro_use]`. `hit!` and
`hit_do!` come from `crate::hits` (`#[macro_export]`-free path import). Nothing in ec-av1 needs
`#[macro_use]`. stream.rs and encode.rs have no top-level macros either.

**1g. Order of moves.** Within a file, extract regions from the bottom up (highest line number
first). Then the line ranges of regions not yet moved stay valid. Do every step with the file
compiling: `cargo check -p ec-av1` plus the source-scan tests.

## 2. decode.rs (61,650 lines)

### 2a. Top-level map (approximate end = next item start - 1)

| Lines | Region | Notes |
|---|---|---|
| 1-~357 | imports, FrameCtx (216-~222 impls), core structs | |
| ~357-8492 | partition/seg/palette/recon helpers, read_coeffs fns | 3,624 lines are hit-counter fns (section 2c) |
| 8493-9129 | `read_coeffs`, `read_coeffs_rect` | |
| 9241-11564 | `struct Neighbours` and `impl Neighbours` (2,072 lines) | one impl, private state |
| 11565-12083 | filter-intra, palette index map helpers | |
| 12084-16934 | `read_intra_mode_rect`, `decode_rect_split` (1,080), `decode_intra_rect_in_inter`, `decode_intrabc_rect`, `decode_intrabc_128rect`, `decode_block_rect`, `decode_leaf_rect` | 4,850 lines, tight |
| 16935-20788 | `decode_block_rect4` (788), `decode_rect4_16_intrabc`, `decode_intrabc_owned_rect`, `decode_rect4_16_strip` (989), `decode_block_rect64` (703) | |
| 20789-24729 | `read_intra_mode`, `read_angle_delta`, CFL consts/fns, `impl PlaneBuf` (378), `read_plane`, `decode_block` (1,448) | |
| 24730-26796 | `decode_block_128rect` (491), `decode_leaf8` (1,257), `read_intra_mode_sub8` | |
| 26797-29818 | `sub8_leaf_chroma444` (689), `sub8_leaf_chroma422` (447), `decode_leaf_split4` (689), `decode_leaf_rect8` (1,158) | |
| 29819-31336 | CDEF consts and fns (`CDEF_DIRECTIONS`, `cdef_find_dir`, `cdef_adjust_strength`, `cdef_filter_block*`), `mod cdef_simd` (30215-30469, inline) | |
| 31337-33053 | `read_block_tx_size` (204), `read_inter_rect_chroma` (208), `INTER_BLOCK_SHAPES`, `rect_inter_residual_supported`, `edge_params_body` | |
| 33054-34521 | `deblock_thresholds`, `sclamp`, `round_pow2`, `pooled`, `mod deblock_simd` (33383-33827, inline, 445 lines) | |
| 34522-36564 | `replay_filters`, `replay_final`, `replay_inner` (257), `cdef_band` (212) | |
| 36565-39260 | `decode_key_frame_tile_lr`, `decode_key_frame_tiles_lr`, `decode_key_frame_tile_with_cdfs` (2,696) | |
| 39261-42377 | `switchable_interp_ctx`, `resolve_interp_filter`, `intra_inter_ctx`, `ref_chroma_shape`, `ref_planes`, `overlappable_above`, `obmc_run` (41605-41815), `read_compound_ref_frames`, `read_inter_compound_mode`, `assign_compound_mv`, `build_gm_mv_table` | 3,116 lines |
| 41528-~41600 | `#[cfg(test)] mod obmc_skip_chroma_tests` (inline) | |
| 42378-48099 | `fn decode_inter_block` (5,722 lines, one function) | **over 5k as a single function**, see 2d |
| 48100-51108 | `decode_inter_sub8_split4` (994), `decode_intra_sub8_leaf` (874), `decode_inter_sub8_rect2` (1,141) | 3,009 lines |
| 51109-54360 | `grid_stamp_rect`, `decode_inter_block8` (2,935) | |
| 54361-57728 | `decode_inter_frame_tile`, `decode_inter_frame_tile_lr`, `decode_inter_frame_tiles_lr`, `decode_inter_frame_tile_with_cdfs` | 3,368 lines |
| 57728-57729+ | `#[cfg(test)] mod tests` (57729-61558, 3,830 lines), `#[cfg(test)] mod tx4x8_table_tests` (61559-61650) | |

### 2b. Test modules
- `mod tests` 57729-61558 (3,830 lines, `use super::*`) -> `decode/tests.rs`, via `#[cfg(test)] mod tests;` (rule 1c). Under 5k, so no further split.
- `mod tx4x8_table_tests` 61559-61650 -> `decode/tx4x8_table_tests.rs`, or fold into tests.rs.
- `mod obmc_skip_chroma_tests` 41528-~41600 -> `decode/obmc_skip_chroma_tests.rs`.
- Scattered `#[test]` fns inside production regions (13661-14548 block, 7999, 8244, 13753, 34356-34380). Leave them with their region. They are small.
- Test helpers at column 0 (`#[cfg(test)] fn` at 7999, 8244, 13753) stay with their region.

### 2c. Hit-counter / census functions (cross-cutting)
- 318 top-level items whose names match `hits|counter|census|_hit|reset_` (about 4,255 lines). They are
  spread through 1-8492 (3,624), 20789-24729 (257), 29819-34521 (97+156) and 41605-42377 (82).
- They are instrumentation (`hit!` calls). Move them all to `decode/counters.rs` by name, and set
  `pub(super)` (they are called from production modules). The `mod.rs` re-exports them with `pub use counters::*;`.
- Also: `tile.rs` has the same pattern (`COMPOUND_*_HITS` statics and `take_*_hits` fns, 1720-1762).
  Move those into a shared module when the tile split runs.

### 2d. Blocker: `decode_inter_block` is 5,722 lines in one function
A file cap does not help if one function is over 5k. Before its file can move, the body must be
split into helper functions, with the phase blocks (mode read, MV/ref setup, prediction, residual,
recon) pulled out one by one. This is a behaviour-preserving refactor. Gate it with the decode
pins: `ec-av1` tests (decode pixel-exact tests and `gate_coverage`). Plan steps D9a/D9b below.

## 3. stream.rs (57,178 lines)

- Production 1-2328 (about 2.3k): `decode_stream`, `decode_stream_with`, `decode_stream_with_threads`
  (649-1383), `SeqFlags`, `RefSnapshot`, `FrameOutput`, `decode_frame` (1546-2307), `Reorder`, `SlotPromise`, plus
  about 85 counter fns (71-390, 2308-2328). No split needed: `stream.rs` -> `stream/mod.rs` only (optionally move the counters).
- `#[cfg(test)] pub(crate) mod tests` 2329-57178 (54,849 lines, 397 `#[test]` fns plus about 140
  helper fns / statics / structs).

### 3a. Test chunks (cut at `    #[test]` boundaries, all under 5k)
| Chunk | Lines | Anchor (first item) |
|---|---|---|
| T1 | 2329-6884 | `pub(crate) mod tests`, `test_card` (2334) |
| T2 | 6885-11454 | `decode_stream_matches_the_tile_path_on_a_key_frame` |
| T3 | 11455-16256 | `a_420_lossless_alt_ref_1to4_strip_witness...` |
| T4 | 16257-20952 | `a_16x4_intrabc_pair_strip_decodes_pixel_exact` |
| T5 | 20953-25719 | `a_real_aomenc_10bit_intra_tools_stream_decodes_pixel_exact` |
| T6 | 25720-30250 | `a_real_aomenc_inter_sequence_with_32x32_level_1to4_strips...` |
| T7 | 30251-34861 | `a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly` |
| T8 | 34862-39404 | `a_real_aomenc_10bit_stream_with_a_32_level_ab_partition...` |
| T9 | 39405-44038 | `real_aomenc_film_grain_streams_decode_pixel_exact` |
| T10 | 44039-48708 | `a_422_muchunk_iis_128root_grad128_comp_stream_is_exact...` |
| T11 | 48709-53366 | `the_chroma_rect_gates_excluded_seed_46_decodes_pixel_exact` |
| T12 | 53367-57178 | `a_lossless_444_10bit_inter_stream...` (`FrameFacts` at 53604) |

### 3b. Shared helpers
Helpers used across chunks (`test_card`, `panned_test_card`, `gop_round_trips`, `read_pin`,
`stream_bit_depth`, `chroma_shape`, `assert_420_oracle_stream`, `panic_message`, `pack_rawvideo`,
`assert_rawvideo_matches`, `fnv1a64`, `have_ffmpeg`, `ffmpeg_decode_*`, `libaom_encode*`,
`encode_aomenc_stream*`, `census_attempt`, `*_gate` fns, `GATE_COUNTER_LOCK`,
`lock_gate_counters`) are not `#[test]`. They sit inside the chunks above. Step S2 moves all of
them into `stream/tests/support.rs` as `pub(super)`. The `gate` fns (intra_rect_in_inter_gate
22228, tx_select_inter_gate 22525, ..., edge32_gate 44519, run_multi_tile_gate 47907) are helpers too.

Fix to `use`: each chunk file starts with `use super::super::*;` and `use super::support::*;`
(`super` = `stream::tests`). Where a test calls `crate::stream::decode_stream` directly, nothing changes.

## 4. Cross-cutting private items that must go pub(super)/pub(crate)

decode.rs (found by the planned module boundaries):
- Structs: `FrameCtx` (216), `TxParams`, `Residual`, `Grid` (Deref at 3521), `WaveQueue`, `WaveGuard`,
  `ReconInline`, `PlaneBuf` (impl at 22459 and 39547), `PlaneGuard`, `Neighbours` (9241), `TxClass` (7858),
  `ObmcPlan`, `ReducedTxRestore`, `ReducedRestore`, `PipeSync`, `PipeGuard` (35524-35535).
- Fns used by several regions: `unsupported` (7743), `ss_x`, `ss_y`, `sb_mi_cur`, `intra_inter_ctx`,
  `switchable_interp_ctx`, `read_intra_mode`, `read_plane`, `read_coeffs`, `read_block_tx_size`,
  `decode_block`, `decode_leaf8`, `decode_block_rect4`, `decode_inter_block`, `decode_intra_sub8_leaf`,
  `cdef_band`, `replay_filters`, `obmc_run`, `obmc_skip_chroma_above`, `whole_plane`.
- Consts used across: `CDEF_*` (28661-29965), `INTER_BLOCK_SHAPES`, `UV_CFL_PRED`, `MI`, `SUB`,
  `SB_MI`, etc. The `use super::{CDEF_BORDER, ...}` lines in `mod cdef_simd` / `mod deblock_simd` show which ones.
- `cdef_simd` and `deblock_simd` (inline mods, `#[allow(unsafe_code)]`) move with their attributes to files
  `decode/cdef_simd.rs` and `decode/deblock_simd.rs`. Their `use super::{...}` list needs `pub(super)` on the consts it imports.

stream.rs: `Reorder`, `SlotValue`, `SlotPromise`, `PromiseState`, `LeafResult`, `collect_one`, `SeqFlags`,
`RefSnapshot`, `FrameOutput`, `stored_cdfs_for`, `decode_frame`, the `FRAMES_DISPATCHED` static (487), and the
`final_dump_prefix` / `set_final_dump_prefix` pair (57-66). Only the test chunks move, so these stay in
`stream/mod.rs` and need no visibility change. The test files reach them through `super::super::*`.

encode.rs: `Plane` (2653), `CoefCtx`/`CoefCtxMap` (2562/2585), `At` (3782), `Reach` (4037), `Ibc` (6094),
`Search` (8495), `Trial` (8720), `VertAbGuard` (3908), `EncodedSequence` (15713), and the `SPLIT_CENSUS` static (5004).

tile.rs: `BlockCoeffs`, `PaletteY`, `Coeff`, `write_palette_syntax`, `write_inter_frame_leaf`, `write_compound_block` (1782), `palette_uv_side`.

## 5. Module layouts and steps

Module sizes below are approximate; the executor re-counts with `wc -l` before each step.

### 5a. decode.rs -> decode/
Proposed layout (every module under 5k):

| Module | Source lines (base) | ~Lines | Depends on |
|---|---|---:|---|
| decode/mod.rs | 1-8492 minus counters (~4,850 left) + `pub use` re-exports | 4,850 | (top of tree) |
| decode/counters.rs | 318 hit/census items (2c) | 4,255 | `crate::hits` |
| decode/coeffs.rs | 8493-12083 (`read_coeffs*`, `Neighbours`, filter-intra, palette index) | 3,590 | mod.rs types |
| decode/rect.rs | 12084-16934 | 4,850 | coeffs, mod.rs |
| decode/rect4.rs | 16935-20788 | 3,850 | rect, block |
| decode/block.rs | 20789-24729 minus counters (~3,680) | 3,680 | coeffs, rect, leaf8 |
| decode/leaf8.rs | 24730-26796 | 2,070 | block, coeffs |
| decode/leaf_sub8.rs | 26797-29818 | 3,020 | leaf8, block |
| decode/cdef.rs | 29819-31336 + cdef_simd (30215-30469) | 1,520 | mod.rs consts |
| decode/inter_shapes.rs | 31337-33053 | 1,720 | coeffs (read_coeffs), mod.rs |
| decode/deblock.rs | 33054-34521 + deblock_simd (33383-33827) | 1,470 | mod.rs |
| decode/replay.rs | 34522-36564 minus counters (~1,890) | 1,890 | cdef, deblock |
| decode/key_tile.rs | 36565-39260 | 2,700 | block, leaf8, replay, deblock, cdef |
| decode/inter_ctx.rs | 39261-42377 (ctx fns, ref fns, obmc_run) | 3,120 | mod.rs, counters |
| decode/inter_block.rs | 42378-48099 (after 2d refactor, split across 2 files) | 5,722 -> ~2 x 2.9k | inter_ctx, block |
| decode/inter_sub8.rs | 48100-51108 | 3,010 | inter_block, block |
| decode/inter_block8.rs | 51109-54360 | 3,250 | inter_block, inter_sub8 |
| decode/frame_tile.rs | 54361-57728 | 3,370 | key_tile, inter_block8, replay |
| decode/tests.rs | 57729-61558 (inline mod tests) | 3,830 | everything (via `super::*`) |
| decode/tx4x8_table_tests.rs | 61559-61650 | 90 | |
| decode/obmc_skip_chroma_tests.rs | 41528-~41600 | 75 | inter_ctx |

Dependency rule: no module depends on a module that comes later in the table, except through
`mod.rs` re-exports. Check with `cargo check` after each step.

### 5b. stream.rs -> stream/
- stream/mod.rs: production 1-2328 (about 2.3k). No split.
- stream/tests/mod.rs: `mod support; mod t01; ... mod t12;`
- stream/tests/support.rs: all non-`#[test]` helpers (section 3b), about 1.5k lines. Estimated; the exact figure comes from step S2.
- stream/tests/t01.rs ... t12.rs: chunks T1-T12 (about 4.5k each, minus the helpers moved out).

### 5c. encode.rs -> encode/ (23,771 lines)
Production 1-15867 (15.9k) plus tests 15868-23771 (7.9k).

| Module | Source lines | ~Lines |
|---|---|---:|
| encode/mod.rs | 1-2548 (Picture 1576, Encoded 1902, CdfSnapshot 1894) | 2,550 |
| encode/coef.rs | 2549-5003 (CoefCtx 2562, Plane 2653, At 3782) | 2,460 |
| encode/split.rs | 5004-8334 (SPLIT_CENSUS 5004, `split_census_on`, `code_square_inter`) | 3,330 |
| encode/inter_search.rs | 8335-13021 (`record_mi*`, Ibc 6094 may move here, Search 8495, `search_inter_block` 10164-11894) | 4,690 |
| encode/frame.rs | 13022-15866 (TPL_STRENGTH, `deltaq_res_log2`, `encode_inter_frame`, `encode_sequence_with_ctx` 15713) | 2,850 |
| encode/tests.rs (A) | 15868-19800 (first half of tests) | 3,930 |
| encode/tests_b.rs (B) | 19801-23771 (second half, cut at a `#[test]` boundary) | 3,970 |

### 5d. tile.rs -> tile/ (11,516 lines)
| Module | Source lines | ~Lines |
|---|---|---:|
| tile/mod.rs | 1-2509 (includes compound hit statics 1720-1762 unless moved to a counters module) | 2,510 |
| tile/coeff_syntax.rs | 2510-5008 (`BlockCoeffs`, `PaletteY`, `Coeff`, `write_palette_syntax`) | 2,500 |
| tile/palette.rs | 5009-8394 (`palette_uv_side`, `PALETTE_HITS` static 5009, `sb_coeff_inter_frame_tile_cdfs`) | 3,390 |
| tile/leaf.rs | 8395-8957 (`write_inter_frame_leaf`, `write_inter_frame_leaf8`) | 560 |
| tile/tests.rs | 8958-11516 (inline `mod tests`) | 2,560 |

### 5e. qm.rs -> qm/ (7,046 lines)
Production is one static literal: `static IWT: [[[u8; 3344]; 2]; 15] = [ ... ]` (139-6937, 6,799
lines, 15 `// qm level k` blocks). It has to become per-level constants:
- qm/mod.rs: `NUM_QM_LEVELS`, `AOM_QM_BITS`, `Iqm`, `adjusted`, `shape_range`, `iwt_matrix`, `dqv_at`, and
  `static IWT: [[[u8;3344];2];15] = [IWT_L0, ..., IWT_L14];`.
- qm/iwt_a.rs: `const IWT_L0..IWT_L4: [[u8; 3344]; 2]` (levels 0-4, about 2.3k).
- qm/iwt_b.rs: levels 5-9 (about 2.3k). qm/iwt_c.rs: levels 10-14 (about 2.3k).
- qm/tests.rs: 6938-7046 (`mod tests`).

### 5f. encoder.rs -> encoder/ (6,503 lines)
- encoder/mod.rs: 1-2557 (2,556 lines; `Av1Encoder` impl 1506).
- encoder/tests.rs: 2558-6503 inline `mod tests` (3,946 lines).

### 5g. cdf.rs (5,519 lines)
- cdf.rs stays at 1-3724 (3,724 lines) and 3850-5519 becomes cdf/tables_tail.rs (1,670 lines, const tables, `pub use` back).
- `#[cfg(test)] mod tests` 3725-3849 (124 lines) -> cdf/tests.rs.

## 6. Ordered extraction steps

Each step moves at most 6k lines, leaves the crate compiling, and is verified with `cargo check -p ec-av1 --all-targets`
and `cargo test -p ec-av1 --lib -- --list` (the count must not change). Test runs happen on the
server per the orchestration doc.

### Phase 0 (prerequisite, all files)
- **P0.** Add the directory-aware source reader in the test helper. Replace the ~70 `include_str!`
  sites in gate_coverage.rs, stack_budget.rs, decode.rs (section 1a). Add the new file names to `stack_budget::FILES`.
  Moves 0 lines. Makes the later steps safe.

### decode.rs
- **D1.** `git mv src/decode.rs src/decode/mod.rs`. 0 lines moved.
- **D2.** Move `mod tests` 57729-61558 to `decode/tests.rs` and `mod tx4x8_table_tests` 61559-61650 to
  `decode/tx4x8_table_tests.rs`. 3,920 lines moved. Also `mod obmc_skip_chroma_tests` 41528-~41600 to a file (75 lines).
- **D3.** Move the 318 counter items (2c) to `decode/counters.rs` (about 4.3k). They are spread across
  the file, so the step is a name-based script. Set `pub(super)`, add `pub use counters::*;` in mod.rs.
- **D4.** Move `mod cdef_simd` 30215-30469 and `mod deblock_simd` 33383-33827 to files (700 lines). Make the
  consts they import `pub(super)`.
- **D5.** Move 54361-57728 (`decode_inter_frame_tile*`) to `decode/frame_tile.rs` (3,370 lines). The step must
  import `key_tile` names. It moves first because nothing below depends on it.
- **D6.** Move 51109-54360 (`grid_stamp_rect`, `decode_inter_block8`) to `decode/inter_block8.rs` (3,250 lines).
- **D7.** Move 48100-51108 (`decode_inter_sub8_split4`, `decode_intra_sub8_leaf`, `decode_inter_sub8_rect2`)
  to `decode/inter_sub8.rs` (3,010 lines).
- **D8a.** (Blocker) Refactor `decode_inter_block` (42378-48099, 5,722 lines) in place: extract its phase blocks
  into named helper fns in the same file, until no single fn is over 5k. Gate: decode pins and gate_coverage.
  Moves 0 lines to new files.
- **D8b.** Move the refactored `decode_inter_block` and its helpers to `decode/inter_block.rs`, split into two
  files if needed (each under 5k). About 5.7k moved; within the 6k limit.
- **D9.** Move 39261-42377 (ctx/ref fns, `obmc_run`) to `decode/inter_ctx.rs` (3,120 lines).
- **D10.** Move 36565-39260 (`decode_key_frame_tile*`) to `decode/key_tile.rs` (2,700 lines).
- **D11.** Move 34522-36564 (`replay_*`, `cdef_band`), counters removed, to `decode/replay.rs` (1,890 lines).
- **D12.** Move 33054-34521 (`deblock_thresholds`, `sclamp`, `round_pow2`, `pooled`) to `decode/deblock.rs` (1,470 lines).
- **D13.** Move 31337-33053 (`read_block_tx_size`, `read_inter_rect_chroma`, `INTER_BLOCK_SHAPES`, ...) to
  `decode/inter_shapes.rs` (1,720 lines).
- **D14.** Move 29819-31336 (`CDEF_*` consts, `cdef_find_dir`, `cdef_adjust_strength`, `cdef_filter_block*`) to
  `decode/cdef.rs` (1,520 lines).
- **D15.** Move 26797-29818 (`sub8_leaf_chroma444/422`, `decode_leaf_split4`, `decode_leaf_rect8`) to
  `decode/leaf_sub8.rs` (3,020 lines).
- **D16.** Move 24730-26796 (`decode_block_128rect`, `decode_leaf8`, `read_intra_mode_sub8`) to `decode/leaf8.rs` (2,070 lines).
- **D17.** Move 20789-24729 (`read_intra_mode`, CFL, `PlaneBuf` impl, `read_plane`, `decode_block`, counters removed) to
  `decode/block.rs` (3,680 lines).
- **D18.** Move 16935-20788 (`decode_block_rect4`, `decode_rect4_16_*`, `decode_intrabc_owned_rect`,
  `decode_block_rect64`) to `decode/rect4.rs` (3,850 lines).
- **D19.** Move 12084-16934 (`read_intra_mode_rect`, `decode_rect_split`, `decode_intra_rect_in_inter`,
  `decode_intrabc_rect`, `decode_intrabc_128rect`, `decode_block_rect`, `decode_leaf_rect`) to `decode/rect.rs` (4,850 lines,
  tight; if it is over, move `decode_intrabc_*` to its own file).
- **D20.** Move 8493-12083 (`read_coeffs`, `read_coeffs_rect`, `Neighbours` struct and impl, filter-intra and palette index helpers)
  to `decode/coeffs.rs` (3,590 lines).
- **D21.** Final check: `decode/mod.rs` should be 1-~8492 minus counters, about 4,850 lines, and under 5k.
  Confirm every `pub` item of the old decode.rs is re-exported.

Decode total: 22 steps (D1-D21, with D8 split into D8a and D8b). Largest single move: D8b (about 5.7k).

### stream.rs
- **S1.** `git mv src/stream.rs src/stream/mod.rs` (0 lines moved).
- **S2.** Move every non-`#[test]` helper in `stream::tests` (section 3b) to `stream/tests/support.rs` as `pub(super)`.
  Create `stream/tests/mod.rs` with `mod support;`. About 1.5k lines moved. (Lines are name-based; re-measure.)
- **S3.** Move T1 (`stream` tests chunk 2329-6884 minus helpers, about 4.0k) to `stream/tests/t01.rs`.
- **S4-S14.** One step per chunk: T2 (6885-11454), T3 (11455-16256), T4 (16257-20952), T5 (20953-25719),
  T6 (25720-30250), T7 (30251-34861), T8 (34862-39404), T9 (39405-44038), T10 (44039-48708),
  T11 (48709-53366), T12 (53367-57178). Each moves about 4.6k lines.
- **S15.** Remove the now-empty `#[cfg(test)] pub(crate) mod tests` header and confirm `stream/mod.rs` is about 2.3k lines.

Stream total: 15 steps (S1-S15), each under 5k moved.

### encode.rs
- **E1.** `git mv` to `encode/mod.rs` (0 lines).
- **E2.** Move `mod tests` 15868-19800 to `encode/tests.rs` (about 3.9k).
- **E3.** Move 19801-23771 to `encode/tests_b.rs` (about 4.0k).
- **E4.** Move 13022-15866 (`TPL_STRENGTH`, `encode_inter_frame`, `EncodedSequence`, `encode_sequence_with_ctx`) to `encode/frame.rs` (2,850).
- **E5.** Move 8335-13021 (`record_mi*`, `Search`, `search_inter_block`) to `encode/inter_search.rs` (4,690).
- **E6.** Move 5004-8334 (`SPLIT_CENSUS`, `split_census_on`, `code_square_inter`) to `encode/split.rs` (3,330).
- **E7.** Move 2549-5003 (`CoefCtx`, `Plane`, `At`, `Reach`) to `encode/coef.rs` (2,460).
- **E8.** Check that `encode/mod.rs` is about 2.5k lines, and under 5k.

Encode total: 8 steps.

### tile.rs
- **T1.** `git mv` to `tile/mod.rs`.
- **T2.** Move `mod tests` 8958-11516 to `tile/tests.rs` (2,560).
- **T3.** Move 5009-8394 (`palette_uv_side`, `PALETTE_HITS`, `sb_coeff_inter_frame_tile_cdfs`) to `tile/palette.rs` (3,390).
- **T4.** Move 8395-8957 (`write_inter_frame_leaf*`) to `tile/leaf.rs` (560).
- **T5.** Move 2510-5008 (`BlockCoeffs`, `PaletteY`, `Coeff`, `write_palette_syntax`) to `tile/coeff_syntax.rs` (2,500).

Tile total: 5 steps.

### encoder.rs
- **N1.** Move `mod tests` 2558-6503 to `encoder/tests.rs` (3,946). Keep `encoder.rs` as `encoder/mod.rs` (2,556 lines).

Encoder total: 1 step (plus the rename).

### qm.rs
- **Q1.** Move `mod tests` 6938-7046 to `qm/tests.rs`.
- **Q2.** Convert `static IWT` levels 0-4 (about 2.3k lines) into `qm/iwt_a.rs` as consts, and make the outer
  `static IWT` in `qm/mod.rs` reference them.
- **Q3.** Levels 5-9 to `qm/iwt_b.rs`.
- **Q4.** Levels 10-14 to `qm/iwt_c.rs`. Confirm `qm/mod.rs` is under 1k.

qm total: 4 steps. Q2-Q4 must keep the same bytes: check the output of `iwt_matrix` with an existing
pinned test before and after.

### cdf.rs
- **C1.** Move 3725-3849 (`mod tests`) to `cdf/tests.rs`.
- **C2.** Move 3850-5519 (const tables) to `cdf/tables_tail.rs` (1,670), `pub use` back into `cdf/mod.rs`.

cdf total: 2 steps.

## 7. Summary

- Files needing a split: 7 (decode, stream, encode, tile, qm, encoder, cdf).
- Steps: 1 prerequisite (P0) + decode 22 + stream 15 + encode 8 + tile 5 + encoder 1 + qm 4 + cdf 2 = 58 steps.
- Blocker: `decode_inter_block` (5,722 lines, one fn) needs a behaviour-preserving refactor (D8a) before its file move.
- Risk: about 70 `include_str!` source scanners (P0 must land first).
- Risk: `tile.rs` / `decode.rs` / `encode.rs` counter statics and `take_*_hits` fns are referenced from
  tests in other modules; keep the names and re-exports.
