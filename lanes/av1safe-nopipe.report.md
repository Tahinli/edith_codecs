# av1safe-nopipe: pipelined post-filter removed

## Outcome
Partial. The per-superblock-row filter pipeline is deleted and the whole-frame filter path is now the only path. The grep target is NOT met: `post_filter.rs` still has the whole-frame CDEF `unsafe` (`cdef_rows`, `Shared::get`, lines ~61-66). `frame_tile.rs` has no `unsafe` left. The bench step was not run (see Bench).

## Approach
- Removed `PipeQ`, `PipeSync`, `UnwindAbort`, `snap_band`, `PipeCfg`, `LrCfg`, `LrPlane`, `PipeGuard`, `pipe_run`, `pipe_run_lr`, `wave_rows_complete`, `pipe_filters_ok`, the `PIPE_FRAMES`/`PIPE_WHOLE`/`PIPELINE_TAKEN`/`LR_PIPELINE_ROW0` statics and their accessors.
- `EC_AV1_PIPE_FILTERS` knob gone with `pipe_filters_ok`.
- `frame_tile.rs`: pipe install site, the per-row `p.note` call, and the `pipelined` branches in the tail removed; the whole-frame deblock, CDEF, LR and snapshot run unconditionally.
- Dead helpers removed: `deblock_span`, `snap_alloc`, `Shared::from_ptr` (par.rs, additive-only rule noted), `timeline::note_pipe`, the `pipe=` field of the wave-stats line in `decode/mod.rs`.
- Stale pipeline comments rewritten in deblock.rs, inter_shapes.rs, counters.rs, restoration.rs, decode/mod.rs.
- `fault` site `filter` removed (it lived in the pipeline band loop); `tests/worker_panic.rs` no longer arms it.
- `Cargo.lock`: adds `cpu-time` (already in the base commit's Cargo.toml; the lock was out of date).

## Files
crates/ec-av1/src/decode/post_filter.rs, decode/frame_tile.rs, decode/coeffs.rs, decode/deblock.rs, decode/mod.rs, decode/inter_shapes.rs, decode/counters.rs, restoration.rs, timeline.rs, par.rs, fault.rs, stream/tests/t02.rs, tests/worker_panic.rs, Cargo.lock.

## Remaining unsafe
- `decode/post_filter.rs` `cdef_rows` (whole-frame CDEF, used by `apply_cdef`): `Shared::new` + `Shared::get` in the band closure. Removing it needs `cdef_band` (decode/replay.rs) to index row-offset slices instead of absolute full-plane indices, plus `wrote` banding. That is a kernel rewrite and was not done here.

## Tests
- Deleted: `stream::tests::t02::a_444_sb128_witness_lr_pipeline_flushes_ru_row_0_pixel_exact` (pipe-vs-serial witness; only existed to prove the pipeline ran).
- Full gate (`EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-av1 --no-fail-fast -- --test-threads=4`, clippy `-D warnings` first, log `target-logs/nopipe-gate2.log`):
  - lib: 817 passed, 0 failed, 49 ignored (base 818 minus the one deleted test)
  - other binaries: all ok, 0 failed (worker_panic 1 passed, aom 1 passed, plus the rest)
  - clippy `--all-targets -D warnings`: clean
- Earlier run without `--test-threads=4` was SIGKILLed in the lib binary; not used.

## Bench
Not run. A base-vs-lane comparison needs the base decode_bench on the same host while the coordinator's base gate is running, which would skew timings; no decode_bench numbers are reported.
