# lane muxhdr report

Branch `lane/muxhdr` (worktree /home/user/.cache/wt/muxhdr), from main 77853fc7. Not pushed, no trailers.

## What changed
ec-core
- `CodecId::PcmS24Be`, `PcmS32Be`, `PcmF32Be` (names `pcm_s24be` ...), registry table and test count updated.
- `ec_core::MasteringPrimaries { green, blue, red, white }` (each `(x, y)` in 0.00002 steps, the mdcv/SEI units), consts `BT2020`, `P3_D65`.
- `ContentLight` gains `mastering_primaries: Option<MasteringPrimaries>` (default None). `ec_core::color::mdcv()` (mp4 `mdcv`, HEVC SEI reader) now fills it (all-zero = not stated). Edith does not name `ec_core::ContentLight` literals (it has its own copy in colorspace.rs and builds `VideoParameters { light: Default::default() }`), so no edith break. In-repo literals fixed (`..Default::default()` in ec-mp4 tests/mp4.rs, ec-h265-syntax sei.rs test).
- Units are unchanged and the same as ec-h265: nits as f32 for luminance (ec-h265-syntax `MasteringDisplay` 0.0001 cd/m2 = `nits * 10000`), MaxCLL/MaxFALL whole nits, primaries/white point 0.00002 steps.

ec-mp4
- Audio entries `in24` / `in32` / `fl32` for all of PcmS24Le/Be, PcmS32Le/Be, PcmF32Le/Be, exactly as ffmpeg writes them in .mov: sound description version 1 (samples/packet 1, bytes/packet, bytes/frame, 2), `wave` { `frma`, `enda` (1 = little-endian), 8-byte zero-type terminator }, then `chan` (mono tag 0x640001, stereo 0x650002, 5.1 UseChannelBitmap 0x3f; other layouts: no `chan`). Tables follow QuickTime/ffmpeg too: one sample per frame (stsz = bytes per frame, one stts run of 1), chunks counted in frames. Verified byte-identical sample entries vs ffmpeg for mono/stereo/5.1, s24le/be, s32le/be, f32le/be.
- Demux reads all of them back: `in24/in32/fl32` + `enda` give the LE/BE codec id, `bits_per_sample` 24/32, and QuickTime v2 `lpcm` (flags float/big-endian, bits 16/24/32) is read as well (read only: ffmpeg writes in24, so the muxer does too). Frame-per-sample PCM is merged into packets of up to 4096 frames (a minute is not 3 million packets). Only these six codecs are touched; sowt/twos/raw demux is unchanged.
- `Mp4Muxer::set_content_light(stream, ContentLight)`: writes `mdcv` (luminances; primaries from `mastering_primaries`, BT.2020/D65 if only luminances are given, as before) and `clli`, in the visual sample entry, after `colr`. Rounding fixed (`round()` instead of truncation, so 0.005 nit = 50, not 49). A track never told writes nothing new; `VideoParameters.light` set at `add_stream` still works as before.
- `Mp4Muxer::set_channel_layout_box(stream, bool)`: adds `chan` to entries that do not carry it by default (sowt, twos). Default off: existing 16-bit output is byte-identical.

ec-matroska
- `MatroskaMuxer::set_content_light(stream, ContentLight)` (before the first packet): `Colour/MaxCLL`, `MaxFALL`, `MasteringMetadata` with PrimaryR/G/B X/Y, WhitePointX/Y (floats, value/50000) then LuminanceMax/Min (rounded to 0.0001 so 0.005 is written as 0.005). Existing `v.light` path gets the same writer. Demux reads the eight chromaticities back into `mastering_primaries`. Nothing new is written when not set.

## Tests (synthetic, ffmpeg sources; skip without ffmpeg unless `EC_REQUIRE_FIXTURES`)
- `crates/ec-mp4/tests/qt_pcm_hdr.rs`: 9 PCM cases (s24le/be stereo, s24le mono, s24le/be 5.1, s32le/be, f32le/be): ffprobe codec/channels/layout/rate identical to the ffmpeg-written source, our sample entry byte-identical to ffmpeg's, ffmpeg decode md5 equal, our demux gives the same codec id and identical concatenated sample bytes. `sowt` unchanged without the setter, equal to ffmpeg's with it. HEVC Main 10 (libx265, bt2020/PQ) remuxed with `set_content_light`: ffprobe `-show_streams -show_frames` shows "Mastering display metadata" (red 34000/50000 ... white 15635/50000, min 50/10000, max 10000000/10000) and "Content light level metadata" (max_content 1000, max_average 400); no mdcv/clli bytes when not set; demux round trip equal; luminance-only gives BT.2020 primaries.
- `crates/ec-matroska/tests/hdr_metadata.rs`: same HEVC Main 10 into mkv: ffprobe side data (values as numbers, primaries 0.68/0.32 ... luminance 0.005/1000, MaxCLL 1000, MaxFALL 400), no 0x55D0 element when not set, exact demux round trip.
- All existing byte-exact/golden mux tests are unchanged and pass.

## Gates (host c, final commit)
- `cargo clippy --workspace --all-targets --release -- -D warnings`: clean.
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-mp4 -p ec-matroska -p ec-core --release`: all pass.
- `cargo check -p engine` (remote-run --with-edith --host c, EDITH_TREE unset, edith main tree unchanged): builds.

## Edith diff (NOT applied) for the proexport code
Note: edith's Matroska writer is its own `MkvMuxer` in crates/engine/src/mux.rs, not ec-matroska, so the mkv part writes the elements itself (ids below). The mp4 side goes through `ec_mp4::Mp4Muxer`.

1. 24-bit PCM for ProRes, `crates/engine/src/prores_out.rs` `write_sound`: use the BE codec (what a ProRes/Logic .mov master carries, ffmpeg `pcm_s24be`) or LE. Bytes are plain packed 24-bit.
```diff
-        let mut params = CodecParameters::new(CodecId::PcmS16Le);
+        let mut params = CodecParameters::new(CodecId::PcmS24Be); // or PcmS24Le
         params.media = MediaParameters::Audio(AudioParameters {
             sample_rate: rate,
             layout: ChannelLayout::from_count(channels),
             ..Default::default()
         });
@@
-            let bytes: Vec<u8> = chunk.iter().flat_map(|&s| (s.clamp(-32768, 32767) as i16).to_le_bytes()).collect();
+            // `samples` must then be 24-bit values in an i32 (mix_pcm16 -> a 24-bit mix);
+            // low 3 bytes of the big-endian i32.
+            let bytes: Vec<u8> = chunk.iter().flat_map(|&s| {
+                let b = s.clamp(-8_388_608, 8_388_607).to_be_bytes();
+                [b[1], b[2], b[3]]
+            }).collect();
```
`export.rs`: `mix_pcm16`/`mix_pcm` scale to 16 bits today; a 24-bit master needs `mix_pcm` to scale by 8_388_607 instead of 32767 (or shift `<< 8` for a bit-for-bit widened 16-bit mix). The muxer takes the packets the same way (`Packet` duration in frames). The `chan` box (mono/stereo/5.1) and `wave/enda` are written by ec-mp4 on its own, nothing to set. `ProResProfile` docs: "pcm_s24be".

2. mp4 `mdcv` / `clli` for `Format::HevcMp4` (`crates/engine/src/mux.rs` `Mp4Muxer`):
```diff
 pub struct Mp4Muxer {
     ...
     colour: ColorInfo,
+    light: ec_core::ContentLight,
 }
@@ in open(): where `colour: color_info(...)` is initialised
+            light: Default::default(),
@@ after set_colour
+    /// HDR static metadata for the `mdcv` / `clli` boxes, written at finish.
+    pub fn set_hdr(&mut self, hdr: &crate::export::HdrOutput) {
+        self.light = hdr_light(hdr);
+    }
@@ in finish(), right after .set_color(VIDEO_STREAM, self.colour)
+            self.writer
+                .set_content_light(VIDEO_STREAM, self.light)
+                .map_err(mp4_error)?;
+
+/// `HdrOutput` in ec-core's units (nits; chromaticities in 0.00002 steps).
+pub(crate) fn hdr_light(hdr: &crate::export::HdrOutput) -> ec_core::ContentLight {
+    ec_core::ContentLight {
+        max_cll: hdr.max_cll.map(f32::from),
+        max_fall: hdr.max_fall.map(f32::from),
+        mastering_max: hdr.mastering_display.map(|m| m.max_luminance as f32 / 10_000.0),
+        mastering_min: hdr.mastering_display.map(|m| m.min_luminance as f32 / 10_000.0),
+        mastering_primaries: hdr.mastering_display.map(|m| ec_core::MasteringPrimaries {
+            green: m.primaries[0],
+            blue: m.primaries[1],
+            red: m.primaries[2],
+            white: m.white_point,
+        }),
+    }
+}
```
`export.rs`, after `Mp4Muxer::create_hevc` (the block that does `mp4.set_colour(...)`): `mp4.set_hdr(&settings.hdr);` when `settings.hdr.transfer != HdrTransfer::Sdr` (a no-op struct otherwise, SDR stays byte-identical because `ContentLight::default()` writes nothing). The same helper is correct for the ec-mp4 SDR copy path.

3. Matroska `Colour/MasteringMetadata` + MaxCLL/MaxFALL, engine's own `MkvMuxer`: add `light: Option<ec_core::ContentLight>` to `HevcParams` (`colour: Option<Tags>` is already there; fill `light: (hdr.transfer != Sdr).then(|| hdr_light(&settings.hdr))`, `None` at the other literals and in `CopyParams`), thread it into `open(...)`, and in the `Colour` writer after `elem(&mut tags, PRIMARIES...)`:
```diff
         uint(&mut tags, PRIMARIES, u64::from(colour.primaries.unwrap_or(2)));
+        if let Some(l) = light {
+            if let Some(v) = l.max_cll { uint(&mut tags, 0x55BC, v.round() as u64); }   // MaxCLL
+            if let Some(v) = l.max_fall { uint(&mut tags, 0x55BD, v.round() as u64); }  // MaxFALL
+            let mut m = Vec::new();   // MasteringMetadata 0x55D0
+            let f = |o: &mut Vec<u8>, id: u32, raw: u16| elem(o, id, &(f64::from(raw) / 50_000.0).to_be_bytes());
+            if let Some(p) = l.mastering_primaries {
+                f(&mut m, 0x55D1, p.red.0);   f(&mut m, 0x55D2, p.red.1);
+                f(&mut m, 0x55D3, p.green.0); f(&mut m, 0x55D4, p.green.1);
+                f(&mut m, 0x55D5, p.blue.0);  f(&mut m, 0x55D6, p.blue.1);
+                f(&mut m, 0x55D7, p.white.0); f(&mut m, 0x55D8, p.white.1);
+            }
+            let dec = |v: f32| ((f64::from(v) * 1e4).round() / 1e4).to_be_bytes();
+            if let Some(v) = l.mastering_max { elem(&mut m, 0x55D9, &dec(v)); }   // LuminanceMax
+            if let Some(v) = l.mastering_min { elem(&mut m, 0x55DA, &dec(v)); }   // LuminanceMin
+            if !m.is_empty() { elem(&mut tags, 0x55D0, &m); }
+        }
         elem(&mut dims, COLOUR, &tags);
```
(`elem` there takes `(&mut Vec<u8>, u32, &[u8])`; floats are 8-byte big-endian f64, as ec-matroska writes them.) `Colour` children appear in the order MatrixCoefficients, Range, Transfer, Primaries, MaxCLL, MaxFALL, MasteringMetadata, like ffmpeg. This replaces the "corner-cut" doc on `CopyParams::colour`; the copy path can pass `crate::demux::ContentLight` converted the same way. Alternatively use ec-matroska's `MatroskaMuxer` and its `set_content_light(0, light)` before the first packet.

The SEIs in the HEVC bitstream stay as proexport writes them; the container boxes now state the same values (ffprobe shows them from both).
