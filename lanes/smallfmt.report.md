# lane/smallfmt report

## Done
- MJPEG-B (`mjpb` in mov): `ec-mp4` maps the entry to `CodecId::Mjpeg`; `ec-mjpeg` detects a packet starting `00000000 'mjpg'` and rewraps each field as a baseline JPEG (header = reserved, `mjpg`, field size, padded size, then offsets for second field, DQT, DHT, SOF, SOS, SOD; blocks keep their length but no marker; scan data is NOT byte-stuffed, so FF is re-stuffed to FF 00). Two-field packets go through the existing weave (top field first). Verified against ffmpeg's `mjpegb` decoder (layout cross-checked with libavcodec/mjpegbdec.c), 0 diff: 4 progressive cases (420/422/444, odd sizes) and 2 two-field cases. Test inputs: baseline JPEGs from ffmpeg's mjpeg encoder rewrapped into MJPEG-B and written into a hand-built .mov (test helper in `crates/ec-mjpeg/tests/ffmpeg_parity.rs`).
- Raw video in AVI (`ec-riff`, `CodecId::RawVideo`, no new CodecId): 8 bpp palettised DIB (palette read from the strf after the header, expanded to `Bgr8`), 15/16 bpp BI_RGB RGB555 (expanded to `Bgr8` by bit replication), YVYU (`I422`), NV12 (`Nv12`), Y800/Y8/GREY (`Gray8`, tight rows), V210 (`I210`, 10-bit LE in 16-bit; rows padded to 48 pixels). All 0 diff vs `ffmpeg -pix_fmt bgr24/yuv422p/nv12/gray/yuv422p10le`, including odd sizes (97x65) and width-96 v210.
- Raw video in mov (`ec-mp4` -> `RawVideo`): `raw ` 24 (`Rgb8`), 32 (ARGB -> `Bgra8`), 16 (RGB555 BE -> `Bgr8`), `2vuy`, `yuvs`, `yuv2` (chroma offset by 128 undone) -> `I422`, `v210` -> `I210`, `v410` -> `I410`, `b16g` -> `Gray16` (LE). 0 diff vs ffmpeg decode of the same .mov (13 cases incl. odd sizes). `raw `/`yuvs` are only video when the track handler is `vide` (they are also 8-bit/16-bit PCM audio entries).
- Not done: `raw ` 8-bit palette / gray depths 33-40 in mov (QuickTime default palette), BI_BITFIELDS 565, 1/4 bpp DIB, 15/16-bit output as native RGB555 (no PixelFormat for it; expanded instead).

## MS-MPEG4 v1
Left refused (MPG4/DIV1). ffmpeg has a v1 decoder but there is no encoder and no public v1 sample available here to build a verifiable stream.

## What edith's engine needs
- MJPEG-B: nothing new; the mov demuxer now yields `Mjpeg` for `mjpb`, and `MjpegDecoder` handles it (use the demuxer's `fiel`-derived field order as for `mjpa`).
- Raw mov: the engine must open `CodecId::RawVideo` with `ec_riff::RawVideoDecoder` for mov tracks too (until now only AVI produced it). The mov demuxer puts a synthesized 40-byte header in `params.extradata` (marker `ec_riff::MOV_MARKER` "mov " in the biSize slot, width, height, depth, entry FourCC); `RawVideoDecoder::new(params)` reads it. Output pixel formats now also include `Rgb8`, `Gray8`, `Gray16`, `Nv12`, `I210`, `I410` besides the previous `Bgr8`/`Bgra8`/`I420`/`I422`; the engine's presentation path must accept those (10-bit 4:2:2/4:4:4 especially).
- `StreamInfo` for AVI 8 bpp streams carries the palette in extradata (up to 40+1024 bytes).

## Gate
Both pass on the server: `cargo clippy --workspace --all-targets -- -D warnings` clean; `EC_REQUIRE_FIXTURES=1 cargo test` of the six crates: 36 suites, all ok. The old AVI refusal test now uses 1 bpp (monow) as the still-refused layout.
