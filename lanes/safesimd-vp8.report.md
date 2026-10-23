# lane/safesimd-vp8: safe SIMD, #![forbid(unsafe_code)]

## Outcome

`ec-vp8` contains no `unsafe` code. `crates/ec-vp8/src/lib.rs` sets `#![forbid(unsafe_code)]`.
A grep of the crate for `unsafe` finds only that attribute line.

## Approach

The six-tap and bilinear SIMD kernels in `src/mc.rs` were `unsafe fn` with
`#[target_feature(enable = "avx2")]`, dispatched behind a hand-written
`avx2_supported()` check. They are now:

- **Kernels**: safe `#[target_feature(enable = "avx2")]` functions. The arithmetic
  intrinsics are safe inside them on rustc 1.99. No raw pointers remain.
- **Loads and stores**: `bytemuck::pod_read_unaligned` and `bytemuck::cast` on
  bounds-checked slices. The 8-byte loads used by the bilinear path read only the
  low 8 bytes that `_mm_cvtepu8_epi16` consumes, so the output is unchanged.
- **Dispatch**: the `multiversion` crate (0.9). `sixtap_2d` and `bilinear_2d` are
  `#[multiversion(targets("x86_64+avx2"))]` functions. Inside them `match_target!`
  selects, at compile time, the AVX2 arm (which calls the kernels) or the scalar
  reference. That arm is the only one that compiles to kernel calls, so the calls
  are safe under the AVX2 target feature. No `unsafe` is written in ec-vp8.
- **Attribute note**: the `#[multiversion::target_cfg]` attribute did not work
  inside the function body on this toolchain (attributes on expressions are
  unstable), so `match_target!` is used instead. `archmage` was not needed.
- The SSE2-only path was not needed: the scalar baseline clone already covers
  non-AVX2 hosts.

Dependencies added to `crates/ec-vp8/Cargo.toml`: `bytemuck = "1"`,
`multiversion = "0.9"`. `Cargo.lock` is updated.

Doc comments and redundant module-level `#![deny(unsafe_code)]` attributes in
`intra.rs` and `transform.rs` were removed.

## Test counts (`EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-vp8`, host a)

| Binary | Baseline (main) | Lane |
|---|---|---|
| lib unit tests | 57 passed | 57 passed |
| test binaries (4, 4, 0, 4, 2, 3 passed) | all ok | all ok |

No failures in either tree. The two SIMD-vs-scalar equivalence tests were rewritten
as `dispatched_sixtap_matches_the_scalar_reference_everywhere` and
`dispatched_bilinear_matches_the_scalar_reference_everywhere`. They run the
dispatched predictors against the scalar reference over all block shapes and phase
pairs, at two origins, on two planes.

## Benchmark (decode throughput, host a, same session)

Clip: `ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 10 -c:v libvpx -b:v 4M -g 60`
(300 frames, 1920x1080). Measured with `crates/ec-vp8/examples/throughput`, 5 rounds per run,
three runs each.

| Tree | Run 1 | Run 2 | Run 3 | Median |
|---|---|---|---|---|
| Baseline (main) | 91.3 MP/s | 99.9 MP/s | 96.3 MP/s | 96.3 MP/s |
| Lane | 96.1 MP/s | 98.4 MP/s | 96.7 MP/s | 96.7 MP/s |

The lane is within noise of the baseline (+0.4% at the median). The budget was 5%.

## Clippy

`cargo clippy --workspace --all-targets -- -D warnings` on host a: exit 0, no warnings.
