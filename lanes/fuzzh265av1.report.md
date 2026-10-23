# lane fuzzh265av1: cargo-fuzz targets for ec-h265 and ec-av1

Goal: DONE.md shared gate 2 (one cargo-fuzz target per untrusted entry point) for ec-h265 and ec-av1,
then the follow-up: make the harnesses fast enough for a 1 CPU-hour gate, minimise the big regression
input, run the 1 h gate on all four targets, fix every crash.

Branch `lane/fuzzh265av1`, reset to main `0bf279bd` for the follow-up.

## Layout
- `crates/ec-h265/fuzz/` (package `ec-h265-fuzz`, own `[workspace]`, `libfuzzer-sys 0.4`):
  - `parse_params_slice`: every VPS, SPS, PPS parsed with `ec-h265-syntax`; each slice header parsed against
    the latest SPS and PPS that parsed. No caps (parser only).
  - `decode_annexb`: the stateful `ec_h265::Decoder`, NAL by NAL, then flush.
- `crates/ec-av1/fuzz/` (package `ec-av1-fuzz`):
  - `parse_obu`: raw OBU stream through `Av1Parser::parse_temporal_unit`, plus `ObuHeader`, `read_leb128`,
    `SequenceHeader::parse`. No caps (parser only).
  - `decode_stream`: `ec_av1::stream::decode_stream_with`.
- Seeds: `seed.sh` copies `seeds/` into the corpus dirs (corpus is gitignored).
  - h265 `seeds/`: four 64x64 libx265 encodes made with ffmpeg (`-x265-params` for short GOPs), 3.9 to 6.5 KB each
    (`h265-64-bframes`, `-ctu16`, `-tiles`, `-10bit`).
  - av1 `seeds/`: seven tiny pins copied from `crates/ec-av1/fixtures` (62 to 6441 bytes, about 15 KB total).

## Harness caps (follow-up)
Profile, release build, host c: an ec-h265 iteration was almost all slice/picture decode (`push_decode`);
split and PS parse were microseconds. The old 1920x1088 cap made one 4216x528 input cost about half a second.
Caps now:

| target | caps |
|---|---|
| decode_annexb | luma 64x64 (one CTB), DPB `sps_max_dec_pic_buffering_minus1 < 6`, at most 8 pictures and 16 slice NAL units per input, `set_threads(1)` |
| decode_stream | luma 64x64, at most 8 frame OBUs per input (the stream is cut before the 9th), `decode_stream_with_threads(.., 1, ..)` |

Oversized SPS / sequence headers are not fed to the decoder. The regression tests replay their inputs
with NO caps, so a capped-out input is still checked in full.

Trade-off: at 64x64 the fuzzer no longer reaches large-picture paths (tiles, multi-CTB pictures, 4:2:2 at
size). Those paths are covered by the conformance and AV1 vector tests, not by the fuzzer.

## Exec/s before and after
| target | before | after |
|---|---|---|
| ec-h265 decode_annexb | 1.5 exec/s (459 runs in 300 s, 1920x1088 cap) | 120 s probes: 100 to 116 exec/s; 1 h runs: 148 exec/s (earlier tree), 112 exec/s (busy host), 118 exec/s (final tree, 425673 runs) |
| ec-av1 decode_stream | 54 exec/s (16234 runs in 300 s, 1920x1088 cap) | 190 exec/s (120 s probe at 64x64); 1 h final run: 193 exec/s |
| ec-h265 parse_params_slice | 164 exec/s | 2251 exec/s (1 h, final tree) |
| ec-av1 parse_obu | 7600 exec/s | 2389 exec/s (1 h) |

Target was at least 200 exec/s for both decoders. Only the AV1 decoder reached it (about 190 exec/s); the
ec-h265 decoder is at 110 to 150 exec/s. The remaining cost was not profiled further (no `perf` on the server).

## Regression input minimisation
- `crates/ec-h265/tests/data/fuzz/decode_annexb-escape-shift.bin`: the 85452-byte input was minimised with
  libFuzzer `-minimize_crash=1` (on the built target, not `cargo fuzz tmin`, which barely shrank it:
  85452 to 83104 bytes). Result: 43 bytes, still reproduces the shift overflow. The 85 KB file is gone.
- The later inputs (listed below) are the fuzzer's own crash files, not minimised.

## Crashes found and fixed
Each has a replay test in the crate's `tests/fuzz_regressions.rs`.

ec-h265 decoder and syntax (`decode_annexb` and `parse_params_slice`):
1. `residual.rs`: escape prefix of `coeff_abs_level_remaining` unbounded, `1 << k` overflow. Fixed: prefix capped
   at `MAX_ESCAPE_K = 16`. Input `decode_annexb-escape-shift.bin` (43 bytes).
2. `ps.rs`: SPS picture size not a multiple of MinCbSize (7.4.3.2.1), so edge CUs overran the planes
   (`recon.rs`). Fixed: rejected. Input `decode_annexb-sps-not-mincb.bin` (43 bytes).
3. `ps.rs` / `cu_syntax.rs`: MaxTbLog2SizeY above 5 or above the CTB, and MinTb >= MinCb, gave `5 - log2`
   underflow. Fixed: 7.4.3.2.1 check in the SPS. Input `decode_annexb-tb-size-overflow.bin`.
4. `cu_syntax.rs`: SliceQpY below -QpBdOffsetY gave a negative `qP % 6` into `LEVEL_SCALE` (`transform.rs`);
   the CU QP wrap also used `%` on a possibly negative value. Fixed: range check, `rem_euclid`. Input
   `decode_annexb-slice-qp-range.bin`.
5. `cu_syntax.rs`: `diff_cu_qp_delta_depth` deeper than the CTB underflowed the quantization group size. Fixed:
   range check. Input `decode_annexb-cu-qp-depth.bin`.
6. `ps.rs`: PCM sample bit depth above the coded bit depth underflowed `BitDepth - PcmBitDepth`. Fixed: 7.4.3.2.1
   check. Input `decode_annexb-pcm-depth.bin`.
7. `cu_syntax.rs`: Log2ParMrgLevel above the CTB (shift overflow in `merge.rs`). Fixed: 7.4.3.3.1 check. Input
   `decode_annexb-par-mrg-level.bin`.
8. `slice.rs`: `num_ref_idx_lX_active_minus1` from the PPS default was not range-checked, so
   `pred_weight_table` allocated 2.6 GB. Fixed: the 0..=14 check now covers PPS defaults too. Test in
   `crates/ec-h265-syntax/tests/fuzz_regressions.rs` (input `parse_params_slice-pred-weight-oom.bin`).
9. `slice.rs`: `delta_poc_msb_cycle_lt` accumulation overflowed. Fixed: saturating add. Input
   `parse_params_slice-delta-msb-overflow.bin`.
10. `ps.rs`: `delta_poc_s0/s1_minus1` past 2^15 - 1 overflowed the POC sums. Fixed: range check. Input
    `parse_params_slice-rps-delta-overflow.bin`.
11. `ps.rs`: `abs_delta_rps_minus1` past 2^15 - 1 (inter RPS). Fixed: range check. Input
    `parse_params_slice-inter-rps-overflow.bin`.
12. `slice.rs`: `luma_log2_weight_denom + delta_chroma_log2_weight_denom` overflowed. Fixed: luma range check and
    saturating sum. Input `parse_params_slice-weight-denom-overflow.bin`.
13. `slice.rs`: `num_long_term_sps + num_long_term_pics` overflowed. Fixed: bound check (sps count, and 32 total).
    Input `parse_params_slice-long-term-count.bin`.
14. `slice.rs`: entry point offset `+ 1` overflowed for all-ones values. Fixed: saturating. Input
    `parse_params_slice-entry-point-overflow.bin`.

ec-av1 (`decode_stream`, plus the AV1 header parser):
15. `show_existing_frame` in `OBU_FRAME` (earlier round). Fixed in `ec-av1-syntax`. Input
    `decode-stream-obu-frame-show-existing.bin` (668 bytes).
16. `ec-av1-syntax/src/frame.rs`: film grain scaling points not strictly increasing (6.8.20) gave a zero-width
    segment, so `build_scaling_lut` divided by zero. Fixed: rejected in the header parser. Input
    `decode_stream-film-grain-points.bin` (772 bytes).
17. `ec-av1/src/decode.rs`: CfL AC buffer shorter than the transform block, `ac_q3[idx]` past the end in
    `reconstruct`. Fixed defensively: a missing AC sample adds nothing (`unwrap_or(0)`). Input
    `decode_stream-cfl-ac-size.bin`. Root cause NOT established: a conforming stream may never reach this, but it
    was not proven. Needs an AV1 owner to check the 4:2:2 CfL source-size arm.
18. `ec-av1/src/decode.rs`: a palette/intrabc prediction override whose length is not the block's (`reconstruct`
    indexed the 4x4 override with an 8x8 block). Fixed defensively: an override of the wrong size is ignored and
    the block is predicted from edges. Input `decode_stream-palette-pred-size.bin` (33 bytes). Same caveat as 17:
    the override is supposed to be stale only in corrupt streams; not proven.

## Gates (final tree)
Run through `scripts/remote-run.sh`. Hosts: the reference YUVs (`~/refyuv/hevcinter`, 3.4 GB) exist on host a only.

| command | result |
|---|---|
| `EC_H265_REF_YUV_DIR=$HOME/refyuv/hevcinter cargo test -p ec-h265 -p ec-h265-syntax --release` (host a) | NOT RE-RUN on the final tree (host a stayed busy). Last full run on host a, before the final five slice-header range checks: exit 0, 18 test groups ok |
| conformance: `cargo test -p ec-h265 --release --test conformance_intra -- --nocapture` (host a) | last run on host a: `total: 9203 pictures checked, 9203 exact, 147/147 streams accepted`; NOT re-run on the final tree |
| `EC_MEMGUARD_MAX=10G cargo test -p ec-av1 --release -- --test-threads=4` (host b) | exit 0; lib 807 passed, 0 failed, 49 ignored; `fuzz_regressions` 4 passed; 2987 s |
| `cargo clippy -p ec-h265 -p ec-h265-syntax -p ec-av1-syntax --all-targets -- -D warnings` | exit 0 on the tree before the final slice-header checks (ec-h265-syntax re-checked after them: exit 0); the other two crates were not re-linted after the last edits |
| `cargo clippy -p ec-av1 --all-targets -- -D warnings` | exit 101: 323 errors, all in pre-existing `crates/ec-av1/src/*.rs` (decode.rs, encode.rs, cdf.rs, ...). Not touched by this lane except the two guards above, which add no new lint. Not fixed: outside this lane. |

## 1 CPU-hour gate (`-max_total_time=3600 -rss_limit_mb=2048 -max_len=262144 -timeout=10`, host c, two at a time)
| target | runs | result |
|---|---|---|
| ec-h265 parse_params_slice | 8107128 | clean, exit 0 (final tree) |
| ec-h265 decode_annexb | 425673 | clean, exit 0 (final tree) |
| ec-av1 parse_obu | 8597687 | clean, exit 0 (tree of 11:21; no change to the parser since) |


Earlier 1 h runs that found crashes are the source of items 1 to 18. Earlier clean 1 h runs (decode_annexb 533325 runs, decode_stream 678057 runs) were on older trees and are superseded by the table above.

## Left
- Host a (the only host with the reference YUVs) was busy for over two hours, so the final test, conformance and clippy runs on the final tree did not happen. These must be re-run before merge: `scripts/remote-run.sh fuzzh265av1 --host a` with the commands in this report.
- The parse_params_slice crashes found this session (items 8 to 14) came one per hour; the final 1 h run after item 14 was clean, but any further input-driven overflow would show up in the same way.
- Items 17 and 18 are guards on an inconsistent AV1 state; the root cause is open.
- The 64x64 caps mean the fuzzers do not reach multi-CTB or tiled picture paths.
- The corpus is not committed (`seed.sh` regenerates it).
