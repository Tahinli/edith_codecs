# lane/safesimd-av1-mc: safe SIMD for ec-av1 mc.rs and transform.rs

## Approach
- `crates/ec-av1/src/mc.rs`: every `unsafe fn`/`unsafe {}` and `#[allow(unsafe_code)]` removed (33 -> 0). Kernels are safe `#[target_feature]` fns. Loads and stores go through slices plus bytemuck. Dispatch uses `multiversion` with `match_target!` (targets `x86_64+avx2`, `x86_64+sse4.1`), keeping the AVX2, SSE4.1, SSE2 and scalar tiers. The raw-pointer `from_raw_parts` casts became `bytemuck::cast_slice`.
- `crates/ec-av1/src/transform.rs`: `unsafe` removed (28 -> 0). The trait `btf`/`had` methods became a safe `Kern<T>` trait. The DCT network is generic over it. The AVX2 path is one safe `#[target_feature(enable = "avx2")]` function, `dct_x8_avx2`, with `#[inline(always)]` network functions. Dispatch is a multiversion `column_pass_dct_dispatch`.
- `crates/ec-av1/Cargo.toml`: added `bytemuck = "1"` and `multiversion = "0.9"`.
- `lib.rs` is untouched: no `forbid(unsafe_code)` yet, per the lane brief. The two files contain no `unsafe` token, so they would compile under it.

## Tests (EC_REQUIRE_FIXTURES=1, release)
| target | main (base) | lane |
|---|---|---|
| `--lib` | 818 passed, 0 failed, 49 ignored | 818 passed, 0 failed, 49 ignored |
| all other ec-av1 targets | 8 passed, 0 failed, 1 ignored | 8 passed, 0 failed, 1 ignored |
| `aom_conformance` (`EC_AV1_AOM_DIR=~/fixtures/av1-aom`) | 1 passed | 1 passed |
| total | 827 passed, 0 failed, 50 ignored | 827 passed, 0 failed, 50 ignored |

Note: without `EC_AV1_AOM_DIR`, `aom_conformance` fails on both trees (it panics under `EC_REQUIRE_FIXTURES`). The counts above use the variable.

`cargo clippy --workspace --all-targets -- -D warnings`: passes on the lane (host c).

## Peak RSS (`--lib`, 12G memguard cap, RSS summed over the test binary, sampled every 5 s)
- main: 3,966,900 KB (about 3.97 GB)
- lane: 3,750,740 KB (about 3.75 GB), 5% below main

The earlier SIGKILLs at the 6G cap on host b were from host load, not a memory regression. Both trees peak under 4 GB.

## Decoder speed and output (`decode_bench t1080.obu`, `EC_AV1_THREADS=1`, host b, built once per tree, runs interleaved)
| run | main fps | lane fps |
|---|---|---|
| 1 | 21.84 | 22.68 |
| 2 | 22.34 | 22.46 |
| 3 | 23.75 | 23.88 |
| median | 22.34 | 22.68 (+1.5%) |

Decoded output (`decode_bench --raw | md5sum`): `3adc24bb024913aab921dc8575b8be8b` for both trees, which matches ffmpeg.

Host b was shared with other lanes, so run-to-run spread is wide (about 10%).
