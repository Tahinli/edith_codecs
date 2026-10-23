# lane hevcmc: HEVC motion compensation (8.5.3.3)

## Goal
Self-contained, tested sample prediction for `ec-h265`: fractional luma and chroma
interpolation, default and explicit weighted prediction, and a PU-level driver that
produces final clipped samples in all three planes. Generic over 8 to 10 bits, samples
as `u16` planes. Nothing else checks MC yet, so the tests are the main deliverable.

Base: `main` at `1b962d32`.

## What changed
- `crates/ec-h265/src/dec/mc.rs` (new, about 400 lines): interpolation and `predict_pu`.
- `crates/ec-h265/src/dec/weighted.rs` (new, about 230 lines): `ExplicitWeights`
  (derivation from `PredWeightTable`) and the weighted store.
- `crates/ec-h265/src/dec/mc_tests.rs` (new, the unit tests of `mc`): 17 tests.
- `crates/ec-h265/src/dec/mod.rs`: registers `mc` and `weighted`.

No existing code calls the new module yet; the placeholder inter path in `inter.rs` is
unchanged. Wiring MC into the decoder is for the next lane.

## Spec coverage
- 8.5.3.3.3.1 luma: 8-tap Table 8-11, shift1 = Min(4, BitDepth-8), shift2 = 6,
  shift3 = Max(2, 14-BitDepth). Full, horizontal, vertical and two-dimensional cases.
- 8.5.3.3.3.2 chroma: 4-tap Table 8-12 in eighth samples, same shifts, 4:2:0 only
  (`xInt = x/2 + (mv >> 3)`, `xFrac = mv & 7`).
- Reference clamping: every sample coordinate clamped to the plane, by gathering the
  source window once per block.
- 8.5.3.3.4.2 default weighting (uni and bi), 8.5.3.3.4.3 explicit weighting (uni and
  bi, including the `log2WD < 1` branch of the uni formula).
- 7.4.7.3 weight derivation: `LumaWeightLX = 2^denom + delta`, offsets scaled by
  `<< (BitDepth - 8)`, chroma denominator `luma + delta_chroma`, chroma offsets taken
  from `WeightEntry` (already derived by 7-56 in the parser).

## API
```rust
// mc.rs
pub struct PlaneRef<'a> { pub samples: &'a [u16], pub stride: usize, pub width: usize, pub height: usize, pub bit_depth: u32 }
pub struct PlaneMut<'a> { pub samples: &'a mut [u16], pub stride: usize, pub bit_depth: u32 }
pub type RefPicture<'a> = [PlaneRef<'a>; 3];       // Y, Cb, Cr
pub struct Block { pub x, pub y, pub width, pub height: usize }   // in samples of the plane
pub struct McScratch;                              // reusable buffers, McScratch::new()

pub fn predict_pu(
    refs: [&[RefPicture<'_>]; 2],                  // refs[list][ref_idx]
    motion: &Motion,                               // dec::cu_syntax::Motion (pred_flags, ref_idx, mv)
    weights: Option<&ExplicitWeights>,             // None = default weighted prediction
    pu: Block,                                     // luma rectangle; chroma is half of it
    out: &mut [PlaneMut<'_>; 3],
    scratch: &mut McScratch,
) -> ec_core::error::Result<()>;

pub fn interp_luma(plane: &PlaneRef, blk: Block, mv: [i32; 2], scratch: &mut McScratch, out: &mut [i32]) -> Result<()>;
pub fn interp_chroma(plane: &PlaneRef, blk: Block, mv: [i32; 2], scratch: &mut McScratch, out: &mut [i32]) -> Result<()>;
// out receives predSamplesLX at 14-bit precision, row-major.

// weighted.rs
pub struct Weight { pub w: i32, pub o: i32 }       // o already scaled by BitDepth-8
pub struct ExplicitWeights { pub log2_denom: [u32; 3], pub entries: [Vec<[Weight; 3]>; 2] }
impl ExplicitWeights {
    pub fn from_table(t: &PredWeightTable, bit_depth_luma: u32, bit_depth_chroma: u32) -> Result<Self>;
    pub fn lookup(&self, list: usize, ref_idx: usize, comp: usize) -> Result<Weight>;
}
pub fn store(dst: &mut PlaneMut, blk: Block, pred: [Option<&[i32]>; 2], explicit: Option<(u32, [Weight; 2])>) -> Result<()>;
```
Bad input (no list in use, a missing reference index, a rectangle outside its plane,
bit depth outside 8..=10, weight deltas outside -128..=127) returns an `Error`; no
panics on input-dependent paths.

## Tests
`crates/ec-h265/src/dec/mc_tests.rs`, 17 tests:
- Hand-computed impulse responses for every luma fraction (1, 2, 3) at 8 and 10 bits,
  every chroma eighth (1 to 7) at 8 bits, full-sample shifts, a vertical-only check, and
  a two-dimensional (2,2) case at 10 bits with the 4x4 result worked out by hand.
- Edge clamping at all four borders and all four corners, integer and fractional MVs
  far outside the picture, luma and chroma.
- Default uni and bi, 8 and 10 bits, including clipping at 0 and max.
- Explicit uni and bi by hand: negative weights, negative offsets, offsets that clip,
  and the 10-bit offset scaling.
- Weight derivation from a `PredWeightTable` (negative deltas, chroma offsets, default
  entries, 10-bit scaling, out-of-range rejection).
- Reference model: a per-sample transcription of 8.5.3.3 (its own copy of both filter
  tables, no shared code) compared against `predict_pu` on 10,000 random PUs. Fixed
  SplitMix64 seed. Sizes 4..=64 in steps of 4, with every eighth case forced to 8x4 or
  4x8. Random reference content, MVs from far outside the picture to inside, uni and
  bi, default and explicit weights, both bit depths, all three planes.
- Benchmark (`bench_1080p_bipred_8x8_frame`, timing only).

## Verify
Run through `scripts/remote-run.sh hevcmc -- ...` from `~/.cache/wt/hevcmc`.

1. `cargo clippy -p ec-h265 --all-targets -- -D warnings`: FAILS, on a lint in a file this
   lane does not touch: `crates/ec-h265/tests/cu_syntax.rs:272` and `:276`
   (`clippy::collapsible_if`, two errors, from main at `7537cdc4`). With the current
   toolchain (rust 1.98 clippy) this is pre-existing. Diagnostic run with only that lint
   allowed (`-A clippy::collapsible_if`): clean, no warnings in `dec/mc.rs`,
   `dec/weighted.rs`, `dec/mc_tests.rs` or any other ec-h265 target.
2. `cargo test -p ec-h265 --release`: PASS. Lib 49 passed (17 of them new),
   integration suites 15, 3, 4, 5, 4, 3, 13, 3 passed, 0 failed.
   The `cu_syntax` test target takes about 73 s and one suite about 325 s; these are
   existing tests.

## Benchmark
Release, one 1920x1080 frame of 8x8 bi-predicted PUs (32,400 PUs, luma plus 4:2:0 chroma,
10-bit, two references, default weighting): **87.0 ms per frame** (about 2.7 us per PU).
Not SIMD. Allocation-free after the first call (`McScratch` is reused).

## Findings for other lanes
- `ec-h265-syntax/src/slice.rs`, `parse_pred_weight_table`: `half_range` is computed as
  `1 << (bit_depth_chroma_minus8.min(8) + 7)`. Equation 7-56 uses
  `wpOffsetHalfRangeC = 1 << 7` when `high_precision_offsets_enabled_flag` is 0 (the
  non-RExt case), which is 128 at every bit depth. For Main10 the parser uses 512, so the
  derived `ChromaOffset` is wrong for 10-bit streams with chroma weights. Not fixed here
  (another lane's file). The parser stores the derived offset and not the raw delta, so
  the fix is in the parser.
- `DecodedPicture` (`dec/mod.rs`) still stores `u8` planes. This module takes `u16`
  plane slices plus a stride, so the decoder's storage needs to change (or convert at the
  boundary) when MC is wired in.

## Left
- Wiring MC into `inter.rs` (replacing the placeholder copy) and the reference picture
  lists; this lane only provides the module.
- SIMD (the `wide` crate) once the scalar path is in use.
- Explicit weights for `high_precision_offsets_enabled_flag` (RExt) are not modelled.
