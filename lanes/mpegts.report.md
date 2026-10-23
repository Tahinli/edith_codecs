# Lane mpegts: report

New crate `crates/ec-mpegts` (branch `lane/mpegts`, worktree `/home/user/.cache/wt/mpegts`, from main 8ab54472).
Pure-Rust MPEG-2 transport stream demuxer for AVCHD `.mts`/`.m2ts` (192-byte BDAV) and `.ts` (188/204).
Nothing outside `crates/ec-mpegts`, `Cargo.lock` and `DONE.md` is touched. edith is untouched.

## Status

| Gate | Result |
|---|---|
| `cargo clippy -p ec-mpegts --all-targets -- -D warnings` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean (server, lane `mpegts-clippy`) |
| `cargo test -p ec-mpegts --release` with `EC_REQUIRE_FIXTURES=1` | passes on the server (lane `mpegts`): 11 unit, 2 committed, 15 ffmpeg parity, 2 robust, 9 synthetic |
| Fuzz | targets `demux_ts`, `ts_parts` in `crates/ec-mpegts/fuzz`; smoke only (see below), not the 1 CPU-hour DONE gate |
| DONE.md | section "Containers / ec-mpegts demux" added |

Fuzz smoke on server c (cargo-fuzz + nightly installed there): `ts_parts` 422k runs in 100 s, no finding. `demux_ts`
first version did 1855 runs in 171 s (too much work per input: every input did up to 10 000 packets and ~60 seeks),
no finding; the target was lightened afterwards and re-run for 200 s: 9170 runs (45 exec/s), cov 2257, rss 556 MB, no crash, no timeout. The 1 CPU-hour
gate of DONE.md is still to be run.

## Tests (all synthetic sources; nothing from ~/Music, ~/Downloads, ~/Videos)

- `tests/ffmpeg_parity.rs`: fixtures are generated at test time (testsrc2, sine), cached in `$TMPDIR/ec-mpegts-fx`,
  skipped with a note when ffmpeg/ffprobe/an encoder is missing, a failure under `EC_REQUIRE_FIXTURES`.
  Per stream: packet count, size, pts, dts, keyframe flag against `ffprobe -show_packets`, and the concatenated bytes
  against `ffmpeg -c copy -f data` (LPCM against ffmpeg's s16be decode). Cases: H.264 + AAC `.ts`, H.264 + AC-3
  `.m2ts` (192), H.264 MBAFF interlaced, H.264 + AC-3, + E-AC-3, + MP2 (refused), + MP3, + LPCM `.m2ts`, HEVC + AAC,
  two programs (default and `with_program`), PTS wrap (`-output_ts_offset 95442`, monotonic across 2^33), audio only AAC
  44.1 kHz, AC-3/E-AC-3 at 32/44.1/48 kHz and several bit rates, corrupted and truncated files.
  Duration is checked against ffprobe's packet extents within 45 ms. Every case also runs `seek_exact`.
- Seek exactness: seek (SyncBefore and Exact, 7 targets from 0 to past the end), then read to the end; the result equals the
  sequential read filtered to packets whose start offset is at or after `last_packet_offset()` (landing), packet for packet
  (stream, pts, dts, keyframe, bytes). SyncBefore lands on a keyframe with pts <= target and no closer keyframe exists.
- `tests/committed.rs`: five tiny fixtures in `tests/data` (239 KB total, `data/gen.sh` regenerates them from testsrc2/sine)
  against ffmpeg's byte hashes in `expected.txt`; runs without ffmpeg.
- `tests/synthetic.rs`: a small TS muxer builds PCR on its own PID, two access units in one PES (AUD split), 192 and 204-byte
  packets, duplicate packets, a PMT change (two files concatenated), a bad PMT CRC, refusals by name, PGS.
- `tests/robust.rs`: seeded mutations (flip, cut, drop, insert, noise, repeat) of every tiny fixture, each opened, read
  through in both video framings and seeked in every mode. Debug build, overflow checks on. No panic.

## API summary for the edith integration lane

```rust
use ec_core::registry::{Demuxer, SeekMode};
use ec_mpegts::{TsDemuxer, VideoFraming, StreamKind, is_mpegts};

is_mpegts(head: &[u8]) -> bool                       // first few KiB; stride-confirmed sync at 188/192/204
let mut d = TsDemuxer::new(reader)?;                 // R: Read + Seek (+ Send for the Demuxer trait)
let mut d = TsDemuxer::with_program(reader, number)?; // pick a program by program_number
```

Opening probes up to 32 MiB (stops as soon as PAT/PMT are read, every stream has parameters and ~12 pictures were seen),
then scans the file tail (4 MiB, growing) for the last timestamps. A 245 MB 720p `.m2ts` opens in 2 ms and reads at
~2.4 GB/s from cache; seeks take 0.5 to 2.5 ms (`examples/bench.rs`).

**Streams** (`d.streams() -> &[StreamInfo]`, `Demuxer` trait): only what the family can decode.
Time base of every stream and packet is `1/90000`.

| Stream type | `CodecId` | extradata | packets |
|---|---|---|---|
| 0x1B H.264 | `H264` | `avcC` from in-band SPS/PPS | Annex-B access unit (AUD, SEI, SPS, PPS stay in band) |
| 0x24 HEVC | `H265` | `hvcC` from VPS/SPS/PPS | Annex-B access unit |
| 0x0F AAC ADTS | `Aac` | 2-byte AudioSpecificConfig | one ADTS frame with header (ec-aac accepts ADTS) |
| 0x81/0x87/0x84/0xA1, private 0x06 with AC-3/E-AC-3 descriptors | `Ac3`, `EAc3` | none | one syncframe (E-AC-3: independent frame plus its dependent substreams) |
| 0x03/0x04 layer III | `Mp3` | none | one MPEG audio frame |
| 0x80 HDMV LPCM | `PcmS16Be` (16 bit), `PcmS24Le` (20/24 bit, byte-swapped) | none | one PES worth of PCM, 4-byte header removed, odd-channel padding channel dropped |
| 0x90 PGS | `Pgs` | none | one PES = whole PGS segments, as in mkv |

`StreamInfo` carries width/height/format/frame_rate (estimated from decode times, e.g. 30000/1001) for video,
sample rate/layout for audio, `start_time` (first PTS, unwrapped), `duration` (from the tail scan), `language` (ISO 639
descriptor) and `default` (first of each type). Video `VideoParameters` has no colour info.

**Packets**: `d.next_packet() -> Result<Packet>`; `Err(Eof)` at the end. `pts` and `dts` are unwrapped monotonic `i64`;
`dts` is `Some` only when the PES carried one; audio frames carry `duration`; video `duration` is `None`.
`flags.keyframe`: video = IDR/IRAP slice, H.264 recovery-point SEI, or the PES `random_access_indicator`; audio/PGS always.
`flags.corrupt`: the PES/frame was damaged (continuity gap, resync, TEI, truncation). Output order is completion order
across streams (a video access unit is emitted when the next one starts).

**Video framing**: `d.set_video_framing(VideoFraming::LengthPrefixed)` converts H.264/HEVC packets to 4-byte
length-prefixed NAL units; `extradata` is the matching `avcC`/`hvcC` either way. Standalone helpers:
`build_avcc(&[sps], &[pps])`, `build_hvcc(&[vps], &[sps], &[pps])` (NAL units without start codes),
`annexb_to_length_prefixed(&[u8], n)`, `nal_units(&[u8])`, `audio_specific_config(aot, sr_idx, ch_cfg)`,
`parse_lpcm_header(&[u8])`.
edith's `engine/src/demux.rs` (mp4/mkv paths) hands H.264/HEVC to the decoder as Annex-B and re-injects parameter sets
itself (`hvcC`/`avcC` to Annex-B), so the default Annex-B packets plus the config records fit its decode path directly.

**Seeking**: `d.seek(stream, Timestamp, SeekMode)`. The picture stream decides where to land (the first video stream,
else `stream`): bisection on byte offset using video decode times, then a forward scan to the last keyframe with
pts <= target (`SyncBefore`), the first keyframe with pts >= target (`SyncAfter`, `Err(Eof)` past the last), or the last
packet with dts <= target (`Exact`, no sync search). All streams resume at that file offset: audio that sits earlier in the
file than the video keyframe is not delivered (no per-stream pre-roll). `d.last_packet_offset()` is the landing offset
after a seek and the start offset of the last returned packet otherwise.

**Other queries**: `programs() -> &[ProgramInfo]` (number, pmt_pid, pcr_pid, PMT streams), `program_number()`,
`tracks() -> Vec<TrackInfo>` (every elementary stream of the selected program including refused ones: `pid`,
`stream_type`, `kind: StreamKind`, `stream: Option<u32>`, `refusal: Option<String>`, `language`), `stream_pid(i)`,
`packet_size()`, `is_bdav()`, `duration_secs()`, `time_base()`, `pcr_pid()`, `first_pcr()`, `stats()`
(`resyncs, cc_errors, duplicates, crc_errors, tei, discontinuities, pmt_changes, bad_pes`, counted since open/last seek).

**Refusals** (in `tracks()[i].refusal`, with the codec named): MPEG-1/2 video (`StreamKind::Mpeg1Video/Mpeg2Video`, name
`mpeg2video`), AAC LATM (`aac_latm`), MPEG audio layer I/II ("only layer III has a decoder (ec-mp3)"), DTS/DTS-HD,
TrueHD, VC-1, IGS, TextST, DVB subtitles/teletext, other stream types. `CodecId` has no MPEG-2 video or MP2 variant, and
`ec-core` was left alone, so those streams are not in `streams()`; the integration lane should show `refusal` to the user.
`ec-probe` is audio-focused and was not touched; `is_mpegts` is the sniff to add there if wanted.

**PMT changes mid-stream**: streams that stay keep their index; a PID that changes type or a new PID gets a new stream
index appended (`streams()` grows during `next_packet`); removed ones stop. `stats().pmt_changes` counts them.

## Behaviours worth knowing

- Audio timestamps: the first frame of a PES takes the PES PTS, later frames follow at one frame duration rounded down
  (matches ffmpeg exactly for AC-3/E-AC-3 and for AAC at 48 kHz; AAC at 44.1 kHz differs from ffmpeg by up to a few
  ticks (<0.1 ms) inside a PES, the parity test allows 8).
- Interlaced (PAFF/MBAFF) H.264 is split on PES boundaries only, so a field pair stays one packet like ffmpeg's parser
  does; progressive H.264 and HEVC are also split on access unit delimiters inside a PES (only the first part has a pts).
- Truncation is only detectable when the file ends mid-packet or inside a length-bounded PES; a file cut at a packet
  boundary inside an unbounded video PES yields a short last access unit unflagged.
- Seeds and durations assume a file spans less than ~13 hours between first PTS and any point (33-bit unwrap is
  nearest-candidate); sequential reads have no such limit.
- Bounded memory: PES/access unit 32 MiB per stream, 64 programs, 128 tracks, 64 ES per PMT, sections 4 KiB, audio carry
  16 KiB, 256 KiB read window.
- Not done: MPEG-2 PS (`.mpg/.vob`), LATM, SCTE-35, PCR clock recovery (PCR is parsed and `first_pcr`/`pcr_pid` exposed;
  seeking and duration use PTS). Bisection uses PTS, not PCR.
- BD LPCM channel order is the disc's, not reordered to the family's layouts.

## Commits on lane/mpegts

All start with `lane/mpegts: `, no trailers. See `git log main..lane/mpegts`.
