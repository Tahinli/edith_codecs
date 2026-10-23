# lane/safesimd-prores report

Goal: `ec-prores` contains no `unsafe` code and sets `#![forbid(unsafe_code)]`. Output stays bit-exact and decode speed stays within about 5%.

## Result

- `crates/ec-prores/src/lib.rs`: `#![deny(unsafe_code)]` became `#![forbid(unsafe_code)]`. `grep -rn unsafe crates/ec-prores` finds nothing except the attribute.
- `crates/ec-prores/src/idct.rs`: the SSE2 IDCT is now safe code. Its only `allow(unsafe_code)` is gone.

## Dispatch approach

The spike's options were tried in this order:

1. **Plain `#[cfg(target_arch = "x86_64")]` fn with safe intrinsics, no dispatch.** Rejected. On rustc 1.99 SSE2 is the x86_64 baseline, but a safe intrinsic call still needs `sse2` listed in the caller's `#[target_feature]`. The build error was E0133 ("the sse2 target feature being enabled in the build configuration does not remove the requirement to list it").
2. **`multiversion`.** Rejected. The generated default clone calls the intrinsics without the feature, which is E0133 under `forbid(unsafe_code)`. The spike's `probe_a` shows the same error.
3. **`archmage` (used).** `X64V1Token` (SSE2) is the token. `summon()` gives the token at run time, and `#[arcane]` on `sse::idct_put_10` supplies the entry point. Every helper in the SSE2 module is `#[target_feature(enable = "sse2")]`, so calls from the arcane body are safe. Pinned `archmage = "=0.9.31-beta.1"`, as in the spike.

Other changes in the same pattern:

- Loads and stores have no raw pointers. The block load is `bytemuck::cast(*block)` and each row store is `copy_from_slice(&bytemuck::cast::<__m128i, [u8; 16]>(px))`. Added `bytemuck = "1"` (already in `Cargo.lock` through `wide`).
- The portable `wide` IDCT path (previously non-x86 only) is now the `summon() == None` fallback on every arch. The `#[cfg(not(target_arch = "x86_64"))]` attributes on it were removed.

## Tests (EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-prores, server b)

Identical before and after:

| binary | passed | failed | ignored |
|---|---|---|---|
| lib (incl. 200k-block SIMD vs reference, 10 and 12 bit) | 2 | 0 | 0 |
| encode | 13 | 0 | 0 |
| ffmpeg_parity | 1 | 0 | 0 |
| perf | 1 | 0 | 0 |
| robust | 5 | 0 | 1 |

Total: 22 passed, 0 failed, 1 ignored (the ignored robust test is ignored on main too).

`cargo clippy --workspace --all-targets -- -D warnings` on server b: exit 0.

## Benchmark (server b, same host, base and new run back to back)

`cargo test --release -p ec-prores --test perf -- --nocapture`, 1080p 4:2:2 HQ, best of 10 passes per run, 3 runs each. Host b is shared and noisy, so the per-run spread is large.

| | run 1 | run 2 | run 3 | mean |
|---|---|---|---|---|
| main, 1 thread (fps) | 62.5 | 62.4 | 60.8 | 61.9 |
| lane, 1 thread (fps) | 58.7 | 63.0 | 58.8 | 60.2 |
| main, 4 threads (fps) | 161.7 | 133.3 | 157.7 | 150.9 |
| lane, 4 threads (fps) | 138.2 | 147.7 | 154.4 | 146.8 |

Single-thread mean is -2.8% and the 4-thread mean is -2.7%, inside the ~5% budget. The best single-thread run is higher on the lane (63.0 against 62.5). The per-run spread is larger than the difference, so the difference is within noise.
