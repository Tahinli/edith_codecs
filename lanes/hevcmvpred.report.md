# lane hevcmvpred: merge, AMVP and temporal motion vector derivation (8.5.3.2)

## Goal
Replace the `ZeroMotion` placeholder with the real luma motion derivation: a motion field per
picture, merge mode, AMVP, temporal candidates from the collocated picture, and chroma vector
helper. Pixels are still the placeholder (motion compensation is another lane).

## Base
`1b962d32` (main, merge of lane/hevcpu).

## What changed
- `dec/motion_field.rs` (new):
  - `PbMotion`: `predFlagLX`, `refIdxLX` (i8, -1 when unused), `mvLX` (i16). An unused list is
    normalised to ref -1 and zero vector, so equal motion compares equal.
  - `MotionField`: one entry per 4x4 luma block, `Option<FieldCell>` where `FieldCell` is the
    motion plus the slice index. `None` means intra or not decoded yet. `store`, `at`, a running
    FNV `checksum`.
  - `SliceRefs`: POCs and long-term flags of a slice's `RefPicList0/1`, as they were when the slice
    was decoded.
  - `MotionField::compress(poc) -> ColField`: keeps the top-left 4x4 of each 16x16 block (all that
    8.5.3.2.8 reads) together with `slices`. `ColField` is what the DPB keeps.
  - `collocated(dpb, header, lists)`: picks `colPic` (`RefPicList1` when B and
    `collocated_from_l0_flag` is 0, else `RefPicList0`, at `collocated_ref_idx`).
  - `SliceMotion<'a>`: the real `MotionSource`. Holds the field, lists, POC, collocated view and
    slice parameters (`MotionParams`: MaxNumMergeCand, Log2ParMrgLevel, CtbLog2, collocated flags,
    NoBackwardPredFlag). `derive_motion` runs merge or AMVP, then stores the PU's motion.
  - `temporal_mv` / `col_mv` (8.5.3.2.8 and 8.5.3.2.9): bottom-right inside the CTB row and the
    picture, else centre; the collocated list choice (intra, uni, bi with `NoBackwardPredFlag`);
    LongTermRefPic mismatch gives unavailable; scaling by POC distance unless the target is
    long-term or the distances are equal.
  - `scale_mv` (the tx / distScaleFactor / clip formulas), `chroma_mv_420` (4:2:0 chroma vector in
    1/8 units is the luma quarter-sample value), `to16` / `to32`.
- `dec/merge.rs` (new): 8.5.3.2.2 to 8.5.3.2.5.
  - Spatial A1, B1, B0, A0, B2 with pruning against the spec's raw availability, the parallel
    merge level, singleMCLFlag for 8x8 CUs when Log2ParMrgLevel > 2, and the second-partition
    exclusions (Nx2N/nLx2N/nRx2N for A1, 2NxN/2NxnU/2NxnD for B1).
  - Temporal candidate (refIdx 0, both lists for B).
  - Combined bi-predictive candidates (Table 8-7 order, POC-or-MV difference test).
  - Zero candidates (numRefIdx rule), then the 8x4 / 4x8 bi-to-uni restriction on the chosen one.
- `dec/amvp.rs` (new): 8.5.3.2.6 and 8.5.3.2.7.
  - Spatial A: first pass without scaling (same POC, either list), second pass with scaling
    (matching long-term marking). B: same, plus the `isScaledFlagLX == 0` rule (the unscaled B
    candidate takes the A slot and B is re-derived by the second pass).
  - Duplicate removal, temporal only when fewer than two spatial candidates, zero fill.
  - `mvLX = mvpLX + MvdLX` with the 16-bit wrap (8-57 to 8-60) in `wrap16`.
- `dec/dpb.rs`: `DpbPicture::motion: Option<Arc<ColField>>`, `Dpb::set_motion`.
- `dec/inter.rs`: `decode_inter_slice` now takes the `MotionSource` (the placeholder test passes
  `ZeroMotion`).
- `dec/mod.rs`:
  - `CurPicture::motion`, created with the first inter slice (`slice_motion` helper), with
    `SliceRefs` pushed per slice.
  - Both the syntax-check path and the pixel path use `SliceMotion`.
  - `finish_picture` compresses the field into the DPB picture and mixes its checksum into
    `Decoder::motion_checksum()`.
- `tests/cu_syntax.rs`: placeholder call updated; new test
  `motion_derivation_is_deterministic_over_conformance_streams`.

## Choices and where they follow the spec
- Availability (6.4.2 and 6.4.1) is "already stored in this slice". Decoding order is z-scan, so
  this is the z-scan test for every neighbour that can be referenced. The NxN partIdx 1 exclusion
  falls out, since partIdx 2 is not stored yet. Intra and undecoded blocks are both `None`, which is
  unavailable for spatial and collocated use alike. Intra CUs need no hook in the parser.
- Bottom-right CTB-row test uses the PU's `yPb`. It equals the spec's `yCb` test, since the PU lies
  in the CB, which lies in one CTB.
- `NoBackwardPredFlag` is computed per slice from the lists (all POCs <= current).
- Per-slice reference lists: each slice's lists are stored, and a cell carries its slice index.
  Dependent slice segments and tiles/WPP are refused by the parser, so the single-slice case is the
  one exercised.
- Motion of unused lists is normalised, so the 8x4 restriction and pruning comparisons match
  the spec's "same motion vectors and reference indices" (and HM's `hasEqualMotion`).
- A temporal candidate with a zero `td` is returned unscaled rather than dividing by zero. This is
  not reachable in conformant streams.

## Verify (server, release)
- `scripts/remote-run.sh hevcmvpred -- cargo clippy -p ec-h265 --all-targets -- -D warnings`: **exit 0**,
  no warnings. (Two rounds of lint fixes first: `needless_range_loop`, `unnecessary_mut_passed`,
  and `collapsible_if` in the placeholder test, which now uses let-chains.)
- `scripts/remote-run.sh hevcmvpred -- cargo test -p ec-h265 -p ec-h265-syntax --release`: **exit 0**,
  0 failed.
  - ec-h265 lib 55 (32 before, 23 new: merge 12, AMVP and scaling 11)
  - conformance 15, cu_syntax 4 (3 before, 1 new), dec_meta 4, dec_shell 13, decode 5, encode 4,
    perf 3, rdoq_cache_hash 3
  - ec-h265-syntax lib 18, fuzz 2, sei_sweep 1
- Determinism over the conformance streams: `motion_derivation_is_deterministic_over_conformance_streams`
  decodes every stream twice with the syntax-only parse and motion derivation. Result: **147 streams,
  0 slice failures, equal motion checksums on both runs.** It also asserts zero failures, ignoring
  parameter sets refused by name, as the sweep does.
- Unit tests (hand-built neighbours and hand-computed spec values, all in the new modules):
  - availability: neighbour present, missing or intra, neighbour in another slice
  - pruning: B1 equal to A1 is removed, so merge_idx 1 becomes B0
  - parallel merge level 4 with singleMCLFlag, and level 2 for the same layout
  - second Nx2N partition excludes A1
  - temporal scaling with a negative POC distance (`-64` from `64`, td 4, tb -4)
  - temporal long-term mismatch gives zero candidate
  - combined bi-predictive candidate from an L0-only and an L1-only neighbour
  - 8x4 bi candidate becomes uni L0
  - zero candidates use increasing refIdx
  - AMVP: unscaled A, scaled A (+32 from 64 with td 8, tb 4), negative distance (-64),
    long-term no scaling, long-term mismatch skipped, B fills the A slot, A/B duplicate collapse,
    temporal second slot, 16-bit wrap at both ends, full mvd wrap (32000 + 1000 gives -32536),
    `scale_mv` sign and clip cases
- MV agreement with a reference decoder: **not measured**. `ffprobe -export_side_data mvs` on
  `AMVP_A_MTK_4` returns no motion side data (0 matches), and FFmpeg's HEVC decoder does not export
  motion vectors. `codecview` is visual only. So no per-block comparison is possible here.

## Not done / left
- Pixels: motion compensation is not in this lane. Pixel output is still the placeholder.
- No reference check of the MV values themselves. Their correctness rests on the hand-computed unit
  tests, and on the determinism check plus zero errors across 147 streams. Only a per-block MV
  reference (for example a patched HM run, or a decoder that dumps MVs) would confirm them.
- Multi-slice pictures and dependent slices: the per-slice lists are stored, but the parser refuses
  dependent slices, and the pixel path refuses a second slice. Not exercised end to end.
- Chroma: only the 4:2:0 vector helper is provided.
