# lane hi444 report (branch lane-hi444)

Commits: the feature commit and a clippy fix on top of ca702772. No trailers.

## Done
- Applied the codecs-lane diffs: `decode.rs` `Deep::load` (I444/I410, 2x2 average, `(a+b+1)>>1` per axis, 10-bit `>>2`) and `demux.rs` `avc_bit_depth` (monochrome, 4:4:4 and transform_bypass no longer refused; refusal text updated).
- Chroma path: 2x2 average to 4:2:0 (the minimum). Monochrome arrives as I420/I010 with neutral chroma, so no change was needed.
- Chroma path, end-to-end 4:4:4 export: not built. The export is 4:2:0 only. Chroma-reduction wording already exists (`export.rs chroma_reduction_words`). Carrying 4:4:4 would need a 4:4:4 encoder input in the HEVC/AV1 seats (ec-h265 / rav1e `Cs444`) plus a wide `Deep` plane variant. Medium effort, not cheap.
- Hardware decode: `h264_software_only` (decode.rs) now also returns true for chroma 4:4:4 or gray and for any profile name containing "4:4:4" (Predictive/CAVLC 4:4:4 Intra, including 4:2:0 lossless under them). `probe()` therefore reports Software, `open_hw` is never called and `hw_decodes` returns the named reason. engine-hw's CAP_TABLE has H264 Baseline/Main/High only, so no engine-hw change.
- FMO is now refused at open, by name, in `avc_bit_depth`: "H.264 FMO (flexible macroblock ordering, N slice groups) is not supported ...". Previously ec-h264 failed on the first picture. More than 10 bits is still refused as "N-bit H.264 ...", and a luma/chroma depth mismatch likewise.
- Probe: `VideoFacts` has no pixel-format string. It exposes `profile` ("High 4:4:4 Predictive", "High", "High 10") and `bit_depth`. `Demuxer::chroma()` gives C444/Gray (displays "4:4:4"/"gray"). No new field, because adding one touches struct literals outside this lane.

## Tests (server host c)
- New `crates/engine/tests/h264_hi444.rs`: 6 shapes (yuv444p, yuv444p10le, both `-qp 0`, gray, gray10le) x mp4/mkv, with B-frames and a GOP of 10.
  - Probe and open: chroma, depth, profile and frame count; backend Software.
  - Pixels vs ffmpeg: ffmpeg's native decode is averaged to the working 4:2:0, ffmpeg converts it to BGRA, and PSNR is compared. 4:4:4 is >= 48.2 dB (the gap is the YUV->RGB rounding and the upsampler; threshold is 45 dB). Gray is bit exact. The full-range VUI that ffmpeg writes for gray is honoured.
  - Mid-GOP seek equals the sequential decode (both pinned to software and on the default route).
  - Export to HevcMp4 and Hevc/mkv: 30 frames, 1.0 s +-0.1.
- `interlaced_h264.rs`: the old refusal test is now `four_four_four_monochrome_and_lossless_open_and_keep_the_gpu_off`. The refusal case in `h264_high10.rs` is removed.
- `demux.rs` unit test `avc_refuses_more_than_ten_bits_and_fmo_by_name_and_opens_the_rest`: hand-built SPS/PPS cover 8/10-bit 4:4:4 opening, 12 and 14-bit refusal, and 3-group FMO refusal. libx264 cannot make 12-bit or FMO, so those are synthetic parameter sets, not streams.

## Gates (host c, final commit)
- `cargo clippy -p engine -p edith_mcp -p edith --release --all-targets -- -D warnings`: clean.
- Full `cargo test -p engine -p edith_mcp -p edith --release --no-fail-fast`: everything passes except the 2 known VA-API-only failures (h264_/hevc_mid_gop_seeks_match_a_sequential_decode). The engine lib reported 866 passed.

## export.rs hunks
None. No edits to crates/app, project.rs, playback.rs, text.rs, edith.rs, any Cargo.toml or assets.
