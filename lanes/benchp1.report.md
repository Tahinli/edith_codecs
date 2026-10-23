# benchp1: bench rows for ec-subs, ec-ass, ec-pgs, ec-ogg, ec-riff (DONE.md G6)

Base: main 654717d7. Branch lane/benchp1.

## Changes
- `crates/{ec-subs,ec-ass,ec-pgs,ec-ogg,ec-riff}/examples/benchp1.rs`: one bench example per crate.
  - ec-subs: `benchp1 <srt|vtt> <file> [reps]`, parse throughput and cue count.
  - ec-ass: `benchp1 ass <file> [reps]`.
  - ec-pgs: `benchp1 gen <out.sup> <seconds>` (synthetic .sup, same segment layout as the crate's tests) and `benchp1 decode <file.sup> [reps]`.
  - ec-ogg: `benchp1 <file.ogg> [reps]`, packets, bytes and demux time.
  - ec-riff: `benchp1 read <wav> [reps]` and `benchp1 write <wav> <out> <riff|rf64> [reps]`.
- `scripts/bench-benchp1-clips.sh`: ffmpeg clip generator (clips stay out of git).
- `lanes/bench.md`: new section `## lane benchp1` with 10 rows (G6).

## Inputs (ffmpeg 8.1.3, in ~/.cache/benchp1-clips)
- SRT and WebVTT: 2 h, 14400 cues each. Made by ffmpeg from an awk-written ASS script (20 styles, 14400 styled events).
- ASS: the same styled script, 2.35 MB.
- PGS: 2 h, 7200 shows plus 7200 clears (14400 display sets), 16.1 MB. ffmpeg has no PGS encoder, so `benchp1 gen` writes it.
- Ogg: 10 min stereo, Vorbis q4 (6.1 MB) and Opus 128k (6.5 MB).
- WAV: 10 min 24-bit 48 kHz 5.1, six pink-noise channels joined as 5.1, 518 MB.

## Verify commands and results
- Clippy, all five crates, `--all-targets -- -D warnings`, on server: all rc=0.
- `cargo test --release -p <crate>` on server: ec-subs 20 passed, ec-ass 8, ec-pgs 7, ec-ogg 18, ec-riff 22 passed with 1 ignored (pre-existing `#[ignore]`). 0 failed.
- Checks on server host a (ffprobe and ffmpeg):
  - SRT, VTT, ASS: cues 14400 = ffprobe subtitle packets 14400.
  - PGS: 43200 segments = ffprobe `-f sup` packets 43200. 14400 frames rendered, push errors 0.
  - Vorbis: 28140 packets, 6082163 bytes = ffprobe. Opus: 30001 packets, 6515119 bytes = ffprobe.
  - WAV read: 172800000 samples = 600 s x 48000 x 6.
  - WAV RIFF write and RF64 write: ffmpeg s24le md5 of the output = source md5 b405de61ecd1348c483a0c45184c1c9a. RF64 magic confirmed.
- Timing: best of 5, local 12-core machine (D7). Binaries built on host a, run locally. 1-minute load was 1.4 to 1.6 at the start of every row (all below 3).

## Results (local, best of 5, 1 thread)
| crate | input | MB/s | x realtime |
|---|---|---|---|
| ec-subs SRT | 2 h, 14400 cues | 122.47 | 365136x |
| ec-subs WebVTT | 2 h, 14400 cues | 125.32 | 733198x |
| ec-ass | 2 h, 14400 events | 104.15 | 319600x |
| ec-pgs decode | 2 h, 14400 sets | 0.98 | 437x |
| ec-ogg Vorbis demux | 10 min | 494.08 | 48780x |
| ec-ogg Opus demux | 10 min | 498.18 | 45802x |
| ec-riff WAV read | 10 min 5.1 24-bit | 1547.01 | 1790x |
| ec-riff RIFF write | 10 min 5.1 24-bit | 1431.72 | 1657x |
| ec-riff RF64 write | 10 min 5.1 24-bit | 1495.72 | 1731x |

## Findings
- ec-pgs `push` queues every finished display set as a full 1920x1080 canvas (about 8 MB) until `take_frame` drains it. Feeding the whole 2 h file in one push was OOM-killed on host a. Feeding one segment per push and draining each time keeps peak RSS near 26 MB. Not fixed here (no source change in this lane). A streaming caller must drain per push.
- ec-pgs decode is slow: 16.5 s local for 2 h (437x). The cost is the full-canvas render per display set. The host-a run of the same binary took 52 s; the local number is the one recorded.
- ec-riff write times are to the page cache (no fsync), so they measure encode plus buffered write, not disk speed.

## Left
- No edith test or G5 work here (separate gates).
- The pgs bench uses a synthetic .sup, not a real Blu-ray stream.
