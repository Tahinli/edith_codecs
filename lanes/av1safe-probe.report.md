# av1safe-probe: safe allocation counting for examples and tests

## Outcome
- `crates/ec-av1/examples/alloc_probe.rs` has no `unsafe`. It uses `stats_alloc` (totals) and `tracking-allocator` (size histogram and sampled backtraces).
- Same allocator-based `unsafe impl GlobalAlloc` removed from three test targets: ec-h264, ec-image, ec-opus.
- ec-opus `libc::clock_gettime` (unsafe) replaced by `cpu_time::ThreadTime` from the `cpu-time` crate (new dev-dependency; libc dev-dependency removed).
- Remaining `unsafe` outside `crates/ec-av1/src`: only `std::env::set_var/remove_var` in tests of ec-aac and ec-vp9 (see below).

## Approach
- `stats_alloc` (0.1.10, `nightly` feature only to get a const `StatsAlloc::new`, which a `#[global_allocator]` static needs on stable) counts `allocations` and `bytes_allocated`. Reallocs are counted separately, as before.
- `tracking-allocator` (0.4.0) wraps `System` and calls a tracker on every allocation. The tracker builds the size histogram and samples a backtrace every 512th allocation.
- The tracker skips reallocs (detected by a bump of `stats.reallocations`), as the old hand-written allocator did.
- Output format is unchanged (`shown=`, `allocs=`, `size 2^i :`, sampled trace lines).

## Files
- crates/ec-av1/examples/alloc_probe.rs
- crates/ec-av1/Cargo.toml (dev-deps stats_alloc, tracking-allocator)
- crates/ec-h264/Cargo.toml, crates/ec-image/Cargo.toml, crates/ec-opus/Cargo.toml (dev-dep tracking-allocator; ec-opus drops libc)
- crates/ec-h264/tests/conformance.rs, crates/ec-image/tests/fuzz.rs, crates/ec-opus/tests/conformance.rs
- Cargo.lock

## Remaining unsafe (not fixed, outside ec-av1/src)
- crates/ec-aac/tests/{reelh_per_frame.rs, obs_real_content.rs, sbr_real_library/chunk_a.rs, chunk_b.rs, chunk_c.rs}
- crates/ec-vp9/tests/{scratch_sweep.rs, scratch_lfdump.rs}
- Reason: these set env knobs (`EC_AAC_SBR_*`, `EC_VP9_*`) that library code reads at run time. Removing the `unsafe` needs the knobs passed as parameters, which is a change in those crates' src.
- ec-va / ec-hw: no `unsafe` in code lines (only doc-comment mentions). They are not buildable on the servers (cros-libva needs system libva), so they were excluded from clippy there.

## Tests
- `cargo clippy --workspace --exclude ec-va --exclude ec-hw --exclude ec-bench --all-targets -- -D warnings` on server: clean (rc 0).
- `cargo test --release -p ec-image --test fuzz`: 19 passed, 0 failed.
- `cargo test --release -p ec-opus --test conformance steady_state`: 1 passed.
- `cargo test --release -p ec-h264 --test conformance steady_state`: 1 passed.

## Probe output comparison (fixtures/av112bit-key.obu)
- Format is identical. Headline numbers are not byte-identical to the old allocator:
  - old: allocs 1235 to 1252, bytes 816 KB to 860 KB across 3 runs (the old probe itself varies run to run).
  - new: allocs 1221 to 1233, bytes ~1.37 MB across 3 runs. The bytes gap (~+500 KB) is consistent and was not attributed.
- Known semantic differences: the histogram and backtrace sampling are driven by the tracker, so allocations made inside the probe's own backtrace capture are not in the histogram, and tracker-side ordering under multi-threaded decode may classify reallocs differently. The old probe also varied between runs.
