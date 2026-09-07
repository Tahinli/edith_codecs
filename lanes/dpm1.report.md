# lane-dpm1 — the native gate's decode assertion, all reference points

## (0) The assertion now COLLECTS (encode.rs)
`external_ladder` no longer panics on the first bad reference point: every
failure is printed at once (`LADDER DECODE FAILURE: ...`) and pushed onto
`LADDER_FAILURES`; each gate calls `assert_ladder_decodes()` after its loop, so
the whole BD table still prints and every bad point is named. First run already
paid for itself: it found a SECOND failure (screen capture, crf 5) the
panic-at-first version hid, and it showed the bars failure is the 2160p clip,
not the 1080p one.

## (1) bars ±1 — the charter's premise was mis-attributed
`bars 1080p` libaom crf 45 decodes EXACT (reproduced standalone: crop
1920x1024+0+28, 12 frames, -g 12, cpu-used 6, crf 45 -> our decode == ffmpeg,
byte for byte). The real failing row is **bars 2160p** (same crf 45, frame 1,
Y sample 66, ours 146 / ffmpeg 145). Status below.

## (2) film B crf 5 — STALE premise, already closed
Regenerated from the manifest (film B, 00:40:00, crop 1920x1024+960+292):
`-cpu-used 6 -crf 5 -g 48` and `-g 12` both decode **sample-exact** vs ffmpeg
at this head (48 and 12 frames). No Golomb refusal. The lane-dkey/dgolomb
reports' "4th inter frame refuses" is pre-merge state.

## (3) force_integer_mv fixture — now SENSITIVE
`stream.rs a_libaom_force_integer_mv_stream_decodes_exact`: source replaced by a
GLOBAL 5 px/frame vertical scroll of the screen-detector pattern, frozen on
frame 8, 16 frames, -g 16. With the fix stubbed (`if false && force_integer_mv`)
it now decodes WRONG at frame 8 luma sample 25695 (20 vs ffmpeg 19); with the
fix it is exact. Blind recipes tried: patch-in-static-background (the old one),
3 patches at 3 speeds with freezes at {5,9} / {4,8,12} / {8}, 3 px scroll.
Instrumented finding: a +64 perturbation of rounded candidates DID change the
output on those streams, so the rounded candidates were consumed but only in
slots whose ±4 eighth-pel difference never reached a coded block's mv.

## OPEN
* bars 2160p libaom crf 45 frame 1 Y sample 66 (+-1 reconstruction).
* screen capture (OBS .mkv row) libaom crf 5 frame 0 (KEY) Y sample 0: 126 vs
  33 — a key-frame desync at the first sample.
