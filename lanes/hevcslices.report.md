# lane hevcslices: multiple slices, dependent slice segments, tiles, WPP, TSUNEQBD

## Goal

Accept and decode, pixel-exact for intra pictures, the streams that were refused for
multiple slices per picture (30), tiles (16), dependent slice segments, and unequal
luma/chroma bit depth (TSUNEQBD). Base: `5d7956ca` (main).

## Result

| | Before (`5d7956ca`) | After |
|---|---|---|
| Accepted streams | 100 of 147 | 147 of 147 |
| Intra pictures checked against MD5 SEI | 269 | 349 |
| Intra pictures exact | 269 | 349 |
| Refusals | 47 (30 slices, 16 tiles, 1 TSUNEQBD) | none |

No regressions: every picture that was exact before is still exact.

## Changes

- `dec/tiles.rs` (new): tile scan (6.5.1). Uniform and explicit tile sizes,
  `CtbAddrRsToTs` / `CtbAddrTsToRs`, `TileId` per CTB, tile-start and WPP row-start tests.
  Unit tests for the uniform split and a 2x1 tile layout.
- `dec/slice_seg.rs` (new): a dependent slice segment inherits the header of the
  independent segment before it (only its address and entry points are its own).
- `dec/pic.rs`:
  - `zscan` (MinTbAddrZs) uses tile-scan CTB addresses.
  - `available()` is the single 6.4.1 check: inside the picture, decoded before the
    current block, same `SliceAddrRs` and same tile.
  - `CtbInfo` gains `slice_addr` and `tile`.
  - `SliceCarry` (per picture): the WPP contexts (`TableStateIdxWpp`), the dependent
    segment contexts (`TableStateIdxDs`), `qPY_PREV` across segments, and `SliceAddrRs`.
- `dec/cu_syntax.rs`:
  - `refuse_unimplemented` is removed.
  - The CTB loop runs in tile scan from `slice_segment_address`.
  - Contexts follow 9.3.1: fresh at a tile start; WPP sync from the CTB above-right when
    it is available (else fresh); a dependent segment restores `Ds`.
  - WPP storage after the second CTB of a row in a tile (the spec's `CtbAddrInRs % W == 1`
    or tile-change rule).
  - `end_of_subset_one_bit` and byte alignment at each tile start and each WPP row start in a tile.
  - `qPY_PREV` resets at a slice start (not a segment start), a tile start, and a WPP row start.
  - The split, skip and MPM neighbour reads use `available()`.
  - `sao()` reads the merge flags under the spec's `leftCtbInSliceSeg` / `upCtbInSliceSeg`
    and tile conditions.
- `dec/mod.rs`: each slice segment decodes into the same `PicState` (also in the
  syntax-only path, which used a fresh state per call). The dependent header is resolved
  before decode. The multi-slice refusal is gone. The unequal-depth refusal is gone.
- `dec/deblock.rs`: the slice boundary test compares `SliceAddrRs`, not the segment index.
  The tile boundary test uses `loop_filter_across_tiles_enabled_flag`.
- `dec/sao.rs`: the slice boundary rule uses `SliceAddrRs` (flag of the later slice in
  decoding order); the tile boundary rule is added.
- `dec/motion_field.rs`, `dec/mod.rs`: spatial merge and AMVP neighbours also require the
  same tile (6.4.1). The tile map lives on `MotionField`, so `SliceMotion` is unchanged.
  This path is only checkable once inter pixels land.
- Unequal bit depth: `DecodedPicture` gains `bit_depth_c` and `bit_depth_of(c)`, and
  `grey_planes`. The RPS grey fill takes `(luma, chroma)` depths. The MD5 checker in
  `tests/conformance_intra.rs` sizes each plane by its own depth. The decoder paths for
  intra, transform, SAO, deblock, PCM and chroma QP already used per-component depth.

## Verify

| Command | Result |
|---|---|
| `scripts/remote-run.sh hevcslices -- cargo clippy -p ec-h265 -p ec-h265-syntax --all-targets -- -D warnings` | clean (exit 0) |
| `scripts/remote-run.sh hevcslices -- cargo test -p ec-h265 -p ec-h265-syntax --release` | exit 0; all test binaries pass, 0 failed (includes the new tile unit tests) |
| `scripts/remote-run.sh hevcslices -- cargo check --workspace --all-targets` | exit 0; warnings only in ec-av1 (not this lane) |
| `scripts/remote-run.sh hevcslices -- cargo test -p ec-h265 --release --test conformance_intra -- --nocapture` | total: 349 intra pictures checked, 349 exact, 147/147 streams accepted |

## Not done, or limits

- Entry point offsets are not used or validated. Decoding is sequential and realigns at
  each substream end. Validating offsets would need the emulation-prevention byte map.
- Inter pictures are still not checked (placeholder inter). The tile check on merge and
  AMVP neighbours is implemented but not pixel-verified.
- Dependent-slice and WPP-with-slices behaviour is verified only through the MD5 of intra
  pictures in the accepted streams. The conformance table covers WPP_A..F, DSLICE_A..C,
  ENTP_A..C, SLICES_A, TILES_A/B and the others, all exact.
