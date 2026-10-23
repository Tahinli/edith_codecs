# Lane avivideo: ec-riff AVI audio+video demuxer

Branch `lane/avivideo` (worktree /home/user/.cache/wt/avivideo). Commits are `lane/avivideo: ...`, no trailers, not pushed.

## What changed
- New `crates/ec-riff/src/demux.rs`: `AviDemuxer`. The audio-only `AviReader` (`avi.rs`) is untouched; its tests (wav, rf64, avi_fuzz_regressions, unit) pass unchanged.
- `crates/ec-riff/tests/avi_video.rs` (16 tests), fuzz target `crates/ec-riff/fuzz/fuzz_targets/avi_demux.rs`, `DONE.md` entry. `Cargo.toml`: dev-dependency `ec-mjpeg`. `registry.rs` NOT touched.

## Decisions
- Raw/uncompressed video (DIB, YUY2, UYVY, v210, I420, YV12): refused by name, not given a `RawVideo` id. Reasons: no consumer; packed 4:2:2 and bottom-up DIB have no `PixelFormat`; a new `CodecId` means editing `registry.rs`, which the dnxhd lane is also editing. In a file that also has audio, the audio still opens and `tracks()` names the refused picture stream.
- DNxHD (`AVdn`): `CodecId::Dnxhd` does not exist on main today, so it is refused by name with a `TODO(dnxhd)` in `map_video`. After the dnxhd merge, map `AVDN` to `CodecId::Dnxhd` (one line, `demux.rs`).
- DV (dvsd/dv25/dvh1...) refused by name (no CodecId). MPEG-4 Part 2 (mp4v/XVID/DIVX/DX50/FMP4/DIV3...) refused by name.
- B-frames: AVI has no timestamps; packets are in decode order and pts = dts = decode-order slot. For H.264/MPEG-2 with B-frames that pts is not presentation time; the decoder's POC/temporal reference gives display order. Documented in the module docs and tested.
- Zero-size video chunks: no packet, but the frame slot is kept so later pts jump. Zero-size audio chunks (ffmpeg writes them for B-frame interleave) take no time.
- ffmpeg's avi muxer writes `idx1` (and an `indx` placeholder that stays JUNK) for files under 1 GB, so OpenDML is tested with hand-built files spanning `RIFF AVI ` + `RIFF AVIX`, `indx` super-index and `ix00/ix01` (base offsets, non-key bit 31, `dmlh`).
- Field order: `Mjpeg` streams always `Some(BottomFieldFirst)` (ffmpeg's AVI convention, harmless for progressive packets); other streams from `vprp` (one field: Progressive; two fields: the field with the smaller `VideoYValidStartLine` first, heuristic); `None` when the file states nothing.

## Tests
ffmpeg-made, compared with `ffprobe -show_packets -show_data_hash adler32` (per-stream count, size, keyframe flag, dts time, byte checksum): mjpeg+pcm_s16le, mjpeg+mp3, h264 (no B) + aac with seek, h264 with B-frames + aac, mpeg2video+mp2. MJPEG packets decoded through ec-mjpeg equal ffmpeg's yuvj420p output byte for byte. rawvideo yuyv422 and bgr24 and XVID refused by name. A file cut at 60% (no idx1) is read by linear scan and is a prefix of the full packet list. Hand-built: OpenDML over two RIFFs, idx1, bare scan (also over AVIX), zero-size frames, seek (SyncBefore/After/Exact across AVIX), vprp, garbage input.

## Gates (server)
See bottom (filled after the runs).

## Public API summary (edith integration)
```rust
// crate ec-riff
pub fn is_avi(head: &[u8]) -> bool;                        // RIFF....AVI  (12+ bytes)
pub struct AviDemuxer<R: Read + Seek> { .. }
impl<R: Read + Seek> AviDemuxer<R> {
    pub fn new(r: R) -> Result<Self>;                      // Err(Unsupported) names the first refused stream when none is usable
    pub fn tracks(&self) -> Vec<AviTrackInfo>;             // every strl incl. refused: number, media, tag, stream: Option<u32>, unsupported: Option<String>
    pub fn field_order(&self, stream: u32) -> Option<Interlace>;   // Mjpeg => BottomFieldFirst
    pub fn index_kind(&self) -> AviIndexKind;              // OpenDml | Idx1 | Scan
    pub fn chunk_count(&self, stream: u32) -> usize;
    pub fn seek_stream(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<Timestamp>; // landed instant
    pub fn rewind(&mut self);
}
impl<R: Read + Seek + Send> Demuxer for AviDemuxer<R> {    // streams(), next_packet() (file order, Eof at end), seek()
}
```
- Stream time base is `dwScale/dwRate`; video pts = frame slot, audio pts = samples (fixed `dwSampleSize`) or chunk count (compressed). `StreamInfo.duration` from the chunk table (OpenDML `dmlh` counts for video).
- Codec ids: Mjpeg, H264 (Annex B, in-band SPS/PPS; any BITMAPINFO tail bytes are extradata), Mpeg2Video, ProRes; audio PcmU8/S16Le/S24Le/S32Le/F32Le, Mp2, Mp3, Ac3, Aac (extradata = AudioSpecificConfig).
- For MJPEG: `MjpegDecoder::set_field_order(demux.field_order(i).unwrap())`.
- Keyframes: index flags; sniffed (IDR/SPS, I picture) when scanning; audio and Mjpeg/ProRes always key.
- Limits: 16M chunks, rec-list depth 16, header bodies 16 MiB; packet allocation is bounded by the file length.

## Gate results (server host c, final code)
- `cargo clippy --workspace --all-targets --release -- -D warnings`: clean.
- `EC_REQUIRE_FIXTURES=1 cargo test -p ec-riff -p ec-core -p ec-mjpeg --release`: all pass (avi_video 15 pass + 1 ignored seed dumper, ffmpeg present).
- Fuzz `avi_demux`: 601 s, 536,815 executions, no crash, no artifacts (seeds: ffmpeg mjpeg/h264/mpeg2/mp3 AVIs plus hand-built OpenDML/idx1/bare files via `dump_seeds`).
