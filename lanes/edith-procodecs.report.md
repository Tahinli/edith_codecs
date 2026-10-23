# procodecs / codecs half (branch lane-procodecs)

## What was built
- `crates/engine/src/decode/intra.rs`: shared pulled `Stream<S: Seat>` + `run::<S>` span loop + `probe()` (first-sample facts).
  Seats: `decode/prores.rs` (ec-prores), `decode/dnxhd.rs` (ec-dnxhd), `decode/mjpeg.rs` (ec-mjpeg). `Pulled::{ProRes,Dnxhd,Mjpeg}` (next + next_wide), `run` dispatch in decode.rs, `hw_decodes` refuses them (engine-hw arms were already no-ops).
- Seeks: every sample is a sync sample; a span starts exactly on the frame (one frame earlier when interlaced, for the deinterlacer).
- Pictures: 10-bit via `Deep` (4:2:2/4:4:4 averaged to 4:2:0); 12-bit (ProRes 4444, DNxHD 12) narrowed to 10 (`>>2`); gray -> I420 neutral chroma; export uses `next_wide` (10-bit seat) for ProRes/DNxHD.
- Interlace: ProRes/DNxHD frame flag, MJPEG two-JPEG packets (woven by ec-mjpeg, field order from mp4 `fiel`/mkv FieldOrder via `deinterlace::container_order`, default TFF) -> shared `Deinterlacer` (flagged_only, like MPEG-2). `deinterlace::filter_field` now also accepts I444/I410. `probe_media().video.interlaced` = "tff"/"bff" for these.
- MJPEG full range: `probe` states `full_range=true` + BT.601 as the stream's own colour tags; `Demuxer::open` re-resolves `meta.color` (container `colr` still wins). ProRes/DNxHD matrix/transfer/primaries come from their frame headers.
- ProRes alpha: no alpha path exists for video clips in the engine; alpha plane dropped (documented in intra.rs / prores.rs), colour planes unaffected.
- Containers (demux.rs): `CODEC_IDS` row Mjpeg (`V_MJPEG`/`jpeg`); `MP4_ONLY` table (mjpa, apco apcs apcn apch ap4h ap4x, AVdn AVdh); `mkv_private_codec` (V_QUICKTIME / V_MS/VFW/FOURCC with AVdn/AVdh in CodecPrivate, mirrors ec-matroska); samples pass verbatim (no Annex-B) in mp4 and mkv. `Demuxer::fill_intra` (one decode of the first sample at open) fills bit depth (ProRes 422 10, 4444 12, DNx 8/10/12, MJPEG 8), chroma, DNxHD profile, interlace, colour; a named decoder refusal (DNxHD alpha, 12-bit JPEG...) fails the open, a merely undecodable first sample does not. New field `intra` in Mp4Demuxer/MkvDemuxer, accessor `Demuxer::intra()` (wildcard arm for other variants, so the AVI variant needs no edit).
- Capability matrix: unit test extended (Mjpeg row, MP4_ONLY, mkv private rule). Documented gaps in `UNSUPPORTED`: `V_PRORES` (ProRes in Matroska: ec-matroska maps no id), `V_QUICKTIME`/`V_MS/VFW/FOURCC` (only DNxHD inside).
- Profiles (video_profile.rs): ProRes `422 Proxy|422 LT|422|422 HQ|4444|4444 XQ` (from sample-entry fourcc; Apple names, not ffprobe's); DNxHR `DNxHR LB|SQ|HQ|HQX|444`, DNxHD `DNxHD 36..220x` by nominal rate (within 6 %).
- MCP (crates/mcp/src/tools/media.rs): `codec_phrase` ("ProRes 422 HQ", "DNxHR HQX", "DNxHD 145", "MJPEG"), `plain_codec` for apco.. / AVdn(dnxhd) / AVdh(dnxhr) / V_MJPEG / jpeg / mjpa (mjpeg); unit test added. JSON `codec` is the engine name ("ProRes", "DNxHD", "MJPEG"), `profile` the profile.

## Tests
`crates/engine/tests/procodecs.rs` (6 tests, ~75 s on host c): ProRes proxy/lt/standard/hq/4444/4444xq/4444+alpha/interlaced; DNxHD 120/185x/36 (1080p), DNxHR lb/sq/hq/hqx/444 (+ sq/hqx in mkv), DNxHD interlaced; MJPEG yuvj420/422 in mov+mkv; two-field MJPEG in mov(mjpa)+mkv; ProRes-in-mkv named gap. Per case: probe (codec, width, fps, depth, profile, interlaced, duration, frame count, sync points), BGRA PSNR vs ffmpeg (measured 31-55 dB, floor 28; 4:4:4 lowest because chroma is averaged, wrong matrix/range = ~24), seeks (7 targets + a range) bit-equal to sequential, export (duration, nb_frames), A/V sync (flash 1.0 s / beep) open + seek + export for 7 clips. Exactness vs ffmpeg is not claimed at the engine level (chroma averaging, 8-bit truncation); the decoders are byte-exact in their own crates. mov vs mkv MJPEG pictures compared.

## Hunks in shared files
- export.rs (one line, codec list): `wide: (wide && meta.codec == Codec::Hevc)` -> `matches!(meta.codec, Codec::Hevc | Codec::ProRes | Codec::Dnxhd)` (10-bit export seat).
- deinterlace.rs: `container_order` made pub(crate); `field_order` returns the intra facts first; `filter_field` accepts I444/I410.
- No app/project/playback/text/edith changes. App diffs needed: none (file-dialog filters: add `mov`/`mkv` are already there; `.mxf`/AVI belong to the other half).

## Cargo.toml (already in base)
ec-prores, ec-dnxhd, ec-mjpeg = path "../../../edith_codecs/crates/<name>".

## Limits
- 4:2:2/4:4:4 are averaged to 4:2:0 and 10/12-bit reduced to 8 for playback (as HEVC/H.264 High 10 do); export keeps 10 bits.
- Per-clip Deinterlace mode is not honoured (as MPEG-2): interlaced ones are always frame-deinterlaced.
- ProRes alpha dropped. ProRes in Matroska and MJPEG-B (`mjpb`) not mapped. DNxHD profile name by nominal rate, `None` for non-Avid rates.
- The AVI half can reuse `decode::intra::{probe, Stream}`; for AVI set `IntraFacts`/`fill_intra` equivalents and pass BottomFieldFirst for MJPG-tag two-field packets.

## Gates (host c, final commit "style(engine): intra seat uses as_chunks")
- `cargo clippy -p engine -p edith_mcp -p edith --release --all-targets -- -D warnings`: clean.
- Full `cargo test -p engine -p edith_mcp -p edith --release --no-fail-fast`: passed 3588, failed 2 (the known VA-API-only h264_/hevc_mid_gop_seeks_match_a_sequential_decode).
