# fuzzaudio1: cargo-fuzz targets for ec-aac and ec-opus

Goal: DONE.md shared gate 2, at least one cargo-fuzz target per untrusted entry point in
ec-aac and ec-opus.
Base sha: 3e6d0111 (main at worktree creation).

## What changed
- `Cargo.toml` (root): `exclude = ["crates/*/fuzz"]` in `[workspace]`.
- `crates/ec-aac/fuzz/`: standalone cargo-fuzz crate (`ec-aac-fuzz`, own `[workspace]`, path dep on ec-aac).
  - `decode_adts`: whole input as one packet through `AacDecoder::new().decode`.
  - `decode_raw`: byte 0 selects one of four hand-packed ASCs (LC mono 48k, LC stereo 48k,
    HE-AAC explicit SBR stereo, LC 5.1 48k); then u16-BE-length-prefixed raw access units.
  - `seed.sh`: ADTS fixtures as decode_adts seeds; re-packed raw AUs as decode_raw seeds (6 + 8).
- `crates/ec-opus/fuzz/`: standalone cargo-fuzz crate (`ec-opus-fuzz`).
  - `decode_packet`: byte 0 bit0 = channels (1/2), bits1-3 = rate index mod 5 (8/12/16/24/48 kHz);
    then u16-BE-length-prefixed packets decoded in sequence on one `Decoder`.
  - `parse_toc`: first byte as TOC (`Toc::new`, mode/bandwidth/frame_size/channels), then
    `Packet::parse` in plain and self-delimited framing.
  - `seed.sh`: RFC 6716 `.bit` test vectors (mono and stereo configs) and Ogg-Opus fixtures (30 + 15 seeds).
- Both crates: `.gitignore` for `target`, `corpus`, `artifacts`, `coverage`.

`parse_toc` was included because `Toc` and `Packet` are public.

## Verify commands and results
All run through `scripts/remote-run.sh fuzzaudio1`, from the worktree.
- `cargo +nightly fuzz build --fuzz-dir crates/ec-aac/fuzz`: ok (all targets built).
- `cargo +nightly fuzz build --fuzz-dir crates/ec-opus/fuzz`: exit 0.
- `cargo +nightly fuzz run decode_adts --fuzz-dir crates/ec-aac/fuzz -- -max_total_time=120 -rss_limit_mb=3000 -timeout=10`:
  exit 0, 7077 runs in 121 s, no crash.
- `... decode_raw ...` (ec-aac): exit 0, 3920 runs in 121 s, no crash.
  First attempt never reached a server: both hosts were slot-busy for the 10-minute retry window,
  `remote-run` fell back to local, and local has no cargo-fuzz (`error: no such command: fuzz`).
  That was a harness failure, not a codec result. Re-run pinned with `--host a`.
- `... decode_packet --fuzz-dir crates/ec-opus/fuzz ...`: exit 0, 13267 runs in 121 s, no crash.
- `... parse_toc --fuzz-dir crates/ec-opus/fuzz ...`: exit 0, 21988375 runs in 121 s, no crash.

Crashes, timeouts, OOM: none in any of the four smoke runs. No reproducers to report.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- Corpus is seeded from fixtures only; no minimised corpus or coverage measured.
- `fuzz/Cargo.lock` was generated on the host and is not in this commit.
- The Ogg-Opus fixture seeds are the 3 stereo/mono/5.1 files; no mka/mp4 seeds were added.
