# lane/safesimd-av1-filters: safe SIMD for the AV1 filters (partial)

## Outcome

Partial. The SIMD kernels and their dispatch in the AV1 filter lane are free of `unsafe`.
The parallel plane-sharing sites still use `crate::par::Shared` (an `unsafe` API in
`par.rs`, outside this lane), so the crate cannot yet take `#![forbid(unsafe_code)]`. That
attribute was not added, as instructed.

## Approach

- Kernels in `decode/cdef_simd.rs`, `decode/deblock_simd.rs`, `restoration.rs` (`sgr_simd`)
  and `film_grain.rs` (`simd`): safe `#[target_feature(enable = "avx2")] fn`. No raw
  pointers. Loads and stores are bounds-checked slices converted with `bytemuck::cast`.
  Gathers became checked scalar table lookups into an array, then a cast back to a vector.
- Dispatch: the `simd_level()` / `is_x86_feature_detected!` gates and the `unsafe` call
  sites are replaced by `#[multiversion::multiversion(targets("x86_64+avx2"))]` with a
  `match_target!` expression in the body. Sites: `cdef_find_dir_partials_dispatch`,
  `cdef_filter_block`, `filter_edge16` (deblock), `ab_row` (restoration), and the film-grain
  row wrappers and test helpers.
- `Cargo.toml` (ec-av1) gains `bytemuck = "1"` and `multiversion = "0.9"`; `Cargo.lock` updated.

## Files

- `crates/ec-av1/Cargo.toml`, `Cargo.lock`
- `crates/ec-av1/src/decode/cdef_simd.rs`, `decode/cdef.rs`
- `crates/ec-av1/src/decode/deblock_simd.rs`, `decode/deblock.rs`
- `crates/ec-av1/src/restoration.rs`
- `crates/ec-av1/src/film_grain.rs`

## Remaining `unsafe` (all `par::Shared` or raw-pointer plane sharing)

These are left for the plane-threading lane, which owns `par.rs`:

- `decode/post_filter.rs`: `unsafe impl Send` for `PipeCfg` (line ~149) and `LrCfg` (~418);
  `Shared::get` calls at ~167-169, ~292-294, ~613-617; the raw `idx_grid` / `lr_grid`
  pointers, read through `from_raw_parts` at ~169 and `&*cfg.lr_grid` at ~294.
- `decode/deblock.rs`: `Shared::get` at ~514-515 and ~591-592.
- `restoration.rs`: `Shared::get` at ~1599-1600.
- `film_grain.rs`: `Shared::get` at ~1524-1525.
- `decode/mod.rs` (not this lane): `#[allow(unsafe_code)]` on the `deblock_simd` module.

## Tests

`EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-av1 --no-fail-fast` on host c, lane branch:
lib 818 passed, 49 ignored; 6 further binaries pass; 826 passed and 50 ignored in total.
The one failure, `aom_vectors_match_per_frame_md5`, also fails on the base tree. The
coordinator attributes it to an unset fixture env var (main now falls back to
`fixtures/av1-aom`, commit 9f645d86). Main's lib is 818 passed, 0 failed, 49 ignored
(from an independent run); the base lib rerun was stopped and not repeated.

`cargo clippy --workspace --all-targets -- -D warnings` on host c: exit 0.

## Benchmark

AV1 decode, `examples/decode_bench` on `~/.cache/av1decbench-clip/t1080.obu` (1080p, 300 frames),
default threads, host c, 3 runs each, alternating base and lane. Base is `main` at `c3430e71`.

| Tree | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Base (main) | 12.06 fps | 13.13 fps | 12.65 fps | 12.65 fps |
| Lane | 11.52 fps | 12.43 fps | 13.40 fps | 12.43 fps |

The lane is -1.7% at the median, inside the 5% budget. The host was shared with other lanes,
so absolute fps is low. Film grain was not benchmarked separately: no clip with grain was
available.
