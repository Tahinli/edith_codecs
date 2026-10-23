# Lane mpeg4p2 report: ec-mpeg4

Branch lane/mpeg4p2 in /home/user/.cache/wt/mpeg4p2 (rebased on main, no trailers, not pushed).

## What it is
`crates/ec-mpeg4`: MPEG-4 Part 2 Visual Simple and Advanced Simple decoder plus H.263 baseline. `CodecId::Mpeg4Part2` (registry name "mpeg4"). Output I420 in display order, interlace flag set where the VOL is interlaced. Byte-exact with ffmpeg's C decoder (`ffmpeg -threads 1 -cpuflags 0`).

## API (for the edith integration)
- `Mpeg4Decoder::new(CodecParameters) -> Result<Mpeg4Decoder>` (extradata = VOL header / esds DecSpecificInfo, optional; in-band VOL headers also work). Implements `ec_core::registry::Decoder` (send_packet / receive_frame / flush / reset).
- Direct API: `send_bytes(&[u8], Option<Timestamp>)`, `pop_picture() -> Option<(VideoFrame, PictureInfo)>` (`PictureInfo { pict_type: char, damaged, .. }`), `flush_stream()`, `last_picture_info()`, `low_delay()`.
- Tuning: `set_fourcc([u8;4])` (AVI FourCC; enables ffmpeg's FourCC based workarounds; the registry constructor cannot receive it), `set_xvid_idct(Option<bool>)` (None = auto from XviD user data like `-idct auto`).
- Delay: unless the VOL has low_delay, one reference picture is held (display order, B frames come out immediately, flush emits the held one). No PTS reordering is done: a packet's pts is attached to the picture decoded from it. AVI has no PTS anyway.
- N-VOP (not coded) packets produce no frame, as libavcodec does. Packed bitstream (DivX packed B-frames) is unpacked internally (ffmpeg stash model, also for XviD packed).

## Scope
I/P/B/S(GMC) VOPs, half/quarter-pel, 4MV, direct mode (all variants), field DCT and field MC, MPEG and H.263 quant incl. custom matrices, AC/DC prediction, resync, N-VOPs, packed bitstream, H.263 short header (QCIF..4CIF, GOB headers). GMC with 0 to 3 warp points (superset of the brief's 1 point; all exact vs libxvid streams).
Refused by name: studio profiles, non-rectangular shape, static sprites, sprite brightness change, NEWPRED, reduced-res VOPs, scalability, data partitioning, RVLC, H.263 PLUSPTYPE/SAC/AP/PB-frames. Data partitioning and RVLC are not implemented (the tables are there).

## Containers
- ec-riff: XVID/XVIX/DIVX/DX50/FMP4/MP4V/M4S2/MP4S/RMP4/DVX1/BLZ0/UMP4 -> Mpeg4Part2 (keyframe sniff when no index). DIV1-6, MP42, MP43, MPG4, 3IV2, SEDG stay refused by name. Pass the FourCC to `set_fourcc` if you have it.
- ec-mp4: `mp4v` (esds objectTypeIndication 0x20, extradata = DecSpecificInfo; other OTIs skipped), `s263`/`h263` (short header, no config).
- ec-matroska: V_MPEG4/ISO/SP|ASP|AP, and V_MS/VFW/FOURCC with the same FourCCs.

## Verification
- tests/ffmpeg_parity.rs (32 cases) and tests/containers.rs (AVI DX50/XVID/FMP4, MP4, 3GP H.263, MKV, packed bitstream, data-partitioning refusal, corruption robustness, ignored speed test).
- Reference caveats: `-threads 1` (frame threading loses the interlaced direct-mode field-MV table in ffmpeg) and `-cpuflags 0` (ffmpeg's x86 put_no_rnd_pixels8_x2/y2 SIMD differs from its own C code by +-1 in rare pixels; we follow the C code).
- A resync quirk found at server gate time: ffmpeg clears only last_mv[dir][0] at a video-packet start (field MV predictors survive); matched, with regression tests.
- Fuzz: crates/ec-mpeg4/fuzz, target decode_mpeg4, seeds in fuzz/seeds. Results and speed below.

## Final numbers (server, last commit)
- `cargo clippy --workspace --all-targets --release -- -D warnings`: clean.
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-mpeg4 -p ec-mp4 -p ec-riff -p ec-matroska -p ec-core --release`: all pass.
- Speed (ignored test, 720p, 100 frames, qpel+4MV+B, one thread, server under load): 224 fps (local machine about 360 fps).
- Fuzz decode_mpeg4: 600 s, 117,701 runs, no crash, no OOM, no timeout.
