# lane-interp3 r2 -- the compound 8x8 leaf's ENTROPY desync is closed (two twin drifts); gate now RED on a small luma reconstruction residue

Base: b702853 (r1) + `git merge main` (4eff3a1, brings lane-mvtwin's mvstack fixes).
Merge diffstat: `crates/ec-av1/src/mvstack.rs`, `crates/ec-av1/src/stream.rs`,
`lanes/mvtwin-r1.report.md` only -- `cdf.rs` untouched by either side (byte-compare
trivially clean), no fixtures churn, no conflicts.

## Phase table -- libaom `read_inter_block_mode_info` (decodemv.c ~1400-1600)

`IB` = `decode_inter_block` compound arm, `L8` = `decode_inter_block8` compound arm
(both in `crates/ec-av1/src/decode.rs`; line numbers are this commit's).

| # | phase (libaom) | IB | L8 | verdict |
|---|---|---|---|---|
| 1 | `skip_mode` (ctx = above+left skip_mode) | 16363 | 19087 | matches |
| 2 | `skip` | 16375 | 19093 | matches |
| 3 | cdef / delta_q / delta_lf | 16377-16382 | (16x16 slot's, per this fn's doc) | matches (leaf never a delta root) |
| 4 | `is_inter` (`intra_inter_ctx`) | 16389 | 19105 | matches |
| 5 | `comp_mode` (`read_comp_mode`, ref-count ctx) | 16475 | 19147 | matches (`skip_mode \|\| (reference_select && ...)` vs `reference_select \|\| skip_mode` then `skip_mode \|\| ...` -- same predicate) |
| 6 | `read_ref_frames`: `comp_ref_type`, uni/bi trees | 16480 | 19151 | **DIFFERENT -> FIXED**: L8 passed a hard-coded `LAST_FRAME` (or `-1`) as the above/left reference; IB passes the real `neighbours.above_ref[cmi]`/`left_ref[rmi]` |
| 7 | `av1_find_mv_refs` (`find_mv_stack_compound`, sign bias + gm table + tpl) | 16519 | 19195 | matches (L8's doc comment claiming "no sign-bias table nor tpl_frame" is stale -- both are passed) |
| 8 | `compound_mode` CDF (`COMPOUND_MODE_CTX_MAP`) | 16532 | 19205 | matches |
| 9 | drl + `read_mv` per ref (`assign_compound_mv`, has_nearmv/have_newmv rules) | 16537 | 19226 | matches (one shared fn) |
| 10 | `interintra` | n/a compound | n/a | matches (never read on compound -- correct) |
| 11 | `read_motion_mode` | n/a compound | n/a | matches (`has_second_ref` -> SIMPLE_TRANSLATION) |
| 12 | `comp_group_idx` (neighbour ctx) | 16675 | 19263 | matches |
| 13 | `compound_type` -> wedge idx+sign / diffwtd `mask_type` | 16710 | 19279 | matches |
| 14 | `compound_idx` (distance-weight ctx) | 16936 | 19310 | matches |
| 15 | `read_mb_interp_filter` (after `read_compound_type`; suppressed by `skip_mode` / `is_nontrans_global_motion`; `get_ref_filter_type` on ref0 matching EITHER neighbour ref) | 16980 | 19383 | matches (landed r1) |
| 16 | tail: grid `MiInfo` stamp -- per-slot `is_global_mv_block` | 16577+16660 | 19424 | **DIFFERENT -> FIXED**: L8 stamped `is_global_mv0/1 = false` unconditionally |
| 17 | tail: `record_inter_rect_mi` + `record_compound_ctx_rect_mi` (skip/skip_mode/inter/ref/ref1/filter/comp_group_idx/compound_idx bands) | 18870 | via `compound_ctx8` + caller, 19000 | matches -- verified by the range ladder staying exact for the whole stream |

Root causes 6 and 16 are the round's fix. Both are the same class the charter named
(twin functions drift), and both are invisible to any value-only check: (6) changes a
CDF ROW, (16) changes a LATER block's mv-stack composition.

## Changed (path:line)

- `crates/ec-av1/src/decode.rs:19151` -- `read_compound_ref_frames` now gets
  `above_ref0`/`left_ref0` (the per-mi bands the leaf already reads at 19078-19081)
  instead of `LAST_FRAME`/`-1`. `-1` on an unavailable band is `start_tile`'s /
  `start_row`'s own reset value, so no `has_above` guard is needed (IB has none either).
- `crates/ec-av1/src/decode.rs:19410` -- the leaf's grid stamp computes libaom's
  per-slot `is_global_mv_block` (mode `GLOBAL_GLOBALMV`, that slot's gm model >
  TRANSLATION; the >= 8x8 size predicate is always true for this leaf).
- `crates/ec-av1/src/decode.rs:19726` -- the leaf's SINGLE-ref arm prints the same
  per-entry `EC_STACK` ladder `decode_inter_block`'s single-ref arm prints (this is
  the instrument that isolated cause 16).
- `crates/ec-av1/src/stream.rs:6315` -- the gate's ignore reason + comment replaced
  with this round's measurement (entropy closed, pixel residue open).

## Gate: still RED, still `#[ignore]`d -- but on a different, much smaller failure

`a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
(`--enable-dual-filter=1 --enable-obmc=1 --enable-warped-motion=1
--enable-onesided-comp=1`, `--min-partition-size=8 --enable-ab-partitions=0
--enable-1to4-partitions=0`, 16 frames, 64x64).

- r1: decode STOPPED at a bogus refusal at frame 5 (0 frames ever compared).
- r2: the whole 16-frame stream parses; the failure is now
  `frame 7 mismatch vs ffmpeg` -- a reconstruction residue, luma-only,
  |delta| <= 8, first sample (42,15) inside/around the 8x8 leaf at mi (10,4),
  spilling 1 px across that leaf's left and bottom deblock edges. It grows by pure
  propagation to 222 Y + 4 U + 1 V samples at frame 15 (chroma stays exact until
  frame 14).

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/{aom16.trace,ours16e.trace} | `EC_TRACE_MODE=1 aomdec --rawvideo` and `EC_TRACE_MODE=1 decode_probe` on the pinned 16-frame stream, `diff` of every `EC_MODE`/`EC_MODE_VAL` line (mi + msac RANGE, never tell) | 570 vs 570 elements, diff EMPTY, no refusal (before the fix: ours stopped at 347 elements, first RANGE divergence at the 8x8 leaf mi (8,8), stack 3 entries vs libaom's 2)
EVIDENCE: same dir, `pin.obu` (EC_INTERP3_FRAMES=5) | ladder before fix 1 | ours read refs (LAST,LAST2) + NEW_NEWMV at leaf mi (10,2) where aomdec reads (LAST,GOLDEN) + NEAREST_NEARESTMV -- the two differ only in a CDF row of `uni_comp_ref`
EVIDENCE: same dir, `mm.{aom,ours}.yuv` | `aomdec --rawvideo` vs `decode_probe` on the pinned mismatch stream, per-frame per-plane sample compare | frames 0-6 byte-exact; frame 7: 36 Y / 0 U / 0 V wrong, max |delta| 5; frame 15: 222 Y / 4 U / 1 V, max |delta| 8

## Refusals

None lifted this round (the r1 refusal `an OBMC neighbour whose interp filter was
never recorded` stays; `refusal_inventory` unchanged). Two refusals this stream used
to hit -- `an inter 16x16-level AB or 1:4 partition` and `an inter partition below
8x8` -- were our own desync (class refusal-from-own-desync) and no longer fire on it.

## Residue

- fix-now(next round): the luma reconstruction residue above. First suspect is the
  OBMC blend over a COMPOUND neighbour -- the leaf now records a real filter pair
  where it used to record the `[3,3]` sentinel, and the oracle's `EC_OBMC` rung
  prints `nbmv`/`nbref`/`nbbsize`/`filt` per neighbour while THIS decoder has no
  `EC_OBMC` emitter at all. Build that emitter first (instrument-before-excuse), then
  diff the 325 oracle `EC_OBMC` lines of `mm.obu` against ours.
- deferred(needs that fix): the gate's counter asserts (compound-8x8 filter reads,
  dual-filter differing directions, 8x8 OBMC blends) are never reached while the
  pixel compare fails first; the 10-bit arm; a `--tile-columns=1` arm.
