# lane hevc10simd2: ec-h265 encode speed, second pass

## Result: criterion NOT met (30 fps at 1080p on 4 threads); +15 to 25% bit-identical

Bench: `scripts/bench_hevc_enc.sh` (synthetic testsrc2 1080p, 30 frames, keyint 8, QP 30, 4 threads, whole process incl. yuv read and recon write; `enc=` is encode_sequence only).
Host c (AMD EPYC-Rome 4 vCPU, shared; load noted per run, other lanes running).

| depth | before fps (load 0.8-2.5) | after fps (load 4.7) | sha1 before = after |
|---|---|---|---|
| 8-bit | 10.31, 12.91, 10.50 | 12.55, 13.44, 12.74 | 29fc16181fe9 |
| 10-bit | 8.05, 8.19, 8.75 | 10.28, 10.53, 10.90 | e8be339172d4 |

Host c is about 40% slower than the host a used by the previous lane (18.9 / 14.6 fps), so these are not comparable with that report.
Single thread interleaved A/B (T1 only, host c): 8-bit 4.8-5.2 -> 5.6-5.7 fps, 10-bit 3.8-4.0 -> 4.6-4.8 fps.

## Changes (all bit-identical)
- `simd_tx.rs` + `transform.rs`: AVX2 (runtime detected, scalar fallback) forward/inverse DCT/DST 4..32 across lines, pruned inverse, quant and dequant. Kernel speedups 2.5-3.5x. Tests vs scalar for all sizes, DST, bit depths 8/10, extreme inputs.
- `intra.rs`, `ctu.rs`: smoothed reference cache per block (no per-mode filter_refs copy), no Refs copy when unfiltered, AVX2 multiversioned planar/DC/angular. Test vs plain path (35 modes, n 4..32, u8/u16).
- `enc_p.rs`: `code_tb` on stack buffers and integer SSD (no per-TB Vec allocs).
- `scripts/bench_hevc_enc.sh`, `EC_H265_TIMING` hook in `examples/hevc_gop.rs`.

## Gates (host c)
`EC_REQUIRE_FIXTURES=1 cargo test -p ec-h265 --release` all pass, pins unchanged; clippy `-D warnings` clean on each lane branch before merge.

## Findings / not done
- Profile is flat (single thread): ME cost+SAD ~15%, transforms ~20% (now much lower), intra ~9%, code_tb 5%, memmove 5%, memset 2%, mc ~8%, CABAC/residual ~8%. perf call graphs are blocked (perf_event_paranoid), so memmove/memset callers were not traced.
- Not done: ME cost/half_band AVX2, mc interpolation (src/dec/mc.rs, decoder file owned by another lane), deblock/SAO SIMD, residual/CABAC. Needing ~2x more, these plus a coarser RD search (not bit-identical) are required.
- Scaling: GOP-parallel at 30 frames is 4 GOPs (8,8,8,6), 2.9x on 4 vCPU (likely SMT siblings); about 0.4-0.6 s of the process is harness I/O and copies outside encode_sequence.

## Rebase onto main (hevcrext, cc79b8f0)
intra.rs conflict resolved: the cache and AVX2 path now take RExt's `PredFlags` (`predict_cached_ext`, `predict_filtered`); smoothing, strong filter, edge and no-boundary flags follow `predict_ext` exactly. The encoder-side cache is only used for 4:2:0 Main/Main10 and is keyed per block.
Gate (host c): clippy `-D warnings` exit 0; `EC_REQUIRE_FIXTURES=1 cargo test -p ec-h265 -p ec-h265-syntax --release --no-fail-fast` exit 0, 245 passed, 0 failed; encoder hashes unchanged (8-bit 29fc16181fe9, 10-bit e8be339172d4). fetch-vectors: one set (h264-jvt-hi444) failed on network, all hevc sets present.
Post-rebase bench, host c at load ~5: 8-bit 13.3-15.3 fps, 10-bit 12.1-12.3 fps.
