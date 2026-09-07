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

## (B) bars 2160p libaom crf 45 — FIXED (root cause, class, sweep, fixture)
First divergence (two instrumented decoders, `EC_TRACE_MODE_STEP` +
`EC_TRACE_COEFF`): INTER frame 1, block **mi(0,288)**, a 128x128 INTRA block
inside an inter frame, its FIRST chroma (U) transform unit -- aomdec reads
`txb_skip` on ctx 10, ours on ctx 11 (`get_txb_ctx`'s offset-10 rows, base 1 vs
0). The luma ladder and every symbol before it matched exactly.

Root cause, `crates/ec-av1/src/decode.rs`: a 128x128 block codes its chroma as
four TX_32X32 units, one per 64x64 mu chunk, and libaom stamps the coefficient
context PER UNIT. The block tail re-stamps those four units only when
`mu_chroma` is set -- the two INTER mu-chunk sites (27255, 28552) set it, the
INTRA-in-inter site did not, so the whole-block record left all four units
carrying the TOP-LEFT unit's level. Class **`override-slot-on-one-arm`** (a fix
installed on one arm of a set of twins). Sweep: all `CHROMA_SPLIT_TX_HITS`
sites -- the two key-frame ones (14364, 14703) have their own local re-stamp,
the three inter-frame ones now all set `mu_chroma`. No other site.

Why it read as a `+-1` at sample 66: the U-plane symbol desynced the tile, and
the loop-restoration coefficients live in the same tile data, so the whole
frame shifted by one level while 1.7M samples behind it went wrong.

Fixture: `stream.rs a_libaom_stream_with_128_intra_blocks_in_inter_frames_decodes_exact` -- the repo's own 2160p bars fixture at the native crop, libaom
`-cpu-used 6 -crf 45 -g 12`, asserts the tool fired and every frame is exact.
With `mu_chroma = false` on that arm it FAILS with "frame 1 plane Y differs
from ffmpeg at sample 66" (verified).

## (A) screen capture libaom crf 5 — root cause found, feature DEFERRED
That stream's key frame is coded **LOSSLESS** (`base_q_idx 0`, `lossless[seg]`
true; aomdec's own `EC_IMODE_VAL ... tx=0` shows TX_4X4 on a BLOCK_32X32).
libaom then (a) forces TX_4X4 + the Walsh-Hadamard transform everywhere and
codes NO `tx_depth` symbol, and (b) narrows `is_cfl_allowed` to
`plane_bsize == BLOCK_4X4`, which changes the `uv_mode` alphabet. Our tile
reader ignores `header.lossless` entirely: the FIRST block, mi(0,0), reads
`uv_mode` = 12 (UV_CFL_PRED) where aomdec reads 1 (V_PRED) -- symbol four of
the frame. Class `branch-dropped-as-unreachable` (the header computes
`lossless`/`coded_lossless` and nothing consumes it).

Shipped now: a NAMED REFUSAL in `stream.rs` (`a lossless frame (qindex 0) ...`)
so the decoder stops returning a picture that is wrong from luma sample 0.
`deferred: the lossless decode path (WHT 4x4, forced tx size, lossless CfL
rule, filter bypass) — a feature lane, not a fix — unblocks: implement
`inverse WHT + the TX_4X4 forcing + `is_cfl_allowed`'s lossless branch, gate
with this same screen row at crf 5.`

## OPEN
* The native gate's screen row now reports the refusal instead of a silent
  wrong decode; it is RED until the lossless lane lands.
