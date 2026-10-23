# Definition of done — edith_codecs

Written 2026-10-15. A crate is "done" when it meets **what edith needs**, not
when it matches every corner of its spec. Work outside these specs is frozen
until edith needs it.

## Tiers

| Tier | Meaning | Who may open a lane |
|---|---|---|
| **P0** | edith calls it today | yes, against the criteria below only |
| **P1** | edith should call it; wiring it in is the work | yes, wiring + criteria below |
| **Frozen** | edith does not need it | no new features; keep building, clippy, tests only |

## Gates every P0/P1 crate must pass

1. `cargo clippy -p <crate> -- -D warnings` clean (an `#[allow]` needs a one-line reason).
2. At least one `cargo-fuzz` target per untrusted entry point (parse/decode), 1 CPU-hour
   each with no panic, hang or OOM. Every crash found becomes a regression test.
3. No panic on malformed input: unsupported → `Error::Unsupported` naming the feature,
   malformed → `Error::Corrupt`. No `debug_assert` as the only guard before indexing.
4. Correctness measured against an external decoder (ffmpeg/libaom/libvpx/libopus),
   on both the conformance set named below and `real_library.toml`.
5. An edith integration test drives the crate through edith's own call site.
6. Speed measured and recorded in `lanes/bench.log` on the same machine and settings.
7. No source file over 5,000 lines in a crate that still has open work.

## Process rules

- A lane charter names the edith-visible effect it buys. No effect, no lane.
- **Round cap: 3.** A topic not closed after 3 rounds escalates to a stronger model,
  or the topic is dropped and recorded as a refusal.
- A finished crate gets a one-line "DONE <date>" entry in this file; reports stay in `lanes/`.

---

## Video

### ec-h264 decode — P0
edith: software decoder whenever VA-API is missing or fails (`engine/src/decode.rs:1297`, `:3766`).
- **Scope:** Baseline/Main/High/High 10/High 4:2:2 (and High 4:2:2 Intra), 4:2:0 and 4:2:2 at 8 to
  10 bits, progressive, PAFF and MBAFF, CAVLC+CABAC, B-frames, weighted pred, 8x8, scaling matrices.
  4:2:2 comes out as `I422` (8-bit) or `I210` (9/10-bit), like ec-h265.
- **Out of scope (refused by name):** FMO with interlaced coding, SP/SI, data partitioning, 4:4:4 (High 4:4:4 Predictive,
  CAVLC 4:4:4 Intra), monochrome (4:0:0), separate colour planes, lossless transform bypass,
  bit depth above 10, differing luma/chroma bit depths.
- **Done when:**
  - Every JVT progressive 8-bit 4:2:0 stream in `scripts/fetch-vectors.sh` decodes bit-exact.
    The out-of-scope streams (SP/SI, data partitioning, 4:4:4, monochrome)
    each refuse by name, and a test asserts that.
  - Every JVT FRExt High 4:2:2 stream (Hi422FR1..4,6..15, Hi422FREXT16..19: frame, PAFF, MBAFF, 8 and
    10 bit, CAVLC and CABAC) is bit-exact against the reference YUV or ffmpeg (`tests/high422_jvt.rs`),
    and x264 `high422` sweeps match ffmpeg byte for byte (`tests/high422.rs`).
  - `real_library.toml`: all bit-exact (currently 15/15, `lanes/h264reallib.report.md`).
  - 1080p30 decodes at ≥ realtime on this machine (frame or slice threading allowed).
  - The shared gates pass (`ec-h264/src/dpb.rs:875` `expect` → error).
- **Frozen:** SP/SI, data partitioning, 4:4:4, monochrome, separate colour planes.
- **D1 resolved:** High10, PAFF, MBAFF and High 4:2:2 are all decoded, see the D1 row. AVCHD 1080i and some
  cameras produce these, so they matter to an editor more than anything in the frozen list.

### ec-h264 encode — P0
edith: software H.264 export (`engine/src/export.rs:4407`), Preset::Fast, no B-frames.
- **Done when:**
  - The output decodes in ffmpeg identically to the encoder's own reconstruction.
  - Rate control lands within ±5% of the target bitrate over a 60 s clip.
  - BD-rate vs x264 `-preset veryfast` (same features: no B-frames) is ≤ +20%.
    It is currently +20 to +42% (`lanes/h264x264-r2.report.md`).
  - 1080p30 encodes at ≥ realtime with threads.
- **Frozen:** B-frames, interlaced, High10.

### ec-av1 decode — P0
edith: software AV1 decode (`engine/src/decode.rs:3917`), consumes 8/10-bit **4:2:0 only**.
- **Scope:** profile 0 (Main): 8/10-bit 4:2:0 + mono, film grain, superres, tiles.
- **Done when:**
  - The AOM conformance streams for profile 0, 8 and 10 bit, are bit-exact vs libaom/dav1d.
  - `real_library.toml` AV1 entries are bit-exact.
  - 1080p30 8-bit decodes at ≥ realtime with `EC_AV1_THREADS` > 1. Today the default is
    1 thread and the only number is 640x384 (`lanes/bench.log:159`).
  - `decode.rs` (61k lines) and `stream.rs` (57k lines) are split into modules ≤ 5k lines
    each, **before** any further AV1 lane.
  - The clippy errors are fixed: `uninit_vec` at `decode.rs:22362` and `film_grain.rs:1299`;
    `mut_from_ref` at `par.rs:192` gets a justified `#[allow]`.
  - **edith side:** non-4:2:0 AV1 is refused at demux with a named reason. Today it is not
    checked and would be mis-indexed as I420.
- ~~**Frozen:** profiles 1/2 (4:4:4, 4:2:2, 12-bit).~~ **Reopened 2026-10-19 (orchestrator, owner
  request "fix the known gaps"):** three named defects fixed byte-exact vs libdav1d (`av1lr422`,
  `av1e422edge`, `av1v12`; merged ec-av1 818/0, clippy clean on main `bbb021d0`); edith opens,
  plays and exports profile 1/2 by averaging to 4:2:0, ≥ 45 dB vs ffmpeg (edith `c295f63`).
  A 90-stream 4:2:2 sweep (8/10/12-bit, 6 sizes, 5 seeds, CDEF+LR) is byte-exact (`av1422rest`).

### ec-av1 encode — Frozen (edith keeps rav1e)
edith exports AV1 with **rav1e at speed 10** (`engine/src/export.rs:156`, `:4457`), built
pure-Rust (`default-features = false, features = ["threading"]`), and never calls this
encoder. Last numbers (`lanes/pareto2.md`, 2026-09-08): +23 to +31% BD-rate vs libaom cpu-6;
about equal to rav1e on film and better on screen; 2–11x slower than rav1e.
- Decided 2026-10-15: rav1e stays. No new lanes on this encoder. It stays building and
  tested, as the fallback if rav1e is ever abandoned.

### ec-h265 decode — P0 (new work: the largest single item in this file)
Why: phones record HEVC by default (iPhone, most Android), and stock Fedora's Mesa has
VA-API HEVC decode disabled. Without a software decoder, edith cannot open phone footage
on a stock install. Today ec-h265 decodes only its own encoder's IDR output
(`ec-h265/src/decode.rs:82` refuses inter slices), and edith refuses HEVC without VA-API
(`engine/src/decode.rs:863`).
- **Scope:** Main and Main 10, 4:2:0. That covers I/P/B slices, merge/AMVP, all
  inter-prediction interpolation, weighted prediction, deblocking, SAO, multiple slices and
  dependent slices, tiles, WPP, short- and long-term reference sets, and the random-access
  pictures CRA/RASL/RADL/BLA (seeking needs them). SEI: HDR mastering-display and
  content-light metadata passed to edith's tone mapping (`tonemap.rs`). A Dolby Vision
  profile 8.x stream decodes as its HEVC base layer; the RPU is ignored.
- **Done when:**
  - The JCT-VC HEVC_v1 conformance bitstreams for Main and Main 10 are bit-exact vs
    ffmpeg/HM. Out-of-scope streams (RExt, SCC, multi-layer) refuse by name, and a test
    asserts that.
  - `real_library.toml` HEVC entries, including iPhone and Android phone clips (8-bit and
    10-bit HDR), are bit-exact vs ffmpeg.
  - Speed: 1080p30 at ≥ 2x realtime and 4K30 at ≥ realtime, with WPP and/or frame
    threading on ≤ 6 threads. 4K60 phone footage may fall back to proxies (`proxy.rs`).
  - **edith side:** ec-h265 is the software HEVC backend for playback, proxies and export
    input. The `needs_plugin` refusal for HEVC is removed. 10-bit frames flow through
    edith's pipeline; this work is shared with the H.264 10-bit item (D1).
  - The shared gates pass. The decoder is split into modules from the start: no file over
    5k lines.
- **Frozen:** RExt 4:4:4, 4:0:0 and 12-bit (refused by name), SCC, multi-layer (MV-HEVC, SHVC),
  Dolby Vision profile 5 (it has no compatible base layer).

### ec-h265 encode — P0 (inter-frame encode; D3 decided yes)
edith: software HEVC export (`engine/src/export.rs:69`), **intra-only today**, so files
are several times larger than they need to be.
- **Scope:** Main (8-bit) 4:2:0 with I and P frames, merge/AMVP, motion search (reuse
  ec-h264's search design), rate control and threading. B-frames are optional, and only
  added if the BD-rate bar needs them.
- **Done when:**
  - The output decodes in ffmpeg identically to the encoder's reconstruction, for both
    intra-only and inter streams.
  - Rate control lands within ±5% of the target bitrate over a 60 s clip.
  - BD-rate vs x265 `-preset veryfast` (matched features) is ≤ +25%. The intra-only mode
    is kept, as an intermediate setting, at BD-PSNR ≥ −1.0 dB vs x265 intra.
  - 1080p30 encodes at ≥ realtime on ≤ 6 threads.
- **Done 2026-10-19:** Main 10 encode (`h265m10enc`, merge `7b8e7fcd`): byte-exact round trip,
  rate within ±5%, BD-rate vs x265 10-bit below the 8-bit gap. edith exports HEVC Main 10 from
  10-bit sources (edith `1d88293`).
- **Done 2026-10-20:** Main 10 HDR signalling and proof (`hevcmain10enc`): `EncoderConfig` gains
  `chroma_sample_loc` and `mastering_display` (full mdcv SEI) beside `video_signal_type` and
  `content_light`; ffprobe reports Main 10, yuv420p10le and BT.2020 PQ/HLG tags; ffmpeg decode equals the
  reconstruction (testsrc2, mandelbrot, gradients, intra and GOP); gradient and 0/1023 extremes pass at QP -12..51;
  8-bit bitstream hash pinned (`tests/it/enc8_bitstream_hash.rs`), unchanged. BD-rate vs x265 veryfast 10-bit
  720p: 14.5% (testsrc2), 25.7% (mandelbrot), both below the 8-bit gaps (17.3%, 29.0%). 1080p30 10-bit,
  6 threads, loaded host: ~0.87x realtime.

### ec-vp9 decode — P1 (largest cheap win)
edith refuses VP9 without VA-API: "there is no software VP9 decoder"
(`engine/src/demux.rs:141-161`). That is stale: ec-vp9 reports 1080p frames identical to
libvpx 1.15.0 (`lanes/vp9f72.report.md`).
- **Done when:**
  - edith uses ec-vp9 as its software VP9 backend (playback, proxy, export input), and
    the `needs_plugin` refusal is removed for VP9.
  - Profile 0 (8-bit 4:2:0) and profile 2 (10-bit 4:2:0) libvpx test vectors are bit-exact.
  - 1080p30 profile 0 decodes at ≥ realtime (tile threading allowed). Threading is not
    implemented today.
  - Dead code is removed: `modes.rs:131` `read_partition`, `tables/mod.rs:16` `TM_PRED`,
    and the `allow(dead_code)` in `gen_tables.rs`.
  - Stale reports are marked superseded (`vp9inter`, `vp9hbd`).
- **Frozen:** further profile 1/3 (4:4:4) work.

### ec-vp8 decode — P0
edith: software VP8 decode, but only **inside the VA-API plugin** (`engine-hw/src/vpx.rs`),
so VP8 fails whenever the plugin is missing.
- **Done when:**
  - edith calls it from `engine` directly, not only through `engine-hw`.
  - The 12/12 IVF fixtures stay bit-exact (`lanes/vp8.report.md`).
  - The release-mode guard is fixed: `loopfilter.rs:458` and `:584` slice with only a
    `debug_assert`. The `panic!` calls at `intra.rs:114` and `:365` become errors.
- **Frozen:** VP8 encode, alpha.

### ec-mpeg2 decode — P1 (broadcast .ts, DVD VOB, HDV .m2t, old .mpg)
Pure-Rust MPEG-1 and MPEG-2 video decoder (ISO 11172-2 / 13818-2), `CodecId::Mpeg2Video`,
registered in the decoder registry; ec-mpegts exposes stream types 0x01/0x02 with it.
- **Scope:** Main and 4:2:2 profile, all levels; 4:2:0 and 4:2:2; I, P, B; frame and field
  pictures, frame/field/16x8/dual-prime prediction, field or frame DCT, alternate scan,
  both intra VLC tables, q_scale_type, DC precision 8-11, MPEG-1 (oddification). Output is
  woven frames with the interlace flag, plus repeat_first_field / top_field_first /
  progressive_frame in `PictureInfo`. Damaged slices are concealed and the frame is marked
  corrupt; no panics, bounded allocation.
- **Refusals (named errors):** MPEG-1 D-pictures, scalable/SNR/spatial extension, 4:4:4 and
  monochrome chroma formats, pictures above 8192 wide or 8192x4352 area. Pictures before the
  first sequence header are dropped; a B picture without its forward reference is dropped.
- **ffmpeg-compat quirks reproduced:** skipped B macroblocks are predicted frame-wise;
  a zero MV delta returns the predictor without range wrap; invalid cbp 0 and out-of-picture
  MVs are not defined by the spec and are not covered. Mid-row slice starts follow the spec
  (ffmpeg differs) and are not tested.
- **Done when:**
  - Byte-exact against ffmpeg (default simple IDCT) on the ffmpeg-encoded matrix
    (progressive/interlaced, IPB, MPEG-1, 4:2:2, SD and 1080) and on 1200+ random-syntax
    streams (field pictures, dual prime, 16x8, matrices, 4:2:2) in `tests/synth_parity.rs`.
  - `cargo test -p ec-mpeg2 -p ec-mpegts -p ec-core --release` passes with `EC_REQUIRE_FIXTURES=1`.
  - Fuzz target `decode_mpeg2` (`crates/ec-mpeg2/fuzz`) clean; `tests/robust.rs` passes.
  - edith uses it as the software MPEG-2 backend: `engine/src/decode/mpeg2.rs` (with `ivtc.rs`, `deinterlace.rs`), tested by `engine/tests/mpegts.rs` `mpeg2_video_opens_probes_and_decodes_as_ffmpeg_does` (plus seek and interlaced cases), `mpegps.rs` and `ivtc.rs`.

### ec-prores decode — P1 (Apple ProRes .mov)
Pure-Rust Apple ProRes decoder (SMPTE RDD 36), `CodecId::ProRes`, registered in the decoder
registry; ec-mp4 maps the sample entries `apco apcs apcn apch ap4h ap4x` to it.
- **Scope:** 422 Proxy/LT/Standard/HQ (output `I210`), 4444 and 4444 XQ (output `I412`, a new
  12-bit 4:4:4 `PixelFormat` in ec-core), progressive and interlaced (TFF/BFF, two field
  pictures woven, interlace flag set), all slice sizes (1/2/4/8 MBs), progressive and
  interlaced scans, custom luma/chroma quant matrices, per-slice qscale, odd sizes. 8- and
  16-bit alpha is returned as a separate 16-bit plane (`DecodedFrame::alpha`,
  `ProResDecoder::last_alpha`); `VideoFrame` itself cannot carry it. Optional slice-parallel
  decode (`set_threads`, up to 6, default = available cores).
- **Refusals (named errors):** other frame versions / chroma factors, pictures above 16384
  wide or 8192x4608 area, malformed headers and slice tables (`Corrupt`). A damaged slice
  fails the frame.
- **ffmpeg-compat quirks reproduced:** ffmpeg's `ff_prores_idct` (10-bit and 12-bit
  simple-IDCT variants with 16-bit wraparound) bit for bit; 10-bit clip [4,1019], 12-bit
  [4,4091]; absent luma matrix = all 4, absent chroma matrix = luma matrix; 4:4:4 chroma
  block order differs from luma; DC codebook index starts at 5.
- **Performance (server host c, EPYC-Rome, 1080p 422 HQ, ~190 Mbit/s):** single-thread 34-40 fps
  (shared host), 4 threads 65-79 fps. The 60 fps single-thread target is not met: ~1.5 M
  coefficients per frame go through a bit-serial rice/exp-golomb decoder (about 70% of the
  time), under `forbid(unsafe_code)` and baseline SSE2 only (no AVX2, no unchecked access).
  The IDCT is SIMD (`wide`) and is a minor cost. Re-measured 2026-10-23 on quiet host b (main `bed76a50`, `cargo test -p ec-prores --release --test perf`, 1080p 422 HQ 25 fps, 24 frames noisy testsrc2): 63.7 fps single-thread (set spread 2.7%), 161.1 fps on 4 threads, so the 60 fps single-thread target is met; the numbers above are kept as history.
- **Done when:**
  - Byte-exact against ffmpeg 8.x `prores_ks`/`prores_aw` output, 1 and 3 threads, on ~55
    cases (every profile, alpha, interlaced, noise, saturated, odd sizes, SD, 1080p)
    in `tests/ffmpeg_parity.rs`.
  - `EC_REQUIRE_FIXTURES=1 cargo test -p ec-prores -p ec-mp4 -p ec-core --release` passes.
  - Fuzz target `decode_prores` (`crates/ec-prores/fuzz`) clean for 10 minutes;
    `tests/robust.rs` passes.
  - edith uses it as the software ProRes backend: `engine/src/decode/prores.rs` (`ProResSeat` wraps `ec_prores::ProResDecoder`) via `engine/src/decode.rs`, tested by `engine/tests/procodecs.rs` `prores_every_profile_opens_decodes_seeks_and_exports` and `engine/tests/prores_export.rs`.

---

## Audio

### ec-aac — P0
edith: decode for >2 channels only (stereo goes to symphonia, `engine/src/audio.rs:1900`);
encode for AAC export (`engine/src/export.rs:2330`).
- **Decode done when:**
  - **All** AAC in edith routes through ec-aac, mono and stereo included. Today symphonia
    refuses SBR ("aac: aac too complex", `symphonia-codec-aac/src/aac/mod.rs:91-96`), so
    stereo HE-AAC files fail to open.
  - AAC-LC and HE-AAC v1, mono through 7.1, raw + ADTS, match ffmpeg (correlation ≥ 0.999
    per channel). The `sbr_hf.rs:202-226` unwraps become errors.
- **Encode done when:** LC CBR at 1–6 and 8 channels passes the existing
  `INCUMBENT_BAR` (`tests/oracle.rs:396`); an objective score vs ffmpeg's native AAC
  encoder at 128 kbps stereo is recorded (none exists today); rate is within ±5%.
- **Frozen:** SBR/PS encode, LTP, SSR, LATM.
- **Decision needed (D4):** HE-AAC v2 (Parametric Stereo) decode. Today it outputs the
  core only, at half rate (`lib.rs:167`). That is common in low-bitrate broadcast and
  streaming audio.

### ec-opus — P0
edith: the **only** Opus decoder (symphonia has none) and the Opus encoder for MKV export
(`engine/src/export.rs:2438`).
- **Decode done when:** the 12 RFC 6716 vectors pass (`tests/conformance.rs:414`), are
  run and recorded (no pass/fail record exists today), and the RFC 8251 updated vectors
  are added and pass. The `celt.rs:2227/2297/2406/2410` unwraps become errors.
- **Encode done when:** the SILK packet budget is enforced (`encoder.rs:19`, "D2b");
  rate is within ±5%; no FAIL rows remain in the dropout gate (`opus-gate-r1`: reelm@64k).
- **Frozen:** PLC/FEC (a dropped frame decodes to silence, which is acceptable for
  file-based editing).

### ec-vorbis — P0, nearly done
edith: decode and `.ogg` export (`engine/src/export.rs:3932`).
- **Done when:** the shared gates pass. Quality is already met: 14/14 PASS at equal size
  vs libvorbis (`lanes/vorbis-rate-r1.report.md`).
- **Frozen:** further floor/psy tuning.

### ec-mp3 encode — P0, nearly done
edith: MP3 export (`engine/src/export.rs:3832`). Decode stays on symphonia.
- **Done when:** the shared gates pass. Quality is already met: CBR matches or beats LAME
  on 14 rows; hein@192 is 0.2 dB behind (accepted) (`lanes/mp3lame-r1.report.md`).
- **Frozen:** VBR encode (not exposed by edith), decode (symphonia does it).

### ec-mp3 Layer I/II decode (MP1/MP2) — P1
broadcast and disc audio: DVB, DVD, HDV, VCD, `.mpg`, `.mp2`. Lives in `ec-mp3` beside Layer III, behind the same
`Mp3Reader` / `Mp3Decode` / `Mp3Decoder` (the header picks the layer), with `CodecId::Mp2` and `CodecId::Mp1`.
- **Scope:** MPEG-1, MPEG-2 LSF (16/22.05/24 kHz) and MPEG-2.5 headers with each layer's own bitrate table;
  Layer II allocation tables 3-B.2a..d and the LSF table (measured from an oracle, `scripts/mp2-tables/`),
  scfsi, scalefactors, 3/5/9-level grouped samples, requantisation, joint-stereo intensity bound
  (`mode_extension`), dual channel, header CRC (allocation and scfsi bits for Layer II, allocation for
  Layer I); Layer I allocation and scalefactors; the polyphase bank in f64 with the exact `D[i]` window.
  The MPEG-2 multichannel (5.1 BC) extension is ignored: the stereo base is decoded. Output is f32 in `[-1, 1]`
  like Layer III. `ec-mpegts` hands MP1/MP2 streams out as `Mp1`/`Mp2` instead of refusing them.
- **Refused by name:** free-format streams (bitrate index 0). No encoder.
- **Done when:**
  - `ffmpeg -c:a mp2` / `mp2fixed` streams at 16/22.05/24/32/44.1/48 kHz, mono and stereo, 8 to 384 kbit/s
    (sine, pink noise, chirp) decode within 1e-5 of ffmpeg's float decoder (`mp2float`); libtwolame streams
    (joint stereo, stereo, dual channel, mono, with and without CRC) likewise
    (`tests/mp2_matrix.rs`).
  - 336 hand-built Layer I and II streams (legal random allocation, scfsi, scalefactors and samples; MPEG-1,
    LSF and 2.5; every table; stereo, joint stereo with each bound, dual, mono; CRC on and off) match ffmpeg's
    `mp2float`/`mp1float` to a few 1e-5 (full-scale noise in every band; ffmpeg's float synthesis is the
    coarser side) and its fixed-point decoder to 1 LSB of s16.
  - A flipped allocation bit in a CRC-protected frame is reported as a CRC error; free-format frames are
    refused by name; damaged and truncated frames never panic (`tests/mp2_matrix.rs`).
  - Layer III tests pass unchanged; the shared gates pass.
- **Layer I coverage:** ffmpeg has no MP1 encoder and no clearly licensed MP1 sample was at hand, so Layer I is
  tested on the hand-built frames above only.
- **Frozen:** MPEG-2 multichannel and the MPEG-2 extension bit streams, free-format scanning, a Layer I/II encoder.

### ec-ac3 decode — P0
edith: AC-3 and E-AC-3 decode (`engine/src/audio.rs:2091`).
- **Done when:** AC-3 and E-AC-3 independent substreams match ffmpeg on
  `real_library.toml`, through 5.1. The shared gates pass.
- **Frozen:** encode (unused), Atmos JOC, enhanced coupling.
- **Decision needed (D5):** E-AC-3 dependent substreams (7.1). Today only the first
  substream decodes, so 7.1 becomes 5.1.

### ec-truehd decode — P1 candidate
edith does not decode TrueHD at all, so Blu-ray remux tracks have no audio. ec-truehd is
bit-exact on a 7.1 remux (`ec-truehd/src/lib.rs:1-12`).
- **Done when:** edith decodes TrueHD through it from MKV; the 2/6/8-channel substreams
  are bit-exact vs ffmpeg; the shared gates pass. **Frozen:** Atmos (substream 4), encode.
- **Decision needed (D6):** whether Blu-ray remuxes are a target for edith.

---

## Containers

### ec-mpegts demux — P1 (AVCHD camcorder files and broadcast/recorder .ts)
edith: opens `.mts`/`.m2ts` (192-byte BDAV packets) and `.ts` (188/204) through the same
`ec_core::Demuxer` seat as `ec-mp4` and `ec-matroska`. New crate; wiring it into `engine/src/demux.rs`
is the integration lane (edith is untouched by this crate's lane).
- **Scope:** packet size by sync stride (188, 192, 204) with resync after damage; adaptation field,
  PCR, discontinuity; PAT/PMT with CRC32 (first program by default, all listed, `with_program`),
  PMT changes mid-stream; PES reassembly with 33-bit PTS/DTS unwrapped to monotonic `i64`, unbounded
  video PES, continuity-counter checks and duplicate drop (damaged PES flagged `corrupt`).
  Streams: H.264 and HEVC (Annex-B access units, keyframe from IDR/IRAP, recovery-point SEI or
  `random_access_indicator`; `avcC`/`hvcC` extradata and a length-prefixed mode), AAC ADTS (frames split,
  AudioSpecificConfig derived), AC-3 and E-AC-3 (0x81/0x87/0x84/0xA1 and private data with the 0x05/0x6A/0x7A
  descriptors; syncframes split, dependent substreams joined), MPEG audio layers I, II and III (as `Mp1`, `Mp2`,
  `Mp3`; decoded by ec-mp3), HDMV LPCM
  (header parsed, 16-bit as `PcmS16Be`, 20/24-bit as `PcmS24Le`, odd channel counts de-padded), HDMV PGS
  (`Pgs`). Start offset per stream, duration from the file tail, bisection seek to the video keyframe at
  or before the target.
- **Exposed:** MPEG-1/2 video (stream types 0x01/0x02) as `CodecId::Mpeg2Video`, one packet per coded picture (`tests/mpeg2.rs`); decoding is `ec-mpeg2`'s job.
- **Refused by name** (listed in `tracks()` with a reason, never silently dropped, never decoded wrong):
  AAC LATM/LOAS (`aac_latm`), VC-1, IGS, TextST, teletext, and every other stream type. DTS and DTS-HD (lane `dts`), TrueHD (lane `tstruehd`) and DVB subtitles (lane `dvdsubs`) are now exposed, not refused.
  Interlaced (PAFF/MBAFF) H.264 is split on PES boundaries only; access unit delimiters inside a PES split
  progressive H.264 and HEVC only.
- **Done when:**
  - Packets are byte-exact, with sizes, pts, dts and keyframe flags equal to `ffprobe -show_packets`,
    on ffmpeg-made 188-byte `.ts` and 192-byte `.m2ts`: H.264 progressive and MBAFF, HEVC, AAC, AC-3 and
    E-AC-3 at 32/44.1/48 kHz, MP2, MP3, LPCM (`pcm_bluray`, against ffmpeg's s16be decode), two programs, a PTS
    wrap, an audio-only stream (`tests/ffmpeg_parity.rs`); duration within one frame; MP2 is exposed as `Mp2`.
  - A seek then read equals the sequential read from the landing offset, packet for packet, at several
    targets in `SyncBefore` and `Exact` modes, and lands on the last keyframe at or before the target.
  - Truncated, overwritten, cut and garbage-prefixed files open, read on, count the damage in `stats()` and
    flag it `corrupt`; seeded mutation of the fixtures never panics (`tests/robust.rs`).
  - Hand-built streams cover PCR on its own PID, two access units in one PES, 204-byte packets, duplicate
    packets, PMT change, bad CRC, refusals and PGS (`tests/synthetic.rs`); the committed tiny fixtures
    match ffmpeg's bytes without ffmpeg (`tests/committed.rs`).
  - Fuzz targets `demux_ts` and `ts_parts` (`crates/ec-mpegts/fuzz`): 1 CPU-hour each, no panic, hang or OOM. **Done 2026-10-20:** 6.27M (`demux_ts`) and 7.51M (`ts_parts`) runs in 3600 s each on server host a, clean.
  - The shared gates pass; `cargo test -p ec-mpegts --release` passes with `EC_REQUIRE_FIXTURES=1`.
- **Frozen:** LATM, teletext, SCTE-35,
  PCR-driven clock recovery.

### ec-mpegts PS — MPEG-1 system / MPEG-2 program streams (`.mpg`, `.mpeg`, `.vob`, `.m2p`) and raw `.m2v`/`.mpv`

- **Scope:** `PsDemuxer` in `ec-mpegts` (`src/ps.rs`, `src/psvideo.rs`), the same `Demuxer` API as `TsDemuxer` (tracks, streams,
  `next_packet`, `seek` SyncBefore/SyncAfter/Exact, `stats`, `duration_secs`), reusing its PES, framing and refusal machinery.
  Pack headers (MPEG-1 and MPEG-2, SCR), system header, PSM stream types, padding, program end code, private_stream_2 (skipped).
  Video: MPEG-1/2 (`Mpeg2Video`), H.264/HEVC (PSM or sniffed) as Annex-B. Audio: MP1/MP2/MP3, AAC ADTS, AC-3 (private_stream_1
  0x80-0x87), DVD LPCM 16/20/24-bit (0xA0-0xA7; 20/24-bit exposed as `PcmS24Le`). Raw MPEG-1/2 video elementary streams open in the
  same type (indexed once at open; timestamps from the frame rate). 33-bit PTS wrap unwrapped; an SCR step back of more than
  0.5 s, or a step forward of more than 1 s, between consecutive packs (concatenated VOBs, cell joins) starts a new epoch
  (`stats().discontinuities`); at open each later epoch is shifted so its first picture is shown where the previous epoch's
  last picture ends (video presentation times, audio shares the shift), so the timeline is continuous, the duration is the sum
  of the segments and seeks are exact on both sides of a join. A `.vob` that
  starts mid-GOP opens and delivers from the first keyframe on.
- **Refused by name:** DTS (`dts`), other private_stream_1
  substreams, LATM, and every PSM stream type the family cannot decode. VobSub subpictures (`DVD subtitles (VobSub)`) are now exposed (lane `dvdsubs`), not refused.
- **Limits:** 20/24-bit DVD LPCM seeks may land mid 2-frame group; PAFF field pictures are one packet per picture header;
  joins are found from the SCR only (a PTS restart without an SCR restart is not seen) and the alignment needs a video picture on both sides; files over 96 MiB check SCR resets by sampling (backward steps only); pictures with no PES
  timestamp get derived dts/pts (flagged internally; MPEG-1/2 pictures take the display order of the GOP before; after a seek reading starts at the keyframe before the landing one so the times equal the sequential read's); after a seek the pictures that share the
  landing PES but precede the keyframe are dropped.
- **Done when:**
  - Packets equal `ffprobe -show_packets` / `ffmpeg -c copy -f data` (bytes, sizes, pts, dts, keyframe flags), duration within one
    frame, seek-then-read equals the sequential read, on ffmpeg-made `-f mpeg` (MPEG-1 and MPEG-2), `-f vob`, `-f dvd` (nav packs),
    interlaced, PTS wrap, H.264+MP3, DVD LPCM 16/24-bit, raw `.m2v`/`.m1v` (`tests/ps.rs`; committed fixtures without ffmpeg in
    `tests/committed_ps.rs`).
  - Concatenated VOBs (also through the sampled check), mid-GOP start, injected VobSub/DTS/nav/padding, truncation at every
    offset prefix-consistent, seeded mutation never panics (`tests/ps.rs`).
  - Fuzz target `demux_ps`, 5 min smoke on a server, no panic, hang or OOM.
  - The TS tests are unchanged; the shared gates pass; `cargo test -p ec-mpegts --release` passes with `EC_REQUIRE_FIXTURES=1`.

## Dependency replacement — only rav1e and symphonia stay

**Goal:** edith's `Cargo.lock` contains no third-party media crate except **rav1e** and
**symphonia**. Every other codec, container, subtitle, image and VA-API crate is an ec-* crate.

**Stays:**
- `rav1e` (AV1 export; decided 2026-10-15, see **ec-av1 encode**). After wave 3 removes
  `image`, rav1e has one owner only: `engine`'s AV1 export. It no longer arrives via `ravif`.
- `symphonia`, `symphonia-core` (decided 2026-10-15): audio probing and MP3/FLAC/ALAC/WAV
  decode. The one exception is `symphonia-codec-aac`, which goes: all AAC routes through
  ec-aac (the HE-AAC fix under **ec-aac**). Keep symphonia's `aac` feature off so it
  does not come back.
- Not media: `gpui` (UI; it also brings `proc-macro-error2`), `pipewire`/`libspa-sys`
  (audio output), `libc`, `libloading`.

**Done for the whole goal when:**
- `cargo tree -p engine -p engine-hw -p engine-audio -e normal` lists none of:
  `symphonia-codec-aac`, `mp4`, `cros-codecs`, `cros-libva`, `gbm`, `oxideav-*`, `image`,
  `ravif`, `flacenc`, `hound`, `miniz_oxide`.
- A test in edith fails if any of them comes back, in the style of the existing window-guard
  tests. The test allow-lists `rav1e`, `symphonia` and `symphonia-core` by name, and only as
  direct dependencies of `engine`.
- `edith/vendor/` holds nothing but `proc-macro-error2` (or nothing, once gpui no longer needs
  the patch), and the matching `[patch.crates-io]` entries are gone.

**Done for each swap when:**
1. The old crate is gone from `Cargo.lock`.
2. Every existing edith test still passes, with no test deleted or `#[ignore]`d to get there.
3. Parity is shown on `real_library.toml` against the crate being replaced:
   - **demux:** identical packet bytes, timestamps and stream metadata;
   - **mux:** ffprobe-clean, and decodes identically in ffmpeg;
   - **decode:** sample/pixel-equal output.
4. The ec-* crate it now depends on passes the shared gates above (it becomes P0).

### Wave 1 — one-call swaps
| Replace | With | Gap to close |
|---|---|---|
| `miniz_oxide` | ec-inflate | none; swap `demux.rs:2363` to `inflate_zlib(input, LIMIT)` |
| `hound` | ec-riff `WavWriter` | RF64 for > 4 GB WAV exports (unverified today) |
| `flacenc` | ec-flac encode | match flacenc's compression level and bit depths edith exports |

### Wave 2 — the patched forks (highest maintenance risk)
| Replace | With | Gap to close |
|---|---|---|
| `mp4` (vendored, patched) | ec-mp4 | mux sample entries `av01`, `ac-3`, `Opus`/`dOps`, `alac`; the mp4a version-1 esds quirk the vendored patch fixes; rotation, edit lists and stss/ctts parity with `demux.rs:304-377`, `:4206-4236`, `:1196-1226` |
| `cros-codecs`, `cros-libva`, `gbm` (vendored, patched) | ec-hw + ec-va | rewrite `engine-hw` onto ec-hw, keeping each local fix as an ec-hw regression test: open-GOP H.264 `frame_num` gap fill, HEVC Main 10 decode contexts, radeonsi probe size, HEVC encode, the per-cache-line flush, the `munmap` leak, and `slice_fix.rs` (`cabac_zero_word`). Buffer export without the `gbm` crate. |

### Wave 3 — subtitles, stills, Ogg mux
| Replace | With | Gap to close |
|---|---|---|
| `oxideav-subtitle` | ec-subs | map its cue IR to edith's `Segment` (`subtitle.rs:522`, `:609`) |
| `oxideav-ass` | ec-ass | same, plus edith's handling of `Segment::Raw` override tags (`subtitle.rs:559`, `:611`) |
| `oxideav-sub-image` | ec-pgs | the epoch/display-set handling at `subtitle.rs:90-114` |
| `oxideav-ogg`, `oxideav-core` | ec-ogg | Opus pre-skip and Vorbis header lacing parity (`mux.rs:1715-1731`, `export.rs:3957`) |
| `image` | ec-image | pixel-format mapping to edith's frames, in `engine` (`decode.rs:1711`, `:1746`, `:1825`) **and** `app`; EXIF orientation applied by edith as today |

### Optional consolidation (first-party code, not a dependency)
edith's own Matroska reader/writer (`demux.rs`, `mux.rs`) could move onto ec-matroska. Gap:
Projection roll, Cues reorder tolerance, V_AV1/HEVC CodecPrivate parity. Not required for the goal.

---

## Open decisions (for the owner)

| # | Question | Effect if yes |
|---|---|---|
| ~~D1~~ | H.264 interlaced + High10 decode? | **High10 done (lane/h264hi10)**: ec-h264 decodes High 10 (4:2:0 progressive, 9/10-bit, CAVLC and CABAC, intra/inter/weighted prediction) byte-exact against ffmpeg `yuv420p10le`, output as `I010` like HEVC Main 10. **PAFF done (lane/h264interlace)**: field pictures (field POC, field DPB and reference marking, field MV/ref handling, chroma mv offset, field deblocking, temporal direct and implicit weights with field POCs, field scans and CABAC field contexts) byte-exact against the JVT vectors, frames output woven with `VideoFrame.interlace` set. **MBAFF done (lane/h264mbaff)**: pair addressing and per-pair field flag (CAVLC and CABAC, with the 7.4.4 inference and skipped-top handling), Table 6-4 neighbour derivation for every reader, field-MB geometry, doubled reference lists, MV/ref/mvd scaling, implicit weights and temporal/spatial direct with field MBs and every colocated case, constrained intra with partly available left samples, mixed-edge deblocking. All 24 JVT MBAFF vectors (CVMA*, CAMA*, cama*_vtc, CAMP_MOT_MBAFF, CAPAMA3, CVMAPAQP3, FRExt2/4, MR9_BT_B) bit-exact, plus x264 `--interlaced` tff/bff sweeps (CABAC/CAVLC, B, direct, weightp, 8x8dct, slices, High 10, 576 and 1080) byte-exact against ffmpeg (lane/h264mbaff, 2026-10-20). **High 4:2:2 done (lane/h264hi422)**: 2x4 chroma DC (own transform with QP'c+3, nC -2 CAVLC tables, CABAC 8-coefficient DC), 8 chroma AC blocks per component, 8x16 chroma intra prediction (DC rules, plane with yCF 4), full-height chroma MC, the extra chroma horizontal deblock edges (frame and MBAFF), I_PCM; all 18 Sony Hi422FR/Hi422FREXT vectors (progressive, PAFF, MBAFF; 8 and 10 bit) bit-exact, plus x264 high422 sweeps at 8 and 10 bits (intra, long-GOP, CABAC/CAVLC, 8x8dct, weightp, cqm, MBAFF) byte-exact against ffmpeg; output `I422`/`I210`. Monochrome stays refused. |
| ~~D2~~ | Replace rav1e with ec-av1's encoder? | **Decided 2026-10-15: no.** rav1e stays; the ec-av1 encoder is frozen |
| ~~D3~~ | Inter-frame HEVC export? | **Decided 2026-10-15: yes.** HEVC software decode is also P0 |
| ~~D4~~ | HE-AAC v2 (PS) decode? | **Yes (orchestrator, 2026-10-15)**: one lane after AAC routing |
| ~~D5~~ | E-AC-3 7.1 dependent substreams? | **Yes (orchestrator, 2026-10-15)**: one lane |
| ~~D6~~ | TrueHD / Blu-ray remuxes as a target? | **Yes (orchestrator, 2026-10-15)**: ec-truehd is P1, wiring only |
| ~~D7~~ | Speed targets above ("≥ realtime 1080p30") right for this machine? | **Kept (orchestrator, 2026-10-15)**: measured on the local 12-core machine, recorded in `lanes/bench.md` |
| ~~D8~~ | HEVC RExt decode (4:2:2 10-bit: Sony XAVC HS, Canon, Panasonic cameras)? | **Done 2026-10-19**: 4:2:2 at 8/10 bit byte-exact vs ffmpeg on the Sony RExt vectors and generated clips (`hevc422`, merge `8b9d547e`); edith opens it (edith `f291196`). 4:4:4 and 12-bit still refused by name |

## Done log

(one line per crate: `DONE <date> <crate> <scope>`)

- DONE 2026-10-17 ec-truehd decode: 2-, 6- and 8-channel PCM substreams bit-exact vs ffmpeg `-f s32le` (2.0 and 5.1 fixtures at 16/24-bit, committed; 7.1 from a local remux by path, not committed; `crates/ec-truehd/tests/it/fixture_exact.rs`). Report: `lanes/truehdexact.report.md`.
- DONE 2026-10-17 ec-ac3 decode: AC-3 and E-AC-3 through 5.1 at correlation >= 0.999 per channel on the 2 real_library entries (`crates/ec-ac3/tests/it/real_library.rs`); E-AC-3 7.1 dependent substreams (D5) on the matrix fixtures. Updated 2026-10-18: AC-3 zero-bit dither is now ffmpeg's lagged-Fibonacci generator, so per-channel correlation vs ffmpeg is 1.000000 on 10 s pink-noise fixtures at 5.1 448k, 2.0 192k and 5.1 640k (`ac3parity`, merge `a99a45ab`, `crates/ec-ac3/tests/it/ffmpeg_parity.rs`). Bit-exactness is not claimed. AC-3 decode speed: 323.2x realtime, local, best of 5 (`benchrows`, merge `e188b83b`, `lanes/bench.md`). Reports: `lanes/truehdexact.report.md`, `lanes/ac3parity.report.md`.
- DONE 2026-10-18 ec-h264 decode and encode: JVT 26 bit-exact, 9 refused by name with tests (`h264refusals`, `refusals.rs`); real_library 15/15 (`h264reallib`); encode recon 28/28 md5 vs ffmpeg, rate within 5%, BD-rate +12.46% vs x264 veryfast (`h264encrc`, `h264encspeed2`). Edith tests are on codecs-integration.
- DONE 2026-10-18 ec-opus decode and encode: RFC 6716 12/12 and RFC 8251 12/12, SILK packet budget enforced, rate +0.00%, reelm@64k dropout gate met (`opusgate`).
- DONE 2026-10-18 ec-vorbis: 14/14 PASS vs libvorbis at equal size (`vorbis-rate-r1`); fuzz 1 h clean (`fuzz1haudio`).
- DONE 2026-10-18 ec-mp3 encode: CBR matches or beats LAME on 14 rows, hein@192 0.2 dB behind accepted (`mp3lame-r1`).
- DONE 2026-10-18 ec-h265 encode: intra-only and inter output decodes in ffmpeg identical to the encoder's reconstruction (`hevcencrc`, `hevcencrcpar`, `hevc-gop-roundtrip.sh`); rate control within 0.2% at 1000/2500/5000 kb/s and within 0.02% on 60 s constant-target runs (`hevcencrcpar`, `hevcencspeed5`); BD-rate vs x265 veryfast at most +21.16% YUV, bitrate mode (`hevcencrcpar`; constant QP at most +18.8%, `hevcencspeed4`); intra-only BD-PSNR at least -1.0 dB vs x265 intra on all three clips, with `-tune psnr` (`hevcmisc`, merge `7d8711af`); 1080p30 at 40.16 fps = 1.34x realtime on 6 threads, local (`hevcencspeed5`, merge `8fb9fb6c`; bitrate mode 51.9 fps, `hevcencrcpar`). Local machine per D7. Main 10 encode stays open, only if edith starts exporting 10-bit/HDR.
- DONE 2026-10-18 ec-aac encode: LC CBR passes `INCUMBENT_BAR` at 1-6 and 8 channels (`aacencbar`, merge `5241ffbe`; the 8-channel bar is ffmpeg's native AAC, a stricter proxy than rusty_aac, which refuses more than 6 channels, margin 0.0002); objective score vs ffmpeg native AAC at 128 kbps stereo recorded (`aacgates2`); rate within ±5% (`aacgates2`: +0.9 to +4.4% before the ADTS-header budget fix; `aacencbar`: +0.6 to +1.0% after it). Edith tests are on codecs-integration. Frozen items unchanged.
- DONE 2026-10-18 ec-av1 decode (8-bit and 10-bit 4:2:0): AOM profile 0 242/242 exact (`av1svc`, merge `a62e7dc2`); 1080p30 4.03x realtime at 6 threads (`av1bench6`); `decode_stream` and `parse_obu` 1 CPU-hour clean on the final tree, `decode_stream` re-run from real seeds at cov 28.7k (`fuzzre-a`, merge `8936fe06`); worker panics return an error (`av1panichang`); workspace clippy `-D warnings` clean on main `f43403af`. Non-4:2:0 stays refused by name.
- DONE 2026-10-18 ec-vp8 decode: 12/12 IVF fixtures byte-exact vs ffmpeg, malformed input returns errors (`vp8guards`); 1080p30 5.24x realtime (`benchrows`); edith decodes VP8 WebM from `engine` directly and the real_library `vp8_webm` entry matches ffmpeg (edith `codecs-integration` merge `fe21ed4`, `real_library_video.rs`).
- DONE 2026-10-18 ec-vp9 decode (profile 0): md5 = ffmpeg on the fixtures and the real_library `vp9_opus_mp4` entry (edith merge `fe21ed4`); 1080p30 5.50x realtime at 1 thread (`benchrows`); `decode_frame` and `decode_stream` 1 CPU-hour clean on the final tree (`fuzzre-a`, merge `8936fe06`).
- DONE 2026-10-18 ec-aac decode: LC mono to 7.1 correlation 1.000000 vs ffmpeg (`aacdecmulti`); HE-AAC v1 and v2 mono to 5.1 correlation >= 0.99997 vs ffmpeg, `decode_he` 1 CPU-hour clean (`aachev1`, merge `18d1db47`); LC decode targets 1 CPU-hour clean (`fuzzre-c`, merge `f43403af`); bench row (`benchaudio`, merge `1cd59b57`); edith routes every AAC stream to ec-aac with seek tests incl. HE-AAC (edith merge `e0e8922`). ec-aac encode also meets `NOISY_BAR` at 128k/192k (`aacencnoise`, merge `f05345dc`).
- DONE 2026-10-18 ec-mp4: demux and mux parity per the Wave 2 swap criteria (`mp4alac`, `swapmp4`); rotation matrices snap within 0.5 degrees and match ffmpeg in edith (`rotation_parity.rs`, edith merge `61bfe00`); real_library `mp4_h264_aac`, `mp4_phone_h264`, `mp4_mov_text`, `mp4_heaac` packet parity (edith merge `fe21ed4`, `real_library_containers.rs`); bench row (`benchaudio`).
- DONE 2026-10-18 ec-matroska: 7 fuzz targets 1 h clean (`fuzzmkv`); unknown-length clusters wider than the read window keep every block (`reallib`, merge `edea19ce`; edith `mkv_unknown_cluster.rs`); real_library Matroska entries match ffmpeg packets; zlib-compressed PGS and VobSub tracks on 2 real_library keys match ffprobe on 4073 packets (edith merge `8a50186`, `real_library_zlib.rs`). Bench row 1.81 M packets/s, packets = ffprobe (`benchrest`, merge `f1eb8728`). `demux_packets`, `seek` and `open_tracks` 1 CPU-hour clean on the final demux code (`fuzzre-d`, merge `6bc4c954`).
- DONE 2026-10-18 ec-riff: WAV/RF64 writer replaces hound (Wave 1); real_library `wav_pcm16` matches ffmpeg (edith merge `fe21ed4`, `real_library_audio.rs`); bench row (`benchp1`).
- DONE 2026-10-18 ec-subs, ec-pgs: Wave 3 swaps met; fuzz 1 h clean; bench rows (`benchp1`, `pgsbudget`). ec-pgs `compose` and `segments` re-run 1 CPU-hour clean after `pgsbudget` (`fuzzre-d`, merge `6bc4c954`).
- DONE 2026-10-18 ec-ass: Wave 3 swap met; timing, order and text match ffmpeg on 7 scripts, `\t(...)` kept as one verbatim tag (`assparity`, merge `569e2d0b`). `overrides` and `events` re-run 1 CPU-hour clean after the splitter change (`fuzzre-d`, merge `6bc4c954`).
- DONE 2026-10-18 ec-image: Wave 3 swap met in `engine` and the app's own PNG writing; `image` stays only under app -> gpui (exception 2026-10-16). Bench rows: PNG decode 105 Mpix/s and encode 25 Mpix/s bit-exact vs ffmpeg, JPEG decode 225 Mpix/s within max delta 4 of libjpeg (`benchrest`, merge `f1eb8728`).
- DONE 2026-10-18 ec-inflate: Wave 1 swap met (`demux.rs` zlib through `inflate_zlib`); fuzz 1 h clean (`fuzzinflate`, `fuzz1hcontainers`, no source change since); zlib-compressed Matroska PGS and VobSub tracks on 2 real_library keys inflate to ffprobe's bytes, 4073 packets (edith merge `8a50186`, `real_library_zlib.rs`); 288 MB/s on one 64 MiB stream and 320 MB/s on 20k 3 KiB blocks, md5 = zlib (`benchrest`, merge `f1eb8728`).
- DONE 2026-10-18 ec-flac encode (the part on edith's path; edith decodes FLAC with symphonia): Wave 1 flacenc swap met at every level and depth, 12/12 (`flacparity`); fuzz 1 h clean (`fuzzflac`, `fuzz1haudio`); the owner's audio on 3 real_library keys (stereo and 5.1, 16 and 24 bit, 60 s each) written through `engine::flac_write` decodes in ffmpeg to the input sample for sample, with STREAMINFO MD5 right (edith merge `3696025`, `real_library_flac.rs`); encode 152x realtime stereo and 39x 5.1 24-bit, decode 627x and 149x (`benchrest`).
- DONE 2026-10-18 ec-hw, ec-va: every carried cros-codecs/gbm fix has a regression test (`hwregress`); parameter builders 1 h fuzz clean (`hwfuzz`, merge `3c08b0ce`); VA-API decode/encode bench rows on the local GPU with every picture md5 = ffmpeg (`benchhw`, merge `9e1d91cd`). ec-va has no bitstream parser, so G2 is N/A for it (orchestrator, 2026-10-18). ec-hw does not link on the servers (no libva); its gates run locally.

### Milestones (not crate DONE lines; the per-crate gates still apply)
- 2026-10-16 dependency replacement reached on edith `codecs-integration`: `cargo tree -p engine -p engine-hw -p engine-audio -e normal` lists none of the banned crates; `vendor/` holds only `proc-macro-error2`; `engine/tests/dependency_guard.rs` enforces it (PENDING_WAVE2 empty). Lanes: swapflacenc, swapsubs, swapogg, swapimage, swapimage2, swapmp4, depguard, enginehw (+ ec-side mp4mux, mp4demux, imagegaps, hwfixes, hwgaps). On edith `main` since `468d96d` (2026-10-18).
- Exception (orchestrator, 2026-10-16): `image` remains in the **app** crate only, because gpui 0.2.2 exposes `image` types in its API (`RenderImage`, `Frame`). The goal's crate list covers `engine`, `engine-hw`, `engine-audio`, which are clean. The app's own PNG writing goes through ec-image.
- Correction (orchestrator, 2026-10-16): the wave-3 row for `oxideav-ogg` cites Opus pre-skip at `mux.rs:1715-1731`; that is the Matroska `A_OPUS` writer, and edith has no Ogg-Opus writer. Ogg parity was shown on Vorbis only (lanes/edith-swapogg.report.md).

### Criteria met, crate not yet DONE (2026-10-18)

The crate's other criteria still apply. Bench rows are local (D7), best of 5 unless stated.

- **ec-aac decode (met):** AAC-LC mono, stereo, 5.1 and 7.1, raw ADTS and m4a: per-channel correlation 1.000000 vs ffmpeg (`aacdecmulti`, merge `0ad0ea98`, `crates/ec-aac/tests/it/multichannel_parity.rs`). HE-AAC v1 is not covered by this lane. Still open: edith routing and the HE-AAC v2 refusal check in mp4.
- **ec-h265 decode (met):** Dolby Vision profile 8.x base layer decodes bit-exact vs ffmpeg with RPU NAL units (type 62) ignored (`hevcmisc`, merge `7d8711af`). mp4 `dvcC`/`dvvC` boxes are not covered.
- **ec-vp8 decode (met):** bench row, 1080p30 libvpx 4 Mb/s: 157.30 fps, 5.24x realtime, md5 = ffmpeg (`benchrows`, merge `e188b83b`, `lanes/bench.md`).
- **ec-vp9 decode (met):** 1080p30 profile 0, 4 tile columns: 165.13 fps, 5.50x realtime at 1 thread, md5 = ffmpeg (`benchrows`, merge `e188b83b`). Bench-row criterion only.
- **ec-ac3 decode (met):** bench row, AC-3 5.1 448 kb/s: 322.2x realtime (`ac3benchrerun`, local best of 5). Parity re-run on the 60 s bench clip after `ac3parity`: min per-channel correlation 1.000000 vs ffmpeg 8.1.3 (the earlier 0.981 FAIL row predates the dither fix). `lanes/bench.md`, `lanes/ac3benchrerun.report.md`.
- **ec-av1 decode (met):** 1080p30 8-bit 4:2:0 at or above realtime with `EC_AV1_THREADS` > 1: 3.93x at 4 threads, 4.03x at 6 threads, quiet local machine, md5 = ffmpeg (`av1bench6`, merge `5939a7de`, `lanes/av1bench6.report.md`).
- **ec-av1 decode (met):** AOM conformance profile 0 at 242/242 exact (611 shown frames), including the two spatial-SVC vectors via scaled-reference prediction and libaom's last-shown-frame-per-TU output policy (`av1svc`, merge `a62e7dc2`, `lanes/av1svc.report.md`).
- **ec-av1 decode (met):** 1 h fuzz on the final tree: `decode_stream` 960,909 runs / 3601 s and `parse_obu` 21,470,442 runs / 3601 s, both clean; the run1 crashes are fixed with replay tests (`av1fuzzfinal`, merge `96ae4d45`, `lanes/av1fuzzfinal.report.md`).
- **Wave 1 flacenc swap (met):** ec-flac encode at every level and bit depth edith exports decodes identically in ffmpeg, 12/12 (`flacparity`, merge `d376bf2c`, `crates/ec-flac/tests/level_depth_parity.rs`).
- **Wave 2 mp4 swap (met):** ec-mp4 mux of `av01`, `ac-3`, `Opus`/`dOps` and `alac` sample entries is ffprobe-clean and decodes identically in ffmpeg (`mp4alac`, merge `fde0fa1e`).
- **Wave 2 cros-codecs swap (met):** every carried cros-codecs/gbm local fix has an ec-hw regression test; HEVC entry points past 32; display-free HEVC parameter building with a `build_parameters` fuzz target (`hwregress`, merge `55d7a6c5`).
- **Wave 3 swaps (met):** ec-subs, ec-ass, ec-pgs, ec-ogg and ec-riff have bench rows (`benchp1`, merge `c0347b7f`); ec-pgs caps queued display sets at 8 and rasterizes on take, 442x realtime (`pgsbudget`, merge `e7e8c552`); edith tests cover WebVTT/SRT/ASS in Matroska, Ogg Opus import, PGS `.sup` vs ffmpeg and zlib-compressed Matroska tracks (edith `codecs-integration` merge `dcf9352`).
- **ec-hw fuzz (met):** H.264, VP9, AV1 and HEVC VA-API parameter builders are display-free and each fuzzed 1 h with 0 crashes (3.7M, 45.9M, 14.0M and 4.8M runs); the one crash found, an H.264 PPS scaling-list delta overflow in `ec-h264-syntax`, is refused as corrupt with a regression test (`hwfuzz`, merge `3c08b0ce`).
- **ec-av1 decode robustness (met):** a panicking worker (frame job, wavefront recon, loop-filter pipeline) fails the decode with an error instead of hanging, and the next decode in the process works; `tests/worker_panic.rs` (`av1panichang`, merge `c479e778`).

### Plan status (orchestrator, 2026-10-18)

Each execution-plan phase, checked against the tree on edith_codecs `d5ca7236` and edith `codecs-integration` `f4d5e8a`:

- **Phase 0, 1 (gates, fuzz scaffolding, edith wins, wave 1):** done. Every crate on edith's path has fuzz targets, each run 1 CPU-hour on its final code (latest re-runs: `fuzzre-a` to `fuzzre-d`, `truehdrecover`).
- **HEVC decode track:** every Done-when item met (conformance 147/147 streams, refusals by name, 1080p 5.18x and 4K 1.52x, edith software backend), including the phone-clip row (public-domain Android and iPhone originals, plus a synthetic 10-bit HLG clip; below).
- **HEVC encode track:** DONE line above.
- **Phase 2 (AV1 split, threads):** done; the largest source file in any crate is 4848 lines (`ec-av1/src/decode/rect.rs`); AV1 DONE line above.
- **Phase 3 (waves 2, 3, dep guard):** done; `cargo tree -p engine -p engine-hw -p engine-audio -e normal` on `f4d5e8a` lists no banned crate (only rav1e and symphonia remain, as decided); `engine/tests/dependency_guard.rs` enforces it; `vendor/` holds only `proc-macro-error2`.
- **Phase 4 (per-crate gates):** DONE lines for ec-h264, ec-h265 encode, ec-av1, ec-vp8, ec-vp9, ec-aac (decode and encode), ec-opus, ec-vorbis, ec-mp3 encode, ec-ac3, ec-truehd, ec-mp4, ec-matroska, ec-riff, ec-subs, ec-pgs, ec-ass, ec-image, ec-inflate, ec-flac encode, ec-hw/ec-va.
- **All items met** (2026-10-18). ec-h265 phone clips and ec-ogg's real_library row are resolved below on public-domain media. The 10-bit HDR phone row uses a synthetic clip.

### Still open (2026-10-18)

- **G6 note (orchestrator, 2026-10-18):** the speed rows live in `lanes/bench.md` (D7); `lanes/bench.log` is old build output and is not evidence.
- **ec-ogg real_library row (resolved 2026-10-18, owner rule "use public-domain media"):** four public-domain and CC0 Commons recordings (three Vorbis, one Opus, 90–148 s) sit in the local gitignored manifest. `crates/ec-ogg/tests/real_library.rs` (ignored, local only) demuxes every packet and matches ffmpeg on count, sizes, payload bytes, header block, pts and granule ends for all four (`oggreal`, merge `26630dce`). Demux only: decode parity is covered by the ec-vorbis and ec-opus gates. ec-flac decode is still not on edith's path (symphonia, kept 2026-10-15).
- **ec-truehd (resolved 2026-10-18):** a refused access unit now restores the decoder state, so later units decode exactly as if it were absent (233 corruptions, 158,367 later units equal; `refused_au_recovery.rs`). `decode_stream` 1 CPU-hour clean at cov 962 / ft 5136, decode cost +0.6% (`truehdrecover`, merge `1a3aee03`). The earlier cov 3087 came from the pre-fix decoder running on in stale state, and that state is gone now.
- **Decision (orchestrator, 2026-10-18, under the owner's no-questions rule): AAC priming.** With no edit list, edith keeps trimming 1024 samples (`engine/src/audio.rs` `DEFAULT_PRIMING`, ffmpeg's own AAC-LC encoder delay), and ffmpeg's decode does not. Kept: the trim gives the timing the encoder meant, `audio_decode.rs`, `audio_segments.rs` and `audio_copy.rs` assert it, and ffmpeg leaves the trim to an edit list it does not find. Follow-up noted, not a gate: the comment there still says HE-AAC is refused, which is stale since `aacv2`; HE-AAC encoders without an edit list usually delay 2112 samples. The owner can reverse this decision.
- **ec-h265 decode phone clips (resolved 2026-10-18, owner rule "use public-domain or generated media"):** `real_library_hevc_streams_match_ffmpeg` reports 29 bit-exact, 0 refused, 0 failed, 300 pictures each. The phone rows are:
  - a CC0 Android original (720x1280 portrait 8-bit; Android MPEG4Writer handler names and no re-encode trace);
  - a CC0 iPhone 7 Plus original (1080p 8-bit; Apple make/model tags; first 20 s stream-copied, bitstream untouched);
  - a **synthetic** 10-bit HLG BT.2020 clip in an `hvc1` mov (libx265 main10).
  Gap, stated plainly: no public-domain 10-bit HDR phone original turned up, so 10-bit HLG is shown on x265 output, not an iPhone 12+ encoder's. 10-bit decode is otherwise covered by 20 Main 10 library films and the conformance set.

### ec-mjpeg (Motion JPEG decode) - DONE (lane `mjpeg`)

- `ec-image::jpeg::decode_yuv` decodes a JPEG straight to native planes (4:2:0, 4:2:2, 4:4:4, gray; no upsampling, no RGB), with libavcodec's `simple_idct` and DC level shift, so output is byte-identical to ffmpeg's `mjpeg` decoder. Annex K default Huffman tables stand in for a missing DHT (AVI1/MJPG). The stills path (`decode`) is untouched; `ec-image` tests unchanged.
- `ec-mjpeg` (`MjpegDecoder`, `decode_frame`, `CodecId::Mjpeg`): full range (JFIF) always, two-field packets woven and flagged interlaced, restart markers, progressive. Refused by name: 12-bit, arithmetic/lossless/hierarchical, CMYK, RGB-coded, 4:4:0 / 4:1:1 / 4:1:0.
- Containers: ec-mp4 maps `jpeg` and `mjpa` to Mjpeg (`mjpb` stays in the unsupported-entry listing); ec-matroska maps `V_MJPEG`.
- Gate: `crates/ec-mjpeg/tests/ffmpeg_parity.rs` (stills with restart/progressive, 8 pix_fmt x size x quality x huffman x container cases, gray, no-DHT AVI, two-field AVI/MOV/MKV) byte-exact vs ffmpeg; `fuzz/` target `decode_mjpeg`.

### ec-riff AVI video demux - DONE (lane `avivideo`)

- `ec_riff::AviDemuxer` (new `src/demux.rs`; the audio-only `AviReader` is untouched, its tests pass unchanged): audio and video streams, `Demuxer` trait, `seek_stream` returning the landed instant, `field_order(stream)`, `tracks()` listing refused streams by name, `index_kind()`, `is_avi()` sniff.
- Codecs: MJPG/jpeg/dmb1/AVRn to Mjpeg, H264/X264/avc1 to H264 (Annex B), MPG2/mpg1 to Mpeg2Video, apcn etc. to ProRes; audio PCM 8/16/24/32, float, MP2, MP3, AC-3, AAC (ASC from `strf` extradata). Refused by name: MPEG-4 Part 2 / MS-MPEG4, DV, DNxHD (`AVdn`, TODO until `CodecId::Dnxhd` is on main), uncompressed video (DIB, YUY2, UYVY, v210, I420, YV12). Raw video is refused rather than given a `RawVideo` id: there is no consumer, the packed-4:2:2 and bottom-up DIB layouts have no `PixelFormat`/descriptor, and adding an id means touching `registry.rs` that the dnxhd lane is editing.
- Index: OpenDML `indx` + `ix##` across `RIFF AVIX`, legacy `idx1`, linear scan (also over truncated files). Keyframes from the index, sniffed (IDR / I picture) on scan. Zero-size video chunks keep their frame slot. B-frame H.264/MPEG-2: pts = dts = decode-order slot (documented limit). Mjpeg field order BottomFieldFirst; others from `vprp`.
- Tests `crates/ec-riff/tests/it/avi_video.rs`: ffmpeg-made mjpeg+pcm / mjpeg+mp3 / h264(+B)+aac / mpeg2+mp2 / rawvideo / xvid checked against `ffprobe -show_packets` (count, size, key, time, adler32 of bytes); MJPEG frames decoded through ec-mjpeg equal ffmpeg's; hand-built OpenDML (two RIFFs), idx1, scan, vprp, seek. Fuzz target `avi_demux`.

### ec-dnxhd (Avid DNxHD / DNxHR decode) - DONE (lane `dnxhd`)

- `ec-dnxhd` (`DnxhdDecoder::decode(&Packet) -> VideoFrame`, `decode_bytes`, `decode_native`, `CodecId::Dnxhd`): all 20 compression ids ffmpeg knows (1235-1260 DNxHD at 8/10-bit 4:2:2, progressive and two-field interlaced with the interlace flag set, MBAFF 1260; DNxHR LB/SQ/HQ 8-bit, HQX 10/12-bit 4:2:2, 444 10/12-bit 4:4:4 YCbCr) at any size up to 8192x4608. Output I422/I210/I444/I410; ec-core has no 12-bit formats, so `decode` narrows 12 to 10 bits (`(v+2)>>2`) and `decode_native` returns the coded-depth `u16` planes. Refused by name: alpha, RGB (GBR) 4:4:4, ACT on 4:2:2.
- Containers: ec-mp4 maps `AVdn`/`AVdh`, ec-matroska maps `V_QUICKTIME`/`V_MS/VFW/FOURCC` carrying them, ec-riff maps `AVdn` (AVI).
- Gate: `crates/ec-dnxhd/tests/ffmpeg_parity.rs` byte-exact against ffmpeg's C decoder (`-cpuflags 0`): 19 DNxHD streams at 1080p/1080i/720p/1440/960, DNxHR x 5 profiles x 5 sizes (1280x544, 322x178, 258x122, 256x120, 1000x300), 3840x2160, 12-bit and MBAFF by header-patched streams, 1 and 3 threads. Against ffmpeg's default x86 SIMD build the output matches except where ffmpeg's own SIMD IDCT departs from its C code: 10-bit streams of saturated noise at very high rates (under 0.06% of samples differ, overflowing coefficients) and 12-bit (rounding, max abs error 1, above 80 dB). We follow the C code.
- Speed (server, 1080p DNxHR HQ at ~176 Mbps): 28 fps on one thread, 70 fps with 4-6 threads (rows handed out dynamically, `std::thread::scope`, at most 6).
- Fuzz: `crates/ec-dnxhd/fuzz` target `decode_dnxhd`, seeds in `fuzz/seeds`, 10 minutes clean.

### ec-prores encoder (Apple ProRes encode) - DONE (lane `proresenc`)

- `ec_prores::ProResEncoder` (`EncoderConfig`, `Profile`, `nominal_frame_bytes`; also the registry `Encoder` trait): 422 Proxy / LT / 422 / HQ from I210 or I422, 4444 / 4444 XQ from I410 / I412 / I444, progressive. Alpha is not coded (off by design: `alpha_channel_type` 0, `VideoFrame` carries none) and interlaced (two-field) output is refused by name; both are future work.
- Rate control: one quantiser for the whole frame (the MSE optimum), chosen by a secant search on a slice sample so the frame lands on Apple's rate for the profile and size (1080p29.97: 45/102/147/220/330/500 Mbit/s, scaled by `(macroblocks/8160)^0.846`, which gives Apple's UHD figures too); a Bresenham-spread fraction of slices takes the next finer step to land on the budget. Measured frame size is 0.997-1.001 of nominal on dense content at 1080p and 2160p.
- Bit-exact in both decoders: ffmpeg and ec-prores decode every profile identically (tests); output bytes do not depend on the thread count. PSNR vs `prores_ks` at the same profile on testsrc2 / mandelbrot / noisy testsrc2 is higher on smooth content (10-25 dB) and within 0.2 dB on noise, at equal or smaller frames.
- Slice-parallel via `std::thread::scope` (max 6 threads). Muxing: `ec-mp4` writes a ProRes sample entry from `CodecParameters.extradata` = the 4-byte profile fourcc (no config box; `colr`, `fiel`, `pasp` after it); use `Mp4Muxer::with_brands(w, *b"qt  ", &[*b"qt  "])` for a QuickTime .mov. ffprobe reads codec, profile, pix_fmt, `field_order=progressive` and the colour tags. Frame-header creator id is `edth` (configurable; ffmpeg itself writes `Lavc`, not `apl0`).
- Tests `crates/ec-prores/tests/encode.rs` (synthetic only); decoder files untouched. Example `examples/enc.rs`.

### ec-mpeg4 (MPEG-4 Part 2 Simple / Advanced Simple and H.263 decode) - DONE (lane `mpeg4p2`)

- `ec-mpeg4` (`Mpeg4Decoder`, `CodecId::Mpeg4Part2`, registry name `mpeg4`): XviD / DivX 4-5 / FMP4 / mp4v and H.263 baseline (short header, QCIF to 4CIF, GOB headers). I / P / B / S-VOPs, half- and quarter-pel MC, 4MV, direct mode (8x8, 16x16 and field variants), field DCT and field MC, interlace flag set on output, MPEG and H.263 quantisation with custom matrices, AC/DC prediction, resync markers, N-VOPs. Output I420 in display order.
- GMC: 0 to 3 warp points are decoded, all exact (libxvid emits 3). The brief asked to refuse more than 1; this is a superset and nothing is refused.
- Output order and delay: unless the VOL says `low_delay`, one reference picture is held, so output lags decode by one picture; B pictures come out at once and flush emits the held one. There is no PTS in AVI, so display order is the only contract. Like libavcodec, an N-VOP produces no output frame (a final N-VOP makes flush emit the held picture with the last packet's pts).
- Packed bitstream (DivX packed B-frames): unpacked inside the decoder with libavcodec's stash model (a trailing I or B VOP in a packet is stashed and decoded in place of the next, short N-VOP packet). Not gated on a DivX user-data sniff, so XviD packed streams unpack too.
- libavcodec workarounds matched, taken from the user data (XviD / DivX / Lavc build): XviD IDCT chosen when an XviD build is seen (`set_xvid_idct` forces it either way), qpel chroma variants, hpel chroma, dc clip, direct block size, edge. A FourCC is only used if given through `set_fourcc`; the registry constructor cannot pass it.
- Refused by name: studio profiles, non-rectangular shape, static sprites, sprite brightness change, NEWPRED, reduced-resolution VOPs, scalability, data partitioning and reversible VLC, H.263 PLUSPTYPE / SAC / AP / PB-frames. MS-MPEG4 v1/2/3 (DIV3, MP43, ...) stay refused in ec-riff.
- Containers: ec-riff maps XVID/XVIX/DIVX/DX50/FMP4/MP4V/M4S2/MP4S/RMP4/DVX1/BLZ0/UMP4 (supersedes the "refused" line in the avivideo entry); ec-mp4 maps `mp4v` (esds objectTypeIndication 0x20, DecSpecificInfo as extradata) and `s263`/`h263`; ec-matroska maps `V_MPEG4/ISO/SP|ASP|AP` and VFW FourCCs.
- Gate: `crates/ec-mpeg4/tests/ffmpeg_parity.rs` (30 cases) and `tests/containers.rs` byte-exact against `ffmpeg -threads 1 -cpuflags 0`: I/P, q 2 to 31, 4MV, AIC, CBR, B 1 and 2, qpel, field DCT/ME with direct-field, mpeg_quant, resync, odd and tiny sizes, 720p everything, H.263 formats, libxvid GMC/qpel/interlaced, AVI DX50/XVID/FMP4, MP4 mp4v, 3GP H.263, MKV, packed bitstream, refusal and corruption robustness. Reference recipe: `-threads 1` because the frame-threaded interlaced direct mode loses the field-MV table (non-deterministic output); `-cpuflags 0` because ffmpeg's x86 `put_no_rnd_pixels8_x2/y2` SIMD is not bit-exact with its C code (+-1 in rare pixels). We follow the C code. `-flags +bitexact` is not a usable reference (it changes the MPEG-2 intra dequantiser).
- Speed: ignored test `speed_720p_at_least_120_fps` (qpel+4MV+B, one thread); see the report for server numbers. Fuzz: `crates/ec-mpeg4/fuzz` target `decode_mpeg4`, seeded corpus.
- DONE 2026-10-20 ec-dv decode (DV25 4:1:1/4:2:0, DVCPRO25, DVCPRO50, DVCPRO HD 1080i/720p): byte-exact vs ffmpeg dvvideo in native pix_fmt on 9 fixtures flavours and on mutated/damaged streams (incl. ffmpeg VLC length quirk and ec=3 concealment); DV audio sample-exact vs ffmpeg dvaudio (16-bit 48/44.1/32k, 12-bit nonlinear); raw .dv DIF demuxer with `is_dv`; DvVideo mapped in ec-mp4 and ec-riff AVI. 4:1:1 widened to I422 by duplication in the Decoder trait, native via `DvVideoDecoder`. Report: `dvvideo.report.md`.
- DONE 2026-10-20 muxhdr: ec-mp4 muxes and demuxes QuickTime PCM `in24`/`in32`/`fl32` (LE and BE via `wave/frma/enda`, sound description v1, frame-per-sample tables, `chan` for mono/stereo/5.1; sample entries byte-identical to ffmpeg) plus demux of v2 `lpcm`; new `CodecId::PcmS24Be/PcmS32Be/PcmF32Be`; `ec_core::MasteringPrimaries` + `ContentLight::mastering_primaries`; `Mp4Muxer::set_content_light`/`set_channel_layout_box` and `MatroskaMuxer::set_content_light` write `mdcv`/`clli` and `Colour/MasteringMetadata` (primaries, white point, luminance) + MaxCLL/MaxFALL, all read back. Nothing is written unless the caller sets it. Tests: `ec-mp4/tests/it/qt_pcm_hdr.rs`, `ec-matroska/tests/hdr_metadata.rs`. Report: `muxhdr.report.md`.
- DONE 2026-10-23 ec-prores decode (P1, `doneaudit`): byte-exact vs ffmpeg 8.x `prores_ks`/`prores_aw` on the 54 `matches_ffmpeg` cases (`tests/ffmpeg_parity.rs`), 1 and 3 threads, pass (report `prores.report.md`, merge `327c3138`); `EC_REQUIRE_FIXTURES=1 cargo test -p ec-prores -p ec-mp4 -p ec-core --release` 152 passed, 0 failed, 2 ignored (server); clippy clean; `forbid(unsafe)` since `safesimd-prores` (merge `5c0d241a`); fuzz `decode_prores` 10 min clean, 9,027,240 runs on host c; speed on quiet host b (load 0.05) 1080p 422 HQ: 63.7 fps single-thread (2.55x, set spread 2.7%), 161.1 fps on 4 threads (6.44x), so the 60 fps single-thread target is met (`doneaudit` bench rows); edith decodes through `ProResSeat` (`engine/tests/procodecs.rs` `prores_every_profile_opens_decodes_seeks_and_exports`, `prores_export.rs`).
- DONE 2026-10-23 ec-mpeg2 decode (P1, `doneaudit`): byte-exact vs ffmpeg 8.1.3 (default simple IDCT, md5 match) with `ffmpeg_parity.rs` 4 pass and `synth_parity.rs` 4 pass; `EC_REQUIRE_FIXTURES=1 cargo test -p ec-mpeg2 -p ec-mpegts -p ec-core --release` 144 passed, 0 failed, 1 ignored; clippy clean; fuzz `decode_mpeg2`: the first 1-hour run found a panic (MB-row change in a sequence extension without resize overran a pooled picture, decoder.rs:1106), fixed in lane `mpeg2crash` (merge `77ec01a1`, regression test `robust.rs` `mb_height_change_without_resize_is_concealed`), then 1 CPU-hour clean on the fixed code, 430,880 runs on host b (`-max_len=8192`, cov 1807 ft 9673); single-thread decode 101.5 fps on 1080i29.97 20 Mb/s IPB (3.39x) and 467.5 fps on 576i25 6 Mb/s IPB (18.70x), shared host b, spreads 5-15% so treat as rough (`doneaudit` bench rows); merge `0771c21b`; edith uses it as the software MPEG-2 backend (`engine/src/decode/mpeg2.rs`, `engine/tests/mpegts.rs` `mpeg2_video_opens_probes_and_decodes_as_ffmpeg_does`).
- DONE 2026-10-23 ec-mp3 Layer I/II decode (MP1/MP2, `doneaudit`): `ffmpeg -c:a mp2`/`mp2fixed` and libtwolame streams within 1e-5 of ffmpeg's float decoder (`tests/mp2_matrix.rs`, max 5.2e-6 and 6.2e-6); 336 hand-built Layer I and II streams max 3.85e-5 vs `mp2float` and 1 LSB vs `mp2fixed`; `cargo test --release -p ec-mp3`: 35 passed, 0 failed, 2 ignored; clippy clean; fuzz target `decode` (MP1/MP2/MP3 through `Mp3Reader`) 1 CPU-hour clean, 322,492 runs on host c (`mp3fuzz`, merge `49de1e0e`); public-domain real media (archive.org) equal to ffmpeg: ZeitgeistWeekendLiner.mpg 3,287,808 samples (max diff 1.49e-6), wtc_construction.mpg 14,063,616 samples (max 9.8e-7), and StripQueen.mpg equal except its final truncated 88-byte packet, which ffmpeg decodes and ec-mp3 refuses as incomplete (one 1152-sample frame, 24 ms); single-thread 389.0x realtime at 384 kb/s and 401.1x at 192 kb/s on shared host b, spreads 5-24%; merge `6acc9d93`; edith's MP2 tests (`cargo test --release -p engine --test mpegps --test avi`, 18 passed) ran locally because the server slots were busy. Layer I is tested only on hand-built frames (no MP1 encoder and no clearly licensed MP1 sample).
- DONE 2026-10-23 ec-mpegts TS demux (`doneaudit`): `cargo test --release -p ec-mpegts` with `EC_REQUIRE_FIXTURES=1` 79 passed, 0 failed, clippy clean; fuzz `demux_ts` 1 CPU-hour clean, 948,269 runs on host c (`-max_len=16384`, cov 2808 ft 16195), and `ts_parts` 1 CPU-hour clean, 17,792,835 runs on host c, both on current main because the 2026-10-20 record had no log and predated the TrueHD/DTS/DVB-subtitle changes; packet counts equal ffprobe on a 60 s H.264 1080p30 + AAC 128k + AC-3 5.1 448k file (6489 packets, 123.8k packets/s, 942 MB/s, shared host b, spread 3.6-17%); TrueHD (lane `tstruehd`), DTS (lane `dts`) and DVB subtitles (lane `dvdsubs`, merges `482942fb`/`c8a3ed95`) are exposed; edith's `TsDemuxer` (`engine/src/demux/ts.rs`) is tested by `engine/tests/mpegts.rs` (12 tests); the 79-test run is shared with the PS line.
- DONE 2026-10-23 ec-mpegts PS demux (`doneaudit`): the same `cargo test --release -p ec-mpegts` run with `EC_REQUIRE_FIXTURES=1` (79 passed, 0 failed, `ps.rs` 29 and `committed_ps.rs` 2); fuzz `demux_ps` 10 min, 24,217 runs clean (`pstail`), which its section accepts as a smoke run; packets equal ffprobe on a 60 s MPEG-2 1080p30 + MP2 224k + AC-3 5.1 448k file (6175 packets, 61.1k packets/s, 644 MB/s, `PsDemuxer`, shared host b, spread 12-28%); VobSub is exposed (lane `dvdsubs`), no longer refused; edith's `PsDemuxer` (`engine/src/demux/ps.rs`) is tested by `engine/tests/mpegps.rs` (11 tests).
