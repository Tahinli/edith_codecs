# av1safe-cdef: whole-frame CDEF without par::Shared

## Outcome
`par::Shared` is no longer used by the whole-frame CDEF pass. The three
planes and the `wrote` byte map are carved into per-band `&mut` slices with
`split_at_mut`, and each band's `cdef_band` writes only through its own
slices. The `unsafe` blocks and `#[allow(unsafe_code)]` in
`post_filter.rs::cdef_rows` are gone. Output is unchanged.

## Approach
- `par.rs` (additive only): `carve_ranges` (element ranges to disjoint
  `&mut [T]`, via `split_at_mut`) and `run_bands_with` (like `run_bands`, but
  each band gets its own payload). `run_bands`, `run_bands_mut`, and `Batch`
  are untouched.
- `cdef_rows` computes, per plane, element ranges per band: band `k` owns
  plane rows from its first mi row (`(mi*4) >> ss_y`) to the next band's first
  row. The last band runs to the end of the buffer. This covers the padded
  tail when `mi_rows` is odd, since the trailing 8x8 block writes 4 rows past
  `mi_rows*4`. The `wrote` map is carved the same way by unit row (`mi/2`).
- `cdef_band` takes a `CdefBandDst` (the band's three plane slices, each
  slice's first row, the `wrote` slice and its first unit row) plus a
  `[CdefPlaneGeom; 3]` (stride, true width, true height). Reads stay on the
  `src_*` snapshots with absolute coordinates. Writes index
  `dst[oy - row0]`, and the `wrote` index is `mi_r/2 - wrote_row0`.
- `cdef_filter_block` (cdef.rs) takes `&mut [u16]` and `stride` instead of
  `&mut PlaneBuf`. The SIMD and scalar kernels are untouched; they already
  index `dst` from `oy`, so passing `oy - row0` is enough.

## Write-hazard check (task item 3)
The kernels (`cdef_filter_block_scalar`, `cdef_simd::filter_block_avx2`) never
read `dst`. Each writes exactly rows `oy..oy+bh` and columns `ox..ox+bw`.
Non-final bands are aligned to 2 mi (8 luma rows, 4 chroma rows), so every
block a band writes lies inside its own rows. The only overflow is the
trailing odd mi row of the frame, which the last band's slice covers. No
write reaches the neighbour band's rows, so no edge-row snapshot was needed.

## Files
- `crates/ec-av1/src/par.rs` (added `carve_ranges`, `run_bands_with`)
- `crates/ec-av1/src/decode/post_filter.rs` (`cdef_rows` rewritten)
- `crates/ec-av1/src/decode/replay.rs` (`cdef_band`, `CdefBandDst`, `CdefPlaneGeom`)
- `crates/ec-av1/src/decode/cdef.rs` (`cdef_filter_block` signature)

## Remaining unsafe
None in the CDEF pass. `grep -n 'Shared\|unsafe'` on `post_filter.rs` and
`replay.rs` returns nothing. `par::Shared` still exists for its other users.
`cdef_simd.rs` (the AVX2 kernel) is outside this task and was not changed.

## Tests (server, `EC_MEMGUARD_MAX=12G EC_AV1_AOM_DIR=... EC_REQUIRE_FIXTURES=1`, `--test-threads=4`)
- Targeted: `filter_stage_bytes_do_not_depend_on_the_thread_count` 1/0; the
  `cdef`-named lib tests 10 passed, 0 failed, 2 ignored (run alone).
- `--test aom_conformance`: 1 passed, 0 failed.
- Full gate (`cargo clippy -p ec-av1 --all-targets -- -D warnings` clean, then
  `cargo test --release -p ec-av1 --no-fail-fast`): lib 817 passed, 0 failed,
  49 ignored; aom_conformance 1/0; fuzz_regressions 5/0; real_library 0/0 (1
  ignored); worker_panic 2/0; yuv422_dav1d 1/0; doctests 0. Total 826 passed,
  0 failed, 50 ignored. The lib count is 817, which matches the base commit
  4a2682cd. The 818 in the common notes predates the nopipe merge.

## Bench (decode_bench, `t1080.obu`, default threads, host a, 3 alternating runs)
| run | base secs | lane secs |
|---|---|---|
| 1 | 16.933 | 19.214 |
| 2 | 18.629 | 16.358 |
| 3 | 14.078 | 16.679 |
| median | 16.933 | 16.679 |

Lane median is 1.5% faster than base (fps medians 17.72 base, 17.99 lane).
The spread within each tree is about +-20%, so the difference is noise and
there is no regression beyond the 3% threshold.
