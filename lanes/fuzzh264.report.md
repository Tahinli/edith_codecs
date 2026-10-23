# lane fuzzh264: cargo-fuzz targets for ec-h264

Goal: DONE.md shared gate 2. One cargo-fuzz target per untrusted entry point of crate ec-h264.

Base sha: 3e6d0111 (main).

## What changed
- `crates/ec-h264/fuzz/Cargo.toml`: package `ec-h264-fuzz`, `[package.metadata] cargo-fuzz = true`,
  `libfuzzer-sys 0.4`, path deps on `ec-core` and `ec-h264`, own `[workspace] members = ["."]`.
- `crates/ec-h264/fuzz/fuzz_targets/decode_annexb.rs`: splits the raw bytes on Annex B start codes,
  pushes every NAL through `Decoder::push_nal` (closing the picture and re-pushing on
  `PictureBoundary`), then `flush` and drains `next_frame`.
- `crates/ec-h264/fuzz/fuzz_targets/decode_avcc.rs`: packet-level `H264Decoder` as a demuxer drives it.
  Input byte 0 gives the avcC extradata length, then the avcC record, then one packet
  (length-prefixed NALs with a valid avcC, Annex B otherwise).
- `crates/ec-h264/fuzz/seed.sh`: copies 5 seed streams (CABA2_SVA_B, CANL3_SVA_B, BA3_SVA_C,
  SL1_SVA_B, AUD_MW_E .264, all under 200 KB) from `fixtures/vectors/h264-jvt` into
  `corpus/decode_annexb/`, and the same set into `corpus/decode_avcc/`.
- `crates/ec-h264/fuzz/.gitignore`: `target/`, `corpus/`, `artifacts/`, `coverage/`.
- Root `Cargo.toml`: `exclude = ["crates/*/fuzz"]` added under `[workspace]`.
- No `parse_nal` target: ec-h264 exposes no standalone SPS/PPS/slice-header parser (those live in
  ec-h264-syntax, not a public entry of this crate).

## How to run
    cd crates/ec-h264/fuzz && ./seed.sh && cargo +nightly fuzz run decode_annexb
    cd crates/ec-h264/fuzz && ./seed.sh && cargo +nightly fuzz run decode_avcc
Via the server: `scripts/remote-run.sh fuzzh264 -- bash -c 'cd crates/ec-h264/fuzz && ./seed.sh && cargo +nightly fuzz run <target> -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10'`

## Smoke results (server, 120 s each, -rss_limit_mb=3000 -timeout=10)
| target | runs | final cov | final corpus | peak rss | exit | crashes/timeouts/OOM |
|---|---|---|---|---|---|---|
| decode_annexb | 1041 (8 exec/s) | 5257 | 655 units, 10 MB | 473 MB | 0 | none |
| decode_avcc | 6831 (56 exec/s) | 6687 | 785 units, 7.5 MB | 443 MB | 0 | none |

Observation: decode_annexb ran at about 8 exec/s, far below decode_avcc. Likely cause (not
verified): mutated SPS dimensions drive large picture allocations (rss grows to ~470 MB).
Worth a look in a later lane, not a crash.

Attempts that did not count: the first two decode_annexb runs found both server slots busy and
host b rsync failed on another lane's path; one run was killed by the outer timeout, and the
first local fallback ran without cargo-fuzz. The reported numbers come from the successful server runs.

## Left
- No crashes found in the smoke window. Longer runs are the orchestrator's call.
- Corpus is not committed (seed.sh regenerates it).
