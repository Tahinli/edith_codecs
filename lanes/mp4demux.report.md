# lane mp4demux: ec-mp4 demux parity with edith's `mp4::` reads

Base: `main` at `d271a92b`. Branch `lane/mp4demux`. Worktree `~/.cache/wt/mp4demux`.
Scope: DONE.md wave 2, the `mp4` crate to ec-mp4 swap, demux side only. `mux.rs` is untouched
(lane `mp4mux` owns it); `boxes.rs` is untouched.

## What edith reads through `mp4::` and where ec-mp4 stands

Surveyed by grepping `mp4::` and the `Mp4Track`/`Mp4Reader` accessors in `crates/engine` (edith
`10325ab`), and reading the vendored `vendor/mp4` patch (commit `514a5b4`, the QuickTime
`mp4a` v1/v2 sound-description fix).

| edith read | ec-mp4 before | ec-mp4 now |
|---|---|---|
| `mp4a` v1/v2 sound description, `esds` inside `wave` | already read (tested on `test_qt.mov`) | unchanged; verified by parity on `qt.mov` and `test_qt.mov` |
| `tkhd` matrix to quarter turn (`Rotation`) | exact integer test (`ec_core::Rotation::from_matrix`) | edith's tolerance: normalised by the matrix scale, 1e-3 column test, 0.5 degree angle test; mirror, shear, 45-degree are `Other` (`demux::display_rotation`) |
| `elst` entries | only the first non-empty `media_time` | every entry exposed as `TrackInfo::edits: Vec<Edit>` (segment duration, `media_time` (-1 = empty), 16.16 rate) |
| edith's edit-list trim (`trim_ticks`: first real `media_time` less the first `ctts` delay) | not exposed | `Mp4Demuxer::presentation_trim(stream)` |
| edith's frame-0 sample (`first_frame_sample`, the `packet_at` walk on `stts`) | not exposed | `Mp4Demuxer::first_frame_sample(stream)` (1-based) |
| edith's sync samples in display order (`sync_display_order`, open-GOP) | not exposed | `Mp4Demuxer::sync_in_display_order(stream)` |
| `stss` absent means all sync; `ctts` v0 and v1 with negative offsets; `stts`/`stsc`/`stco`/`co64` | already | unchanged; verified by parity (`negcts.mp4`, `bframes.mp4`, `h264-open-gop.mp4`) |
| video coded size: `avc1` entry for H.264, `tkhd` for every other codec (`Mp4Track::width`) | sample entry for all codecs | `tkhd` for non-H.264 when it is non-zero (`demux.rs` read_trak) |
| AAC rate and channels: from the AudioSpecificConfig (edith `channel_config`, `aac_output_rate`) | sample entry (`channelcount` fixed at 2 by spec) | from the ASC (`asc::aac_output`), extension rate for explicit SBR and PS |
| AC-3 channels (edith reads the bitstream; the `dac3` box agrees) | sample entry | from `dac3` acmod and lfeon (`asc::ac3_channels`) |
| `esds` average bitrate and stsz byte sum (`probe_bitrate`) | `Sample::size` | same numbers from `Sample::size`; no separate API needed |
| `colr`, `pasp`, `clli`/`mdcv`, `hvcC`, `avcC`, `av1C`, `dac3`, `dOps` | read by edith by hand (not via `mp4::`) | out of this lane's scope; `extradata` carries the raw config box bodies as before |

Not used by edith through `mp4::` and not added: `Mp4Track::duration` (ec exposes
`StreamInfo::duration` in ticks, edith converts), `frame_rate` (already exact in ec).

## Parity test

`crates/ec-mp4/tests/edith_parity.rs`, run against ffprobe 8.1.3 (`ffprobe -show_entries
stream=... -of default=nw=1` for streams and rotation, `-show_entries packet=stream_index,pts,dts,size,pos,flags`
for packets, `-show_streams -of json` for the rotation side data).

Per track, compared exactly: codec, coded size, timebase, sample rate, channels, rotation, and
for every sample: byte offset (`pos`), size, presentation time (`cts` vs `pts`), sync flag (`K`),
and the decode time up to one constant per track (ffmpeg shifts its `dts` by the largest
composition delay, so only the differences are the same claim; the constant is printed as
`dts-shift`).

Corpus: 126 files, 183 tracks.
- `fixtures/` video and audio (`fixtures/` is the symlink to the primary checkout).
- edith `assets/*.mp4|mov|m4a` (`EC_EDITH_ASSETS`, default `/home/user/Documents/Code/Rust/edith/assets`).
- generated: `scripts/gen-mp4-parity-corpus.sh ~/.cache/mp4demux-corpus` (`EC_MP4_PARITY_CORPUS`):
  B-frames (x264 `-bf 2`), negative ctts (`-movflags negative_cts_offsets`), rotation 90 and 270
  (stream copies of the committed `tests/data/rot*.mp4`), `-f mov` QuickTime with an
  `mp4a` version-1 sound description (`qt.mov`, confirmed v1 with `wave` by byte inspection),
  an audio track delayed 0.5 s (empty edit), AAC alone (priming elst), MP3 in mp4.
- real library: `fixtures/real-library-manifest.tsv` lists 38 mp4-family films; the 31 present
  locally are included. `edith/real_library.toml` lists only mkv/Matroska films, so it contributes
  no mp4 files and is not used.

Result: **126 of 126 files match ffprobe on every compared field.** The table is below.

## Where ffprobe and edith differ on purpose (named, bounded in the test)

1. **Edit lists.** ffprobe applies every edit, empty ones included, as a shift of the whole track
   (`Sample::dts`/`cts`, which are what the parity test compares). edith ignores empty edits and
   trims the picture by `media_time` less the first `ctts` delay (`presentation_trim`,
   `first_frame_sample`). Example: `emptyedit.mp4` (audio delayed 0.5 s): edits `(500,-1),(1500,1024)`.
   The two answers are both exposed; the test compares ffprobe's.
2. **ffmpeg's dts shift.** ffprobe's `dts` is decode time minus the largest composition delay, so
   it is not an absolute number to compare. The test compares `dts` up to one constant per track
   (`dts-shift`, 512 for `negcts.mp4`, 0 elsewhere).
3. **VP8/VP9 sync without `stss`.** The box makes every sample a sync sample; ffprobe derives the
   key flag from the bitstream. This is allowed only for VP8/VP9 with every sample sync in the
   table and fewer ffprobe keys. Affects `test_vp8.mp4` (2 keys in ffprobe, 58 non-key frames
   that the box marks sync). edith follows the box.
4. **Implicit HE-AAC (SBR without an extension frequency in the config).** ffprobe and edith
   both double the rate (`reelump4`, 24000 to 48000). ec-mp4 reports the core rate
   from the ASC because it does not decode; edith resolves it itself from the first access
   units (`mp4_aac_prime`). The test allows exactly a 2x rate difference on AAC.
5. **Trailing subtitle end marker.** A zero-duration sample at the movie's end is in the table
   but ffprobe does not hand it out (`ReelAA .mp4`, 99 samples, 98 packets). The
   test drops that one sample from the subtitle comparison only.
6. **Cover art.** `covr` pictures are listed by ffprobe as `attached_pic` video; neither ec-mp4
   nor edith lists them as tracks. Dropped on the ffprobe side.

## Fixes found by the parity run (all in ec-mp4)

- AAC channel count and rate taken from the AudioSpecificConfig (5.1 and 7.1 were read as 2
  channels from the fixed entry field; `ReelE`, `reelq`).
- AC-3 channel count from `dac3` (`ReelT`: 5.1 AC-3 read as 2 channels).
- Non-H.264 coded size from `tkhd` (edith's `Mp4Track::width` rule; a VP8 entry eight bytes
  short reads 72 wide from the entry, 1280 from `tkhd`, which is also what ffprobe reports).

Found and left as-is: `ffprobe -show_entries side_data=rotation` takes minutes on the 2 GB
film (it decodes for side data), so the test reads rotation from the JSON stream dump instead.

## Verify (all run from `~/.cache/wt/mp4demux`)

| command | where | result |
|---|---|---|
| `scripts/link-fixtures.sh` | local | fixtures linked |
| `scripts/remote-run.sh mp4demux -- cargo clippy -p ec-mp4 --all-targets -- -D warnings` | server host c/a/b | exit 0 (after one collapsible-if fix in the test) |
| `scripts/remote-run.sh mp4demux -- cargo test -p ec-mp4 --release` | server | lib 25 passed; edith_parity 2 passed (assets and corpus absent on the server, so the corpus runs locally); mp4 16 passed, 1 ignored (pre-existing); rotation 3 passed; exit 0 |
| `EC_MP4_PARITY_CORPUS=~/.cache/mp4demux-corpus cargo test -p ec-mp4 --release --test edith_parity -- --nocapture` | local, `CARGO_TARGET_DIR=~/.cache/tgt-mp4demux`, `TMPDIR=~/.cache/tmp-mp4demux` | 2 passed; 126 files, 183 tracks; 0 mismatches |
| `cargo test -p ec-mp4 --release --test mp4 -- --ignored the_real_library` | local | 1 passed (the real-library films demux to EOF, 7 s) |

The ignored test in `tests/mp4.rs` was pre-existing; it was not changed.

## Per-file parity table

Columns: file, then per track `t<stream> <codec> n=<samples> trim=<edith presentation_trim> first=<edith first_frame_sample> dts-shift=<ffmpeg dts constant> edits=<(segment duration, media_time)> display-syncs=<edith sync_in_display_order count>`.
Every row matched ffprobe on offset, size, pts, sync and dts differences.

| file | tracks |
|---|---|
| reel-905a10 | t0 h264 n=48 trim=0 first=1 dts-shift=0 edits=[(2000, 1024)] display-syncs=2<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-70f048 | t0 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0<br>t1 h264 n=36 trim=0 first=1 dts-shift=0 edits=[(500, -1), (1500, 1024)] display-syncs=1 |
| reel-82118f | t0 mp3 n=78 trim=1105 first=1 dts-shift=0 edits=[(2000, 1105)] display-syncs=0 |
| reel-fdca11 | t0 h264 n=48 trim=0 first=1 dts-shift=512 edits=[(2000, 0)] display-syncs=1 |
| reel-fe9d35 | t0 h264 n=48 trim=0 first=1 dts-shift=0 edits=[(2000, 1024)] display-syncs=1<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-a3b645 | t0 h264 n=24 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-9965ed | t0 h264 n=24 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-f010cf | t0 aac n=130 trim=1024 first=2 dts-shift=0 edits=[(2995, 1024)] display-syncs=0 |
| reel-952e6c | t0 aac n=141 trim=1024 first=2 dts-shift=0 edits=[(2986, 1024)] display-syncs=0 |
| reel-3c487f | t0 aac n=131 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-77de12 | t0 aac n=142 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-6a9b39 | t0 aac n=131 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-2f60a9 | t0 aac n=142 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-8416b3 | t0 aac n=142 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-87f4eb | t0 aac n=142 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-b0d504 | t0 aac n=142 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-09cb74 | t0 aac n=142 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-016325 | t0 aac n=142 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-8f8a34 | t0 alac n=33 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0 |
| reel-06ec05 | t0 alac n=35 trim=0 first=1 dts-shift=0 edits=[(2987, 0)] display-syncs=0 |
| reel-4671cb | t0 alac n=33 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0 |
| reel-865f8c | t0 alac n=36 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0 |
| reel-8a25a9 | t0 alac n=33 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0 |
| reel-7aa013 | t0 alac n=36 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0 |
| reel-39971f | t0 alac n=35 trim=0 first=1 dts-shift=0 edits=[(2987, 0)] display-syncs=0 |
| reel-4d4b32 | t0 alac n=36 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0 |
| reel-35c6ac | t0 aac n=131 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-3541cf | t0 av1 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-3a36b3 | t0 av1 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-8e39b7 | t0 av1 n=120 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0 |
| reel-bc52fc | t0 av1 n=120 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0 |
| reel-917bf3 | t0 av1 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-03a989 | t0 h264 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-2a0f4a | t0 h264 n=120 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0 |
| reel-78679c | t0 h264 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-3a3196 | t0 h264 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-28890f | t0 h264 n=120 trim=0 first=1 dts-shift=0 edits=[(4000, 1024)] display-syncs=4 |
| reel-74c076 | t0 hevc n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 2002)] display-syncs=1 |
| reel-31ba56 | t0 hevc n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 2002)] display-syncs=1 |
| reel-1495df | t0 hevc n=120 trim=0 first=1 dts-shift=0 edits=[(2000, 512)] display-syncs=1 |
| reel-d2e923 | t0 hevc n=120 trim=0 first=1 dts-shift=0 edits=[(2000, 512)] display-syncs=1 |
| reel-93e76a | t0 hevc n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 2002)] display-syncs=1 |
| reel-dc4c56 | t0 hevc n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 2002)] display-syncs=1 |
| reel-c7ec91 | t0 vp9 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-8be70a | t0 vp9 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-cb7c25 | t0 vp9 n=120 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0 |
| reel-488b13 | t0 vp9 n=120 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0 |
| reel-287488 | t0 vp9 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0 |
| reel-cf0757 | t0 h264 n=48 trim=0 first=1 dts-shift=0 edits=[(2002, 0)] display-syncs=0<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-fb26dc | t0 h264 n=50 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-51ab95 | t0 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 ac3 n=58 trim=256 first=1 dts-shift=0 edits=[(1994, 256)] display-syncs=0 |
| reel-ae5190 | t0 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 ac3 n=63 trim=256 first=1 dts-shift=0 edits=[(1994, 256)] display-syncs=0 |
| reel-28b490 | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-744e6f | t0 aac n=131 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-35f48e | t0 h264 n=150 trim=0 first=1 dts-shift=0 edits=[(5000, 0)] display-syncs=0<br>t1 aac n=217 trim=1024 first=2 dts-shift=0 edits=[(5000, 1024)] display-syncs=0 |
| reel-9ebdcf | t0 av1 n=3 trim=0 first=1 dts-shift=0 edits=[(100, 0)] display-syncs=0 |
| reel-f2f88f | t0 av1 n=3 trim=0 first=1 dts-shift=0 edits=[(100, 0)] display-syncs=0 |
| reel-0464d1 | t0 av1 n=3 trim=0 first=1 dts-shift=0 edits=[(100, 0)] display-syncs=0 |
| reel-56569e | t0 av1 n=3 trim=0 first=1 dts-shift=0 edits=[(100, 0)] display-syncs=0 |
| reel-70ca66 | t0 av1 n=3 trim=0 first=1 dts-shift=0 edits=[(100, 0)] display-syncs=0 |
| reel-38caa5 | t0 av1 n=3 trim=0 first=1 dts-shift=0 edits=[(100, 0)] display-syncs=0 |
| reel-2bd4aa | t0 h264 n=120 trim=0 first=1 dts-shift=0 edits=[(4000, 0)] display-syncs=0<br>t1 aac n=174 trim=1024 first=2 dts-shift=0 edits=[(4000, 1024)] display-syncs=0 |
| reel-02e079 | t0 h264 n=150 trim=0 first=1 dts-shift=0 edits=[(5000, 0)] display-syncs=0 |
| reel-30f5ba | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-cae864 | t0 h264 n=10 trim=0 first=1 dts-shift=0 edits=[(400, 1024)] display-syncs=1 |
| reel-44c26e | t0 hevc n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 1024)] display-syncs=1<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-6162b2 | t0 hevc n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 1024)] display-syncs=1<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-0037c9 | t0 hevc n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=2 |
| reel-ac4e0d | t0 hevc n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 1024)] display-syncs=4 |
| reel-6e8191 | t0 hevc n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=2 |
| reel-12fdcc | t0 hevc n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=2 |
| reel-ec7ff8 | t0 hevc n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=2 |
| reel-91a0c8 | t0 hevc n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=2 |
| reel-bde851 | t0 h264 n=150 trim=0 first=1 dts-shift=0 edits=[(5000, 1024)] display-syncs=1 |
| reel-1d8055 | t0 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0 |
| reel-3cba2f | t0 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0<br>t2 aac n=45 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0<br>t3 ac3 n=58 trim=256 first=1 dts-shift=0 edits=[(1994, 256)] display-syncs=0 |
| reel-ed5ee2 | t0 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0<br>t2 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-2702b9 | t0 h264 n=25 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=1 |
| reel-c19501 | t0 h264 n=48 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-9204e0 | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-2a3901 | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-c355dd | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-e04d2d | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-906dbd | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-bf1403 | t0 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 aac n=2585 trim=1024 first=2 dts-shift=0 edits=[(60000, 1024)] display-syncs=0 |
| reel-994771 | t0 h264 n=90 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0<br>t1 aac n=131 trim=1024 first=2 dts-shift=0 edits=[(3000, 1024)] display-syncs=0 |
| reel-6be9ed | t0 h264 n=120 trim=0 first=1 dts-shift=0 edits=[(4000, 0)] display-syncs=0<br>t1 aac n=174 trim=1024 first=2 dts-shift=0 edits=[(4000, 1024)] display-syncs=0 |
| reel-726d01 | t0 alac n=33 trim=0 first=1 dts-shift=0 edits=[(3000, 0)] display-syncs=0 |
| reel-ddb243 | t0 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 h264 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0 |
| reel-122fb3 | t0 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0<br>t1 vp8 n=60 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| reel-f3ec48 | t0 vp9 n=60 trim=0 first=1 dts-shift=0 edits=[(2000, 0)] display-syncs=0<br>t1 aac n=88 trim=1024 first=2 dts-shift=0 edits=[(2000, 1024)] display-syncs=0 |
| reel-b00a50 | t0 av1 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 0)] display-syncs=0 |
| reel-e2d47f | t0 h264 n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=1 |
| reel-a137c5 | t0 hevc n=30 trim=0 first=1 dts-shift=0 edits=[(1000, 1024)] display-syncs=1 |
| reel-e99558 | t0 h264 n=19226 trim=0 first=1 dts-shift=0 edits=[(640867, 6000)] display-syncs=68<br>t1 aac n=30037 trim=1024 first=2 dts-shift=0 edits=[(640768, 1024)] display-syncs=0 |
| reel-f45d22 | t0 h264 n=1611 trim=0 first=1 dts-shift=0 edits=[(83, -1), (40315, 7507)] display-syncs=8<br>t1 aac n=3151 trim=0 first=1 dts-shift=0 edits=[(62, -1), (40332, 0)] display-syncs=0<br>t2 ac3 n=2100 trim=0 first=1 dts-shift=0 edits=[(83, -1), (40320, 0)] display-syncs=0 |
| reel-f780b5 | t0 h264 n=175728 trim=0 first=1 dts-shift=0 edits=[(83, -1), (7329322, 7507)] display-syncs=1227<br>t1 aac n=343562 trim=0 first=1 dts-shift=0 edits=[(62, -1), (7329323, 0)] display-syncs=0<br>t2 ac3 n=229042 trim=0 first=1 dts-shift=0 edits=[(83, -1), (7329344, 0)] display-syncs=0 |
| reel-7141d4 | t0 h264 n=311 trim=0 first=1 dts-shift=0 edits=[(159232, 512)] display-syncs=2<br>t1 aac n=447 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| reel-2760de | t0 h264 n=23823 trim=0 first=1 dts-shift=0 edits=[(596170, 2002)] display-syncs=632<br>t1 ac3 n=31052 trim=None first=1 dts-shift=0 edits=[] display-syncs=0<br>t2 aac n=46581 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| reel-1ce6ee | t0 h264 n=129531 trim=None first=1 dts-shift=0 edits=[] display-syncs=1023<br>t1 aac n=126625 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| reel-c87873 | t0 h264 n=194024 trim=0 first=1 dts-shift=0 edits=[(8092418, 2002)] display-syncs=1707<br>t1 aac n=379336 trim=1024 first=2 dts-shift=0 edits=[(8092480, 1024)] display-syncs=0 |
| reel-052c81 | t0 h264 n=451 trim=None first=1 dts-shift=600000 edits=[] display-syncs=5<br>t1 aac n=650 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| reel-8c395a | t0 h264 n=198144 trim=0 first=1 dts-shift=0 edits=[(4953600, 2000)] display-syncs=1372<br>t1 aac n=387044 trim=1024 first=2 dts-shift=0 edits=[(4954150, 1024)] display-syncs=0 |
| reel-612536 | t0 h264 n=1448 trim=0 first=1 dts-shift=0 edits=[(48267, 512)] display-syncs=9<br>t1 aac n=2081 trim=0 first=1 dts-shift=0 edits=[(48321, 0)] display-syncs=0 |
| reel-fe3979 | t0 h264 n=6342 trim=0 first=1 dts-shift=0 edits=[(126840, 256)] display-syncs=23<br>t1 aac n=5465 trim=0 first=1 dts-shift=0 edits=[(126898, 0)] display-syncs=0<br>t2 mov_text n=98 trim=0 first=1 dts-shift=0 edits=[(128660, 0)] display-syncs=0 |
| reel-f9ad5b | t0 aac n=8991 trim=0 first=1 dts-shift=0 edits=[(208771, 0)] display-syncs=0<br>t1 h264 n=12511 trim=0 first=1 dts-shift=0 edits=[(16, -1), (208726, 1501)] display-syncs=107 |
| reel-f9ce99 | t0 h264 n=185 trim=0 first=1 dts-shift=0 edits=[(6173, 1001)] display-syncs=4<br>t1 aac n=268 trim=0 first=1 dts-shift=0 edits=[(6223, 0)] display-syncs=0 |
| reel-ab1f6d | t0 h264 n=529 trim=0 first=1 dts-shift=0 edits=[(17634, 1024)] display-syncs=9<br>t1 aac n=761 trim=1024 first=2 dts-shift=0 edits=[(17625, 1024)] display-syncs=0 |
| reel-a7d08b | t0 h264 n=918 trim=0 first=1 dts-shift=0 edits=[(30600, 1024)] display-syncs=16<br>t1 aac n=1326 trim=1024 first=2 dts-shift=0 edits=[(30766, 1024)] display-syncs=0 |
| reel-5d8019 | t0 h264 n=947 trim=0 first=1 dts-shift=0 edits=[(31567, 1024)] display-syncs=16<br>t1 aac n=1361 trim=1024 first=2 dts-shift=0 edits=[(31577, 1024)] display-syncs=0 |
| reel-28cd87 | t0 h264 n=1009 trim=0 first=1 dts-shift=0 edits=[(33634, 1024)] display-syncs=17<br>t1 aac n=1453 trim=1024 first=2 dts-shift=0 edits=[(33693, 1024)] display-syncs=0 |
| reel-902f91 | t0 h264 n=2004 trim=0 first=1 dts-shift=0 edits=[(66800, 1024)] display-syncs=34<br>t1 aac n=2877 trim=1024 first=2 dts-shift=0 edits=[(66758, 1024)] display-syncs=0 |
| reel-a8fbeb | t0 h264 n=884 trim=0 first=1 dts-shift=0 edits=[(29467, 1024)] display-syncs=15<br>t1 aac n=1272 trim=1024 first=2 dts-shift=0 edits=[(29505, 1024)] display-syncs=0 |
| reel-d0785e | t0 h264 n=2180 trim=0 first=1 dts-shift=0 edits=[(72667, 1024)] display-syncs=37<br>t1 aac n=3133 trim=1024 first=2 dts-shift=0 edits=[(72703, 1024)] display-syncs=0 |
| reel-157ca3 | t0 h264 n=3755 trim=0 first=1 dts-shift=0 edits=[(125167, 1024)] display-syncs=63<br>t1 aac n=5394 trim=1024 first=2 dts-shift=0 edits=[(125225, 1024)] display-syncs=0 |
| reel-639a56 | t0 h264 n=1418 trim=0 first=1 dts-shift=0 edits=[(47267, 1024)] display-syncs=24<br>t1 aac n=2037 trim=1024 first=2 dts-shift=0 edits=[(47274, 1024)] display-syncs=0 |
| reel-ee92e2 | t0 h264 n=1022 trim=0 first=1 dts-shift=0 edits=[(34067, 1024)] display-syncs=18<br>t1 aac n=1471 trim=1024 first=2 dts-shift=0 edits=[(34133, 1024)] display-syncs=0 |
| reel-ccbe4e | t0 h264 n=3152 trim=0 first=1 dts-shift=0 edits=[(105067, 1024)] display-syncs=53<br>t1 aac n=4528 trim=1024 first=2 dts-shift=0 edits=[(105109, 1024)] display-syncs=0 |
| reel-010d74 | t0 h264 n=4907 trim=0 first=1 dts-shift=0 edits=[(163567, 1024)] display-syncs=82<br>t1 aac n=7060 trim=1024 first=2 dts-shift=0 edits=[(163909, 1024)] display-syncs=0 |
| reel-3be877 | t0 h264 n=3853 trim=0 first=1 dts-shift=0 edits=[(128434, 1024)] display-syncs=65<br>t1 aac n=5533 trim=1024 first=2 dts-shift=0 edits=[(128449, 1024)] display-syncs=0 |
| reel-dfa330 | t0 h264 n=2015 trim=0 first=1 dts-shift=0 edits=[(67167, 1024)] display-syncs=34<br>t1 aac n=2893 trim=1024 first=2 dts-shift=0 edits=[(67152, 1024)] display-syncs=0 |
| reel-436303 | t0 h264 n=439 trim=0 first=1 dts-shift=0 edits=[(224768, 512)] display-syncs=6<br>t1 aac n=631 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| reel-4de659 | t0 aac n=4099 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| reel-58a800 | t0 h264 n=3188 trim=0 first=1 dts-shift=0 edits=[(127520, 0)] display-syncs=0<br>t1 aac n=5979 trim=1024 first=2 dts-shift=0 edits=[(127530, 1024)] display-syncs=0 |
| reel-286c86 | t0 h264 n=2392 trim=None first=1 dts-shift=0 edits=[] display-syncs=0<br>t1 aac n=4678 trim=None first=1 dts-shift=0 edits=[] display-syncs=0<br>t2 mov_text n=43 trim=None first=1 dts-shift=0 edits=[] display-syncs=0 |
| ok |  |

## Left

- The implicit-SBR rate stays with edith's `mp4_aac_prime`; ec-mp4 reports the core rate for it.
- `tests/data` rotation fixtures and the generated corpus need ffmpeg; the corpus generator is `scripts/gen-mp4-parity-corpus.sh`.
- The `EC_EDITH_ASSETS` default is this machine's path; the test prints a note and skips when the directory is absent.
- Edit-list application (`Sample::dts`/`cts` shifted by ffprobe's rule) is not what edith's frame index uses; edith keeps its own `presentation_trim`/`first_frame_sample`, and the swap must pass those two through.
