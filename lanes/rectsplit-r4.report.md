# lane-rectsplit r4 — SB-level split-transform strips: TWO root causes, refusal lifted

Branch `lane-rectsplit` off `4c3389a` (r3's safe state). No rebase needed: `main` is still
`3808cf8`, the branch's merge base.

## Headline

r3's four-way table was not a contradiction — it was two defects stacked, the first hiding
the second behind a refusal:

1. **`base_ctx` read the rect `av1_nz_map_ctx_offset` tables transposed.** libaom indexes
   them by its own `coeff_idx`, and `get_nz_map_ctx_from_stats` decomposes that as
   `col = coeff_idx >> bhl`, `bhl` = log2 of the *adjusted* height (5 for both `TX_64X32`
   and `TX_32X64`): `coeff_idx = col * 32 + row`, so 32 consecutive flat entries are one
   COLUMN and the `cdf.rs` 5x5 transcriptions are `[col][row]`. `decode.rs:1162` read them
   `[row][col]` = the transpose = the other shape's rule with 11 and 16 swapped.
   Fixed by stating libaom's generating rule (`txb_common.h:199-209`) once, exactly as
   `base_ctx_rect` already did, instead of indexing the tables at all.
2. **`EOB_PT_512_CHROMA` was transcribed from the wrong row of
   `av1_default_eob_multi512_cdfs[q][plane][2]`.** The last index is `eob_multi_ctx`
   (0 = `TX_CLASS_2D`, 1 = `V_DCT`/`H_DCT`), not a q-context: the flat
   `3277, 6554, ...` distribution our table held is the 1D-class row, identical for both
   planes at every q, and the real chroma 2D row has four q-context variants like every
   other coefficient table. The FIRST real 32x16 chroma transform read `eob_pt` 7 where
   aomdec read 2 — that is what made seed 43 RED once defect 1 was fixed. Under r3's
   shipped code seed 43 never reached it: it desynced 13 elements EARLIER and exited
   through a named refusal, which the gate counts as a pass (class `refusal-hides-a-defect`).

## Changed

- `crates/ec-av1/src/decode.rs:1162` `base_ctx` — the `TX_CLASS_2D` rect arm now applies
  libaom's generating rule (`row < 2 -> 11` when w<h, `col < 2 -> 16` when w>h, else the
  square table), no transposable table read left.
- `crates/ec-av1/src/cdf.rs:269` — `EOB_PT_512_CHROMA` corrected to the 2D chroma row plus
  new `_Q0`/`_Q1`/`_Q3` siblings; `cdf_state.rs:1015` now `pick()`s by q-context.
- `crates/ec-av1/src/decode.rs:4102` `decode_block_rect64` — the split-transform refusal is
  LIFTED, wired to `decode_rect_split` (r1's per-unit port), `RECT_PARTITION_HITS` bumped.
- `crates/ec-av1/src/decode.rs:3428` `decode_rect_split` — records `RECT64_QIDX_DRIFT_HITS`
  for 64-level strips, which now reach this path.
- `crates/ec-av1/src/refusal_inventory.rs:49` — refusal string removed.
- `crates/ec-av1/src/stream.rs:9778` — gate (b) un-`#[ignore]`d, its doc records both causes.
- Instruments kept: `read_coeffs`/`read_coeffs_rect` print an `EC_COEFF_STEP` ladder under
  `EC_TRACE_COEFF` in instrumented-aomdec format, with `pos` converted to libaom's
  column-major `coeff_idx` so the two ladders `diff` line by line.
- New unit test `decode::tests::base_ctx_rect_offsets_match_the_transcribed_tables_over_the_whole_domain`
  — evaluates `base_ctx` over the WHOLE 5x5 domain of 64x32 / 32x64 / square against
  `table[col][row]` (class `enumerate-table-domain`).

## Refusals lifted

- `"a superblock-level HORZ/VERT strip with a split transform (per-unit rect prediction is not ported)"`
  — with `a_real_aomenc_stream_with_a_split_transform_superblock_strip_decodes_pixel_exact`
  green, hard-asserting `rect_split_sb_interior_tu_hits` (SB-level AND tx depth >= 2) sampled
  per compared attempt.

## Gates and evidence

EVIDENCE: msac range ladders `scratchpad/n_ours43.txt` vs `scratchpad/n_aom43.txt` | `EC_TRACE_COEFF=1 decode_probe seed43.obu` vs `EC_TRACE_COEFF=1 aomdec` on the gate-dumped seed-43 stream | first divergent element = the chroma `eob` of the 64x32 strip at mi (0,16): ours `eob_pt` group 8 (eob 103), aomdec group 3 (eob 3), at identical range 44680 — i.e. a CDF-content defect, not a context one
EVIDENCE: `scratchpad/r4_ours43.yuv` vs `scratchpad/r4_aom43.yuv` | `decode_probe seed43.obu out.yuv`; `aomdec --rawvideo` | `cmp` byte-identical (was: frame-0 luma mismatch)
EVIDENCE: `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3 -- --test-threads=1 --include-ignored superblock_level_horz_vert_partition split_transform_horz_vert_strip filter_intra_on_a_horz split_transform_superblock_strip refusal_inventory gate_coverage nz_map` | gates (a) (b) (c) + merged SB HORZ/VERT + inventory/coverage/table tests | 10 passed, 1 failed (81.41s) — the failure is the already-`#[ignore]`d `..._and_delta_q_...` gate, which fails its own drift counter, not a pixel compare (27/40 pixel-exact, 13 named refusals)
EVIDENCE: third leg from lane-tx64x16 — `scratchpad` sweep seeds 42..81, 192x128 gradients, merged-gate recipe with `--min-partition-size=16 --enable-1to4-partitions=1` | encode with the oracle aomenc, `decode_probe` vs `aomdec --rawvideo`, `cmp` | 33 EXACT, 7 named refusals (6x SB-level AB partition, 1x screen-content strip), 0 mismatches — seed 55 (reported 3944 wrong luma samples, first at (154,59)) is now byte-exact
EVIDENCE: full `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3` | whole crate lib | **272 passed, 0 failed, 23 ignored**, 490.90s (r3's expectation was 270/0/24; +1 un-ignored gate, +1 new unit test)

## Film check (Hunger Games, the lane's critical path)

`decode_probe` on the coordinator's mid-film extract (`ffmpeg -ss 600 -t 0.5 ... -f obu`,
11.5 MB, 10-bit 2160p, `.../cdc46329-.../scratchpad/hg5.obu`), which stopped at exactly this
lane's refusal on main:

```
before (main 06d856d): REFUSED: a superblock-level HORZ/VERT strip with a split transform ...
after  (this branch):  REFUSED: AV1 tile (a 32x32 partition type this decoder does not code (value=8))
```

EVIDENCE: `.../cdc46329-.../scratchpad/hg5.obu` | `cargo run -p ec-av1 --example decode_probe` on `lane-rectsplit` r4 | the film advances past this lane's refusal to `PARTITION_HORZ_4` (value 8), a 1:4-partition gap owned by lane-part32/tx64x16

## Sibling sweep (defect class)

- Class `table-transcribed-from-the-wrong-row-of-a-multi-dimensional-default`: every other
  coefficient CDF in `Cdfs::new` already goes through `pick(q_ctx, ...)`; a grep of
  `cdf.rs` for libaom's flat 1D rows (`3277, 6554` / `2979, 5958` / ...) finds no second
  instance. `EOB_PT_512_CHROMA` was the only coefficient table initialised without `pick`.
- Class `reference-layout-not-spec` (transposed table): the only other rect offset consumer,
  `base_ctx_rect`, already stated the rule and is unchanged; the new domain test covers both.

## Residue

- deferred(lane-part32 / lane-tx64x16 — 1:4 partitions): both films and 7/40 seeds of the
  third-leg sweep now stop at `PARTITION_HORZ_4/VERT_4` or SB-level AB partitions.
- deferred(lane-rect64q): `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact`
  stays `#[ignore]`d as r3 left it — its 40 attempts pixel-match (27 compared, 13 named
  refusals) but `deltaq-mode=1` never drifts `CURRENT_Q_IDX` inside a 64-level strip in
  that recipe. r4 added the missing counter bump on the split path; the gate still needs a
  recipe that actually drifts.
- accepted: the third-leg recipe (`--min-partition-size=16 --enable-1to4-partitions=1`) is
  measured here but NOT added as a gate arm — its remaining refusals are 1:4/AB partitions
  owned by other lanes, so the arm would be RED for reasons outside this lane.
