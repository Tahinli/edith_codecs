# Lane h264hi10edith

## Changes
- demux.rs: `avc_bit_depth` reads the first SPS (mp4 avcC and MKV CodecPrivate); `bit_depth()` is 10 for High 10 (9-bit is left-aligned to 10). Above 8 bits only 4:2:0 progressive same-depth <=10 opens; refused by name with an ffmpeg hint: "H.264 High 4:2:2 above 8 bits is not supported ...", 4:4:4, 4:0:0, mixed depth, >10-bit, interlaced. 8-bit H.264 behaviour is untouched (not refused at demux). `VideoFacts.bit_depth` is Some(10) for High 10 (still None for 8-bit H.264).
- decode.rs: `Deep` (I010 -> u16 Wide, or >>2 8-bit I420). Playback `send_picture` narrows I010. `open_hw`/`hw_decodes` route H.264 >8 bit to software (probe says Software).
- export.rs (minimal, below): export's ten_bit was already codec-agnostic (`Demuxer::bit_depth() >= 10`), so only the H.264 software seat needed to emit Wide16 / narrowed planes.
- tests/h264_high10.rs (5 tests; ffmpeg+libx264 synthetic testsrc2/mandelbrot in mp4 and mkv, skips without them).

## Tests
- probe: bit depth 10, profile High 10, backend Software, 4 clips.
- ec-h264 I010 == ffmpeg yuv420p10le byte-exact on all 4 clips (30 frames, B-frames, keyint 10).
- software seeks (1,5,9,10,13,17,25,29) == sequential decode, 4 clips.
- 10-bit export (Hevc mkv / HevcMp4, from mp4 and mkv sources): pix_fmt yuv420p10le, PSNR 38.86 dB vs 8-bit export 38.78 dB (>30 and > 8-bit asserted); default depth follows source.
- High 4:2:2 10-bit refused by name in mp4 and mkv.

## Gates (host c)
- clippy -p engine -p edith_mcp -p edith --release --all-targets -D warnings: clean.
- full cargo test -p engine -p edith_mcp --release --no-fail-fast: all pass except the 2 known VA-API ones (h264_/hevc_mid_gop_seeks_match_a_sequential_decode).

## export.rs diff
```diff
diff --git a/crates/engine/src/export.rs b/crates/engine/src/export.rs
index d5f235f..ec581ce 100644
--- a/crates/engine/src/export.rs
+++ b/crates/engine/src/export.rs
@@ -8631,7 +8631,7 @@ impl Pictures {
     fn software(path: &Path, start_frame: u32, wide: bool) -> crate::Result<Self> {
         let (meta, demuxer) = Demuxer::open(path)?;
         match meta.codec {
-            Codec::H264 => Ok(Self::Sw(Box::new(SwDecoder::new(demuxer, start_frame)))),
+            Codec::H264 => Ok(Self::Sw(Box::new(SwDecoder::new(demuxer, start_frame, wide)))),
             Codec::Av1 => Ok(Self::Av1(Box::new(Av1Stream::spawn(demuxer, start_frame, wide)?))),
             _ => Ok(Self::Sw(Box::new(SwDecoder::open(path, start_frame, wide)?))),
         }
@@ -8707,6 +8707,9 @@ struct H264Seat {
     /// decoder's reorder still held, and only those pictures are left.
     flushed: bool,
     frame: Option<VideoFrame>,
+    /// Whether the export is 10-bit, and the High 10 picture taken for it.
+    wide: bool,
+    deep: crate::decode::Deep,
 }
 
 impl SwDecoder {
@@ -8722,7 +8725,7 @@ impl SwDecoder {
                 wide: (wide && meta.codec == Codec::Hevc).then(Wide::default),
             },
             None if meta.codec == Codec::H264 => {
-                SwSeat::H264(H264Seat::at(demuxer.seek_to_sync_at_or_before(start_frame)))
+                SwSeat::H264(H264Seat::at(demuxer.seek_to_sync_at_or_before(start_frame), wide))
             }
             // AV1 has no software seat in export: its decoder is chunked, so a
             // picture cannot be pulled without holding a whole run of them.
@@ -8766,6 +8769,10 @@ impl SwDecoder {
     fn picture16(&self) -> Option<Wide16View<'_>> {
         match &self.seat {
             SwSeat::Pulled { wide: Some(w), .. } => Some((&w.y, &w.u, &w.v, w.width, w.height)),
+            SwSeat::H264(s) if s.deep.in_wide => {
+                let w = &s.deep.wide;
+                Some((&w.y, &w.u, &w.v, w.width, w.height))
+            }
             _ => None,
         }
     }
@@ -8773,6 +8780,10 @@ impl SwDecoder {
     /// The picture `advance` stopped on, as planes and size.
     fn picture(&self) -> (&[u8], &[u8], &[u8], u32, u32) {
         match &self.seat {
+            SwSeat::H264(seat) if seat.deep.on => {
+                let n = &seat.deep.narrow;
+                (&n.y, &n.u, &n.v, n.width, n.height)
+            }
             SwSeat::H264(seat) => {
                 let frame = seat.frame.as_ref().expect("advance stored a picture");
                 let [y, u, v] = [&frame.planes[0], &frame.planes[1], &frame.planes[2]];
@@ -9026,9 +9037,9 @@ impl SwDecoder {
     /// The H.264 seat over an opened demuxer. Decoding restarts at a sync
     /// sample, so pictures between it and the in point are decoded (the target
     /// references them) and then dropped.
-    fn new(mut demuxer: Demuxer, start_frame: u32) -> Self {
+    fn new(mut demuxer: Demuxer, start_frame: u32, wide: bool) -> Self {
         let index = demuxer.seek_to_sync_at_or_before(start_frame);
-        let seat = SwSeat::H264(H264Seat::at(index));
+        let seat = SwSeat::H264(H264Seat::at(index, wide));
         Self {
             demuxer,
             start: start_frame,
@@ -9070,7 +9081,7 @@ impl SwDecoder {
 
 impl H264Seat {
     /// A fresh `ec-h264` seat whose first picture is display index `index`.
-    fn at(index: i64) -> Box<Self> {
+    fn at(index: i64, wide: bool) -> Box<Self> {
         Box::new(Self {
             decoder: H264Decoder::new(CodecParameters::new(CodecId::H264))
                 .expect("ec-h264 takes its own codec id"),
@@ -9078,6 +9089,8 @@ impl H264Seat {
             origin: index,
             flushed: false,
             frame: None,
+            wide,
+            deep: Default::default(),
         })
     }
 
@@ -9112,6 +9125,7 @@ impl H264Seat {
                         continue;
                     }
                     self.index += 1;
+                    self.deep.load(&pic, self.wide);
                     self.frame = Some(pic);
                     return Ok(true);
                 }
```
