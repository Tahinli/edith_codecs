# lane-av1recon: recon wavefront bit-exact on 4:2:0, on by default at EC_AV1_THREADS > 1

Base: `main` at `add3513a`. Worktree `~/.cache/wt/av1recon`, branch `lane/av1recon`.

## Root cause

The wavefront was correct; the early filter release was not. In `decode_inter_frame_tile_with_cdfs`
and `decode_key_frame_tile_with_cdfs` the in-flight job started as the placeholder `(0, 0)` and the
first superblock visit called `wave_submit(.., 0, 0)` for it before anything was parsed. That
marked superblock (0, 0) as parsed with no job pushed. In a tile one superblock wide (any
frame <= 128 px wide at sb128, <= 64 px at sb64: the 128x96 aomenc qm stream), row 0 therefore
looked fully reconstructed at its own first `row_start`, `wave_rows_complete` returned 1, the
filter pipeline was released (`note(2)` = every band) and deblock/CDEF ran while the wavefront
workers were still writing. Result: nondeterministic scattered +-1 pixels (the md5 changed between
runs), only with the pipeline and recon > 1 (`EC_AV1_PIPE_FILTERS=0` or `recon=1` was exact).
Confirmed by an eprintln: `XNOTE sb_r=0 sb128=true px=128` at the first row start, and by
suppressing the early `note` (exact). Wide streams (1080p, 30 columns) were not exposed, which is
why the 1080p bench md5 matched.

## Changes

- `decode/frame_tile.rs`, `decode/key_tile.rs`: `wave_job` is an `Option`; nothing is submitted
  until a superblock has actually started (and the tail submit is guarded).
- `par.rs`: `AUTO_RECON` / `AUTO_FILTER` defaults used only when `EC_AV1_RECON_THREADS` /
  `EC_AV1_FILTER_THREADS` are unset; `DecodeSplit` installs them for one decode and restores the
  previous value on drop (so the encoder's own decodes stay serial). No-op under `cfg(test)`.
- `autothreads.rs`: `split(threads)`: 1 -> (1,1); else recon = 3n/4 clamped 2..4, filter = n/2
  clamped 1..3. New unit test. Header comment updated.
- `stream/mod.rs`: `decode_stream_with_threads` installs `DecodeSplit`.
- `EC_AV1_THREADS=1` is fully serial; explicit env values still win.
- `lanes/bench.md`: rows added.

## Verify

| command | host | result |
|---|---|---|
| `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4` (defaults) | c | 811 passed / 0 failed / 49 ignored |
| same with `EC_AV1_RECON_THREADS=4 EC_AV1_FILTER_THREADS=2` forced everywhere | b | 810 passed / 1 failed / 49 ignored. The one failure is `t03::a_real_aomenc_12bit_inter_sequence_decodes_pixel_exact`, which asserts a thread-local `mc_subpel_hits` counter the wavefront workers never bump (same class as the 150 counter gates in av1threads.report.md). It is not a pixel mismatch; there were 0 pixel mismatches, including `a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact` |
| `cargo clippy -p ec-av1 --all-targets -- -D warnings` | c | exit 0 |
| qm stream + autothreads tests with `EC_AV1_RECON_THREADS=4`, after the final collapsible-if edit | c | 5 passed |
| 128x96 qm-off stream, `decode_bench --raw` recon 4 vs recon 1, 6 runs | c | all 6 equal to serial (before the fix: 3 different md5s) |
| 1080p `decode_bench --raw` at EC_AV1_THREADS 1, 4, 6 | a | md5 `3adc24bb...` = ffmpeg |

The gate ran before the last edit (two `if waving { if let .. }` collapsed to `if waving && let ..`
for clippy); the edited build was re-checked by clippy and the 5 tests above.

## Speed (host a, 4 cores shared with two foreign processes; median of 3)

| EC_AV1_THREADS | fps |
|---|---|
| 1 | 22.0 |
| 2 | 28.5 |
| 4 | 31.0 |
| 6 | 28.6 |

DONE.md 1080p30 is met at 4 threads on this host (31.0 median, peaks 35). At 2 and 6 threads the
median is 28.5 to 28.6 under the foreign load (explicit knob runs reach 32 to 35 in quiet moments).
Run-to-run noise is +-15%.

## The intermittent t03 10-bit rect-transform failure

Not seen in the default or forced gate runs, so not investigated further. The wavefront race above
is unrelated to a dump-write ENOENT.

## Left

- Frame workers add nothing at 1080p and compete with recon threads; a later lane could cap them
  to 1 or 2 when the split is on.
- Counter gates are blind to the wavefront workers, so `cfg(test)` keeps the serial default.
