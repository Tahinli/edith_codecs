# lane/safesimd-h265: remove unsafe from ec-h265

## Approach

- `#![forbid(unsafe_code)]` in `crates/ec-h265/src/lib.rs` (was `deny` plus local allows).
- `src/simd_tx.rs` rewritten without raw pointers or `_mm_loadu_*`: `bytemuck` slice casts (`ld4`/`st4`, `ld8`/`st8`) feed the same AVX2 transform kernels. Public entries are `#[multiversion::multiversion(targets("x86_64+avx2"))]` and return `bool`/`Option` so the caller falls back to the scalar path when AVX2 is absent. Dispatch is via `multiversion::target::match_target!` as the tail expression.
- `src/enc_me.rs` (SAD) and `src/ctu.rs` (SATD): SSE2 kernels are `#[archmage::arcane]` fns taking an `X64V1Token` obtained from `X64V1Token::summon()`, with safe slice loads.
- `src/intra.rs`: the `unsafe fn predict_avx2` + `is_x86_feature_detected!` pair is replaced by a multiversion `predict_dispatch` wrapping the generic `predict_body`.
- `src/transform.rs`: call sites use the new `bool`/`Option` returns.
- `Cargo.toml`: adds `bytemuck = "1"`, `archmage = "=0.9.31-beta.1"`, `multiversion = "0.9"` (same versions as ec-prores/ec-vp8).
- `grep -rnw unsafe crates/ec-h265` is empty.

## Files

- crates/ec-h265/src/lib.rs
- crates/ec-h265/Cargo.toml
- crates/ec-h265/src/simd_tx.rs
- crates/ec-h265/src/transform.rs
- crates/ec-h265/src/enc_me.rs
- crates/ec-h265/src/ctu.rs
- crates/ec-h265/src/intra.rs
- crates/ec-h265/src/encoder.rs (comment only)
- Cargo.lock

## Verification (server)

- `cargo test --release -p ec-h265 --no-fail-fast` (EC_REQUIRE_FIXTURES=1): identical results before and after. Lib 96 passed / 1 ignored; all 23 test binaries pass with identical counts (no failures).
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- Output hashes identical: decode checksums 10-bit `4b8177f40520219c`, 8-bit `2bfe18f415b7408d`; encode sha 8-bit `29fc16181fe9`, 10-bit `e8be339172d4`.

## Benchmarks (server host a, base and new run back to back, 3 runs each)

Decode: `examples/decode_bench`, 120 frames of 1080p, ms/frame (lower is better).

| clip | base runs | new runs | base median | new median |
|---|---|---|---|---|
| 10-bit | 25.83, 33.42, 27.58 | 19.65, 35.20, 29.51 | 27.58 | 29.51 (+7%) |
| 8-bit | 29.56, 26.99, 27.41 | 26.40, 38.00, 29.43 | 27.41 | 29.43 (+7%) |

Encode: `scripts/bench_hevc_enc.sh` (N=30, THREADS=4, Default speed), fps (higher is better).

| depth | base runs | new runs | base median | new median |
|---|---|---|---|---|
| 8-bit | 11.92, 11.93, 12.72 | 10.43, 7.20, 7.89 | 11.93 | 7.89 (-34%) |
| 10-bit | 8.34, 6.51, 6.49 | 7.91, 6.07, 6.47 | 6.51 | 6.47 (-1%) |

Caveat: host a was shared with other lanes during these runs (load average 9 to 11, one "slot busy" wait). The run-to-run spread is large (for example, the new 8-bit decode run 2 is 38.0 ms against 26.4 ms in run 1). The 5% speed target is NOT confirmed by this run: the 8-bit encode median is lower by 34% and the decode medians are about 7% higher. A quiet-host rerun (interleaved base/new) is needed before accepting the speed claim.

## Follow-up: 8-bit encode regression investigation

Cause checked: the 8-bit-only encode path is the motion-search SAD (`sad_block`, `u8` planes). The first version of `sad_block_sse2` copied every 8-byte row into a `[u8; 16]` (W == 8) and SATD copied each 4-sample row into `[u16; 8]` per load. Both sit in inner loops. The fix replaces those copies with a single 8-byte load moved to the low lanes (`_mm_cvtsi64_si128`), with no temporary block copies. Dispatch stays one `summon()` per call (per block, not per 4x4). The transform/intra multiversion dispatch is one check per call and is unchanged in shape.

Tests after the fix (server host a): lib 96 passed / 1 ignored; encode 4 passed; enc8_bitstream_hash 1 passed; rdoq_cache_hash 3 passed; conformance_intra 2 passed. Hashes identical in every run below.

Interleaved benchmark, host a, one session, base and new alternating (base, new) x 3 rounds, release binaries built beforehand. Load was about 6 to 8 on 4 cores; hosts b and c were busy for the whole check.

| Benchmark | base r1, r2, r3 | new r1, r2, r3 | base median | new median | change |
|---|---|---|---|---|---|
| Decode 10-bit (ms/frame) | 32.58, 14.25, 24.42 | 24.74, 31.92, 20.49 | 24.42 | 24.74 | +1% |
| Decode 8-bit (ms/frame) | 32.97, 31.96, 21.01 | 19.44, 30.70, 20.44 | 31.96 | 20.44 | faster, within noise |
| Encode 8-bit (fps) | 11.50, 9.96, 14.05 | 14.06, 11.12, 15.54 | 11.50 | 14.06 | faster, within noise |
| Encode 10-bit (fps) | 8.85, 8.56, 9.97 | 16.10, 10.00, 10.87 | 8.85 | 10.87 | faster, within noise |

Conclusion: the earlier -34% 8-bit encode drop does not reproduce in interleaved runs, and the medians show no regression on any of the four benchmarks. The host noise (the same base binary ranged from 14 to 33 ms/frame for 10-bit decode) is larger than the 5% target, so this run cannot prove "within 5%" with confidence. The earlier single-block run should be treated as noise, not as a measured regression.
