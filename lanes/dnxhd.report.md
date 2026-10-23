# Lane dnxhd report

New crate `crates/ec-dnxhd` (Avid DNxHD / DNxHR, SMPTE VC-3), branch `lane/dnxhd`, rebased onto main (with ec-mjpeg, ec-prores, ec-riff AviDemuxer).

## Coverage
- All 20 CIDs ffmpeg knows: 1235, 1237, 1238, 1241-1244, 1250-1253, 1256, 1258-1260 (DNxHD, 8/10-bit 4:2:2, progressive and two-field interlaced, MBAFF 1260) and 1270-1274 (DNxHR 444, HQX, HQ, SQ, LB), any size up to 8192x4608.
- Output I422 / I210 / I444 / I410. ec-core has no 12-bit formats: `decode` narrows 12 to 10 bits with `(v+2)>>2` clamped; `decode_native` gives the coded-depth u16 planes.
- Refused by name (`Error::Unsupported`): alpha, RGB (GBR) 4:4:4 (ACT flag clear; ffmpeg's yuv444 encodes set it and are accepted), ACT on 4:2:2.
- Containers: ec-mp4 `AVdn`/`AVdh` (also tolerates the QuickTime 4-byte stsd terminator ffmpeg writes), ec-matroska `V_QUICKTIME` / `V_MS/VFW/FOURCC` with AVdn/AVdh private data, ec-riff `AVdn`/`AVdh`.
- Registry: `CodecId::Dnxhd` ("dnxhd"), count assert now 32.

## Exactness
Byte-exact vs ffmpeg's C decoder (`-cpuflags 0`) on every test stream (19 DNxHD, DNxHR x5 profiles x5 sizes, 4K, 12-bit and MBAFF via header-patched streams, 1 and 3 threads). Against ffmpeg's default x86 SIMD IDCT: identical except (a) 10-bit saturated-noise streams at very high rates, <0.06% of samples (ffmpeg's SIMD differs from its own C on overflowing coefficients) and (b) 12-bit: max abs error 1, >80 dB. We follow the C code. The test tolerates exactly those cases.
Cannot be produced by ffmpeg's encoder (tested by patching): 12-bit, CID 1260 MBAFF. CID 1256 and the 1440-from-1920 width override are table/logic ported but untested.

## Performance (server host c, 1080p DNxHR HQ ~176 Mbps noisy testsrc2)
28 fps 1 thread, 70 fps with 4 to 6 threads (dynamic row queue, std::thread::scope, max 6).

## Public API (for edith)
- `ec_dnxhd::DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd))`; implements `ec_core::registry::Decoder` (send_packet / receive_frame / flush / reset; one packet = one frame, no delay).
- `decode(&mut self, &Packet) -> Result<VideoFrame>` (pts copied), `decode_bytes(&[u8])`, `decode_native(&[u8]) -> Picture` (u16 planes + `FrameInfo`: cid, bit_depth, chroma, size, interlaced, top_field_first, mbaff, sample_aspect_ratio, matrix), `set_threads(1..=6)`.
- VideoFrame.interlace = TopFieldFirst/BottomFieldFirst for two-field frames; planes are woven. color.matrix from the header.
- Codec parameters (width/height/format/SAR) are refreshed after each decode.

## Tests / gates
- `crates/ec-dnxhd/tests/ffmpeg_parity.rs`, `refusals.rs` (refusals by name, 1600 damaged-frame rounds, registry path), ec-mp4 unit test, AVI/MKV/MOV demux equality.
- Fuzz: `crates/ec-dnxhd/fuzz`, target `decode_dnxhd`, seeds in `fuzz/seeds` (148 KB), 10-minute run on the server: 126249 runs in 601 s, no crashes.
- Clippy `--workspace --all-targets --release -D warnings` clean on server.
