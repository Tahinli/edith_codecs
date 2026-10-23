# Lane interlaced: report (branch lane-interlaced, commit c870cd86, not pushed)

## What changed
- New `crates/engine/src/deinterlace.rs`: bwdif-style spatial-temporal, motion-adaptive deinterlacer (ffmpeg's coefficients, no deps, std threads). One progressive frame per frame, keeps the first-displayed field. Generic over 8-bit (I420/I422) and 10-bit (I010/I210). `Deinterlacer` stage = pass-through (no delay, no bytes touched) when disabled; when on it runs one frame behind (prev/cur/next, ends use themselves). `Keep::{Top,Bottom}` takes the parity already: the double-rate (50i->50p) hook is `filter_frame` called twice per input frame with the other field kept; the timeline is frame-locked so nothing uses it (needs a clip/project setting in project.rs/playback.rs, not touched). Also `field_order()` for probing.
- Detection: SPS `frame_mbs_only_flag=0` (`Demuxer::h264_interlaced`), order from the first picture ec-h264 releases (stream's own POC order), fallback mp4 `fiel` / MKV FieldOrder scan, then tff. `VideoFacts.interlaced: Option<&'static str>` ("tff"/"bff"); MCP `probe` rows and the media listing JSON gain `"interlaced"`. (crates/app/src/tests/view.rs: 2 compile-forced `interlaced: None` literals.)
- Routing (decode.rs): interlaced, >8-bit and 4:2:2 H.264 are never offered to VA-API (`h264_software_only`), so playback, stills, thumbnails and export all take ec-h264. `run()` decodes through the Deinterlacer; seeks land one picture earlier (`start-1`) so the target is filtered with its true predecessor. Export (export.rs `H264Seat`) does the same; interlaced seats are not parked.
- demux.rs: interlaced High 10 4:2:0 refusal removed. 4:2:2 (8/10-bit, progressive and interlaced) now opens (scope addition). Refused by name at every depth: 4:4:4, monochrome, lossless (transform_bypass), luma/chroma depth mismatch, depth > 10.
- `Deep::load` (decode.rs) now takes I422/I210 as well: chroma row pairs averaged `(a+b+1)>>1` to 4:2:0 like HEVC 4:2:2; 10-bit stays 10-bit in the wide (10-bit export) path, narrows `>>2` for playback.
- Export is progressive: H.264 mp4 output reports field_order=progressive. HEVC outputs report `unknown` (the ec-h265 encoder writes no VUI field flag; tagging it progressive would be an ec-h265 encoder change, not engine).
- MPEG-TS/.mts: edith has no TS demuxer, so not testable here; mp4 and mkv only. PAFF: x264 cannot produce it; PAFF is covered by ec-h264's own JVT vectors, the engine path is the same (flag from ec-h264). All fixtures are x264 MBAFF.
- tests/h264_high10.rs: the 4:2:2-refused test became a 4:4:4-refused test (4:2:2 now opens).

## PSNR vs progressive original (dB, all planes; 352x288, crf 10, x264 MBAFF)
| clip | woven | ours | ffmpeg bwdif | ffmpeg yadif |
|---|---|---|---|---|
| 8-bit tff mp4 | 30.15 | 33.66 | 33.66 | 33.00 |
| 8-bit bff mkv | 29.65 | 33.69 | 33.68 | 33.00 |
| High10 tff mp4 | 30.17 | 33.68 | 33.68 | 32.87 |
| High10 bff mkv | 29.67 | 33.72 | 33.71 | 33.00 |
| 4:2:2 8-bit tff | 30.05 | 36.99 | 37.00 | 34.79 |
| 4:2:2 10-bit bff | 29.71 | 37.03 | 37.04 | 34.69 |
| 4:2:2 10-bit tff | 30.07 | 37.03 | 37.04 | 34.71 |
| moving edge (8 px/field) | 24.65 | 71.97 | 71.97 | 71.98 |

Within 0.01 dB of bwdif everywhere. Combing metric (share of luma pixels that are a >20-step extreme against both neighbours), testsrc2 tff: woven 0.01625, ours 0.00088, bwdif 0.00086, yadif 0.00141, original 0.00524. Moving edge: woven 0.01591, ours/bwdif/yadif 0.00000.

## Speed (1080p I420, host c, 4 vCPU)
~50 fps single thread, ~110-120 fps with 4 threads (`filter_frame`, test `deinterlace_speed_at_1080`).

## Tests (crates/engine/tests/interlaced_h264.rs, all skip without ffmpeg+libx264)
probe tff/bff/progressive (8/10-bit/4:2:2, mp4+mkv); ec-h264 woven pictures byte-exact vs ffmpeg (yuv420p, yuv420p10le, yuv422p, yuv422p10le); PSNR table above asserted (within 0.5 dB of bwdif, >1 dB over woven, combing < 25%); session frames deinterlaced, every mid-GOP seek and a range equal the sequential decode byte for byte (12 targets x 8 clips); progressive decode hash 12ead68cc048d028 is identical on baseline eec823df and this branch; export (Mp4, HevcMp4, Hevc) progressive, 30 frames, 1.2 s, 10 bits kept for 10-bit sources, combing cut; 4:4:4 / monochrome / lossless refused by name.

## Gates (host c)
- `cargo clippy -p engine -p edith_mcp -p edith --release --all-targets -- -D warnings`: clean.
- `cargo test -p engine -p edith_mcp --release --no-fail-fast`: all pass except the 2 known VA-API-only (h264/hevc_mid_gop_seeks_match_a_sequential_decode).

## export.rs hunks (all interlace; the 4:2:2 work needed none there)
All in `H264Seat`/`SwDecoder` (see `git diff HEAD~1 -- crates/engine/src/export.rs`, 57 lines):
1. `H264Seat` gains field `deint: Deinterlacer`; `at()` initialises it disabled.
2. New `H264Seat::open(demuxer, start, wide)`: seeks to `start-1` when `demuxer.h264_interlaced()`, builds the Deinterlacer(first_wanted=start). Both `SwDecoder::open` and `SwDecoder::new` call it instead of `seek_to_sync_at_or_before` + `at`.
3. `H264Seat::advance`: pictures go through `deint.push(tag, pic)`; below-start outputs are skipped; at EOF `deint.finish()` releases the held last picture.
4. `SwDecoder::parkable` false and `flushed()` true for an interlaced seat (no resumable index).

## Not done / notes
- Double-rate output: not exposed (needs a project/clip setting; project.rs/playback.rs are peer-owned). Hook: call `deinterlace::filter_frame` with `Keep::Top` and `Keep::Bottom`, in display order (first field's output first), and double the frame timing.
- Frames of an interlaced-SPS stream are all deinterlaced, including any frame the stream itself marks progressive (as ffmpeg `deint=all`).
- HEVC export field_order stays `unknown` (see above).
