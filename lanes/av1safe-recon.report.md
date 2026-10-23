# av1safe-recon report

## Outcome
Partial. The stale allows and both tests.rs unsafe blocks are gone. The reconstruction raw-pointer path in decode/mod.rs is unchanged and still has unsafe.

## Approach
- Removed `#[allow(unsafe_code)]` and `#[allow(unsafe_op_in_unsafe_fn)]` from the `cdef_simd` and `deblock_simd` module declarations. Neither module contains `unsafe`.
- tests.rs: the partials check calls `cdef::cdef_find_dir_partials_dispatch` (now `pub(super)`), the multiversion dispatch. The AVX2 filter check calls a new `cdef_filter_block_under_test` helper, a `multiversion` fn that runs `filter_block_avx2` on AVX2 and the scalar form otherwise.

## Files
- crates/ec-av1/src/decode/mod.rs
- crates/ec-av1/src/decode/cdef.rs (visibility only)
- crates/ec-av1/src/decode/tests.rs

## Remaining unsafe (mod.rs)
- `PredBuild` Send impl and the `transmute` to `'static` in `push_pred`: deferred closures capture non-Sync refs and run on wave workers. Needs owned captures and a lifetime on ReconOp.
- `ReconPlanes` Send/Sync and the unsafe in `push_op`, `inline_intra`, `inline_mc`, `wave_worker`, `WaveGuard::install`, `ReconInline::install`: raw plane pointers aliasing the parse stack's `&mut PlaneBuf` (inline path, default) and shared planes across wavefront workers. Needs planes threaded through about 186 `&mut PlaneBuf` sites.

## Tests
- clippy `-D warnings` on ec-av1 all-targets: clean.
- `cargo test --release -p ec-av1 --lib cdef -- --test-threads=4`: 9 passed, 0 failed, 2 ignored.
- Full lib run was stopped on host c at the coordinator's request, so no full-suite count.

## Bench
Not run. No hot-path code changed.
