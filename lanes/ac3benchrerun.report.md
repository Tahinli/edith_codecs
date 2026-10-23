# lane ac3benchrerun: AC-3 bench parity re-run after ac3parity

Base: main 654717d7. Branch lane/ac3benchrerun. No decoder code changed.

## Goal
Re-measure the benchrows AC-3 row (6-channel pink-noise AC-3 5.1 448 kb/s, 60 s) against
ffmpeg 8.1.3, which previously gave min per-channel correlation 0.980963 (FAIL), and re-run
the speed row. DONE gate: correlation >= 0.999 per channel.

## Method
- Clip regenerated with the bench-rows-clips.sh command (ffmpeg 8.1.3, six `anoisesrc` pink
  channels seeds 1-6, `pan=5.1(side)`, `-ar 48000 -c:a ac3 -b:a 448k`) into
  `~/.cache/ac3benchrerun-clips/` (not in git). ffprobe: ac3, 6 ch, 5.1(side), 448000 b/s.
  Reference: `ffmpeg -f f32le -ac 6` decode of that file.
- Parity: `ac3dec` (release, ec-ac3 example) decoded the clip on server host a
  (ffmpeg 8.1.3 there too). Correlation computed with a stdlib python script (host a has no
  numpy; `scripts/bench-rows-parity.py` needs numpy, so the same formula was used).
- Speed: local `ac3dec` build, best of 5 `realtime` lines, load 1-minute 1.22 at start.

## Results
| channel | corr vs ffmpeg | max abs err |
|---|---|---|
| ch0 | 1.000000 | 1.3e-5 |
| ch1 | 1.000000 | 1.2e-5 |
| ch2 | 1.000000 | 1.2e-5 |
| ch3 | 1.000000 | 1.0e-6 |
| ch4 | 1.000000 | 1.2e-5 |
| ch5 | 1.000000 | 1.2e-5 |

min_corr = 1.000000 (samples 2880000 per channel, 1875 frames, 0 decode errors). PASS.
Speed (local, best of 5): 322.2x realtime (320.1, 321.0, 320.5, 321.7, 322.2).
server run: 136.0x (not a timing claim; the speed row is local per D7).

## Gates (server host a, under remote-run lane ac3benchrerun)
- `cargo test --release -p ec-ac3`: all test binaries ok, 0 failed (exit 0).
- `cargo clippy -p ec-ac3 --all-targets -- -D warnings`: exit 0.
- Largest source file in crates/ec-ac3: 1230 lines (eac3.rs), under 5000.

## Changes
- `lanes/bench.md`: AC-3 row now check `min_corr=1.000000 ... PASS`, realtime 322.2x, load
  1.22; notes updated, with the old 0.980963 result kept as history.
- `DONE.md`: the ec-ac3 "Criteria met" caveat now states the 1.000000 result.
- No regression test added: the step 3 fix path was not needed.

## Verdict
DONE criterion (ec-ac3 per-channel correlation >= 0.999 vs ffmpeg on the bench clip): MET.

## Left
- The ffmpeg-generated clip is not committed (by design). Regenerate with the
  bench-rows-clips.sh AC-3 block to reproduce.
- Bit-exactness is still not claimed (unchanged).
