# lane mp3fuzz: ec-mp3 cargo-fuzz target, 1 CPU-hour run

Goal: a cargo-fuzz target for ec-mp3 that covers MP1/MP2 (Layer I/II) and Layer III decode, run for 1 CPU-hour.

Base: `main` at `ff89059b`. Branch `lane/mp3fuzz`.

## Changes

- `crates/ec-mp3/fuzz/Cargo.toml`: standalone fuzz crate (`ec-mp3-fuzz`), libfuzzer-sys 0.4, `[workspace]` members = ["."], excluded from the root workspace like the sibling fuzz crates.
- `crates/ec-mp3/fuzz/fuzz_targets/decode.rs`: target `decode`. The first input byte picks a push chunk size; the rest is pushed into `Mp3Reader` in chunks and every frame is decoded until the reader returns an error. Layer I/II/III is selected by the frame header. Harness cap: 512 frames per input.
- `crates/ec-mp3/fuzz/seed.sh`: 9 ffmpeg seeds, 1 s sine tone each: MP2 mono/stereo at 32k, 44.1k, 48k; MP2 LSF 22.05k; MP3 mono/stereo 44.1k; MP3 LSF 22.05k. No Layer I seeds: ffmpeg has no Layer I encoder and `tests/mp2_matrix.rs` has no Layer I generator.
- `crates/ec-mp3/fuzz/.gitignore`: corpus, artifacts, target, coverage.

No source change in `crates/ec-mp3/src`, so no regression test was added (no crash found).

## Fuzz run

- Host c, cargo-fuzz nightly, libva workaround via `~/lib-va/bin` on PATH.
- Command: `cargo +nightly fuzz run decode -- -max_total_time=3600 -max_len=8192 -rss_limit_mb=1536 -timeout=30`
- `-max_len=8192` was added on the coordinator's instruction. The seeds are whole 20-24 KB files, so the default max_len gave about 25 exec/s, roughly 90k runs in the hour. A first attempt with the default max_len was killed early and its partial log is not counted. 8 KB still holds dozens of frames, so resync and the bit reservoir are covered.
- Result: `Done 322492 runs in 3601 second(s)`, EXIT=0.
- Crashes: 0. No artifacts, no timeouts, no OOM.
- Final coverage: cov 1981, ft 10235, corpus 1250 entries (4392 Kb), rss 491 Mb.

## Tests

- `cargo clippy -p ec-mp3 -- -D warnings` (server): clean, EXIT=0.
- `cargo test --release -p ec-mp3` (server): EXIT=0. lib 22 passed; decode_matrix 1; encode_matrix 5 passed, 2 ignored; mp2_matrix 7; doc-tests 0. Total 35 passed, 0 failed, 2 ignored.
