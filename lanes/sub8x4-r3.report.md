# lane-sub8x4 r3 report (branch `lane-sub8x4`, tip `f38f963`)

Merged `main` @ `9cca16e` (clean auto-merge, **zero conflicts** -- no hand-resolved
function to re-read), then two commits.

## r2 suite line (the unit that was still running at the start of this round)

`test result: FAILED. 419 passed; 3 failed; 38 ignored; 0 measured; 0 filtered out; finished in 674.52s`

The 3: `a_frame_edge_straddling_band_decodes_pixel_exact` (main's `1d2259c` was missing
from the lane -- fixed by the merge, see below) plus the two pre-existing zero-COUNTER
recipe defects this branch already carried (`..._16x16_level_1to4_partitions...`
stream.rs:11002, `real_aomenc_1to4_streams_..._rect_vartx_leaves_fire` stream.rs:8884).
Neither of those two is a pixel mismatch and neither moved this round.

## STEP 1 -- `13cd475`: the `--tile-columns=1` arm is back, and r2's premise was right

r2 excluded the arm with a measurement (desync at decode-order frame 4, identical with
`--min-partition-size=16 --max-partition-size=16`, i.e. a stream with no sub-8x8 block at
all) and named the suspect: main's `1d2259c`, "the saved motion field kept only the LAST
tile's columns -- `av1_copy_frame_mvs` read the mi grid through the per-tile reach clamp".
Confirmed: with the merge the arm is pixel-exact and the gate now carries the neighbour-map
tile arm COMMON asks every lane for (13 arms).

EVIDENCE: crates/ec-av1/src/stream.rs:30038-30044 | `cargo test -p ec-av1 --lib -- sub8 straddling obmc mv_stack 1to4 refusal_inventory gate_coverage` | `34 passed; 2 failed; 5 ignored` in 141.28s -- the 2 are the same two recipe defects above; `a_real_aomenc_inter_sequence_with_intra_sub8x8_leaves_decodes_pixel_exact` and `a_frame_edge_straddling_band_decodes_pixel_exact` both green

## STEP 2 -- film probes (release probe, each under `systemd-run --scope MemoryMax=6G`)

**The r1/r2 "no witness exists" dead-end EXPIRED**, exactly as this lane's r2 predicted:
every firing frame also carried a sub-8x8 intra leaf, which refused until r2 lifted it.

| cut | frames / wall | `intra16x4_in_inter` 16x4/4x16/chroma | `rect64_corner_tu` 64x32/32x64 |
|---|---|---|---|
| head of the 10-bit 3840x1608 stream (2 s) | **OK: 48 frames, no refusal** | 31 / 17 / 23 | 130 / 22 |
| same stream @ +300 s | REFUSED: a Golomb tail longer than this decoder reads | 175 / 1350 / 760 | 270 / 90 |
| 1080p 10-bit 128-SB cut @ 900 | REFUSED: an OBMC neighbour whose switchable interp filter was never recorded | 1 / 4 / 2 | 0 / 0 |
| same @ 5400 | REFUSED: same OBMC string | 15 / 49 / 35 | 0 / 0 |
| same @ 6300 | REFUSED: same OBMC string | 1 / 1 / 0 | 0 / 0 |
| same @ 8100 | REFUSED: an intra-coded 16x4/4x16 strip **on the inter block path** (decode.rs:7390 -- a DIFFERENT site from the one lifted here) | 0 / 0 / 0 | 1 / 0 |

Pixel compare of the only cut that finishes (`EC_PROBE_OUT16` vs
`ffmpeg -pix_fmt yuv420p10le -f rawvideo`, `diff16.py`, 889159680 B each side):
**decode-order frames 0..35 are byte-identical**; frame 36 is the first differing one
(1992768 of 18524160 bytes) and 36..47 all differ -- one wall, propagating.

EVIDENCE: ~/.cache/sub8x4-tmp/{o,r}.raw | release decode_probe + ffmpeg on the 2 s head cut, diff16.py 3840x1608 | frames 0-35 differ in 0 bytes, frame 36 in 1992768

## STEP 3 -- `f38f963`: both refusals narrowed, one film gate, one sibling fixed

* `EC_RECT64_SPLIT` removed (decode.rs:6890-6903): the un-split TX_64X32/TX_32X64 corner
  unit of a split intra strip is now the default path.
* `EC_INTRA16X4_DECODE` removed (decode.rs:21886-21903): the intra 16x4/4x16 strip inside
  an inter 16x16-level 1:4 partition decodes by default when it has a chroma-pair record.
* Fixture `crates/ec-av1/fixtures/hg_rect64_intra16x4_witness.obu` (`git add -f`, in
  `git ls-files`), 23472 bytes, sha256
  `c9e721088766163b9dbeb9fa8dd8b257ff0ec7f9157fc350de4d3c33cd9ec4e4` -- the first 50
  frame-carrying OBUs of the 2 s head cut. Cut TWICE from independent
  `ffmpeg -ss 0 -t 2 ... -c:v copy -f obu` runs: both segments hashed
  `96940689a8c6...` and both truncations hashed `c9e7210887...`.
* Gate `a_10bit_film_frames_with_intra_16x4_strips_and_rect64_corner_tus_decode_pixel_exact`
  (stream.rs, after the intra14 gate): 33 decode-order frames, all three planes, hard
  asserts on all five counters AND on ffmpeg's own frame count (no vacuous compare).
* Why the fixture is 50 OBUs and not the whole 2 s segment is stated in its doc comment:
  the full segment diverges at frame 36, and the prefix is the exact one, not a
  truncation chosen to hide that.
* **Neither refusal string leaves `refusal_inventory`** -- both are NARROWED. "a split
  intra strip whose transform unit is {tx_w}x{tx_h}" still stands for every shape other
  than 64x32/32x64; the 16x4/4x16 one still stands for a strip with no chroma-pair record
  (and decode.rs:7390 is a separate string, untouched). `refusal_inventory` and all 12
  `gate_coverage` tests green with no edit.

EVIDENCE: crates/ec-av1/fixtures/hg_rect64_intra16x4_witness.obu | `cargo test -p ec-av1 --lib -- a_10bit_film_frames_with_intra_16x4 refusal_inventory gate_coverage` | `13 passed; 0 failed` in 62.64s, gate printed `33 frames pixel-exact, intra16x4_in_inter 16x4=31 4x16=17 chroma_ref=23, rect64_corner_tu 64x32=90 32x64=5`

### Sibling sweep (COMMON's SIBLING GATES rule) -- found and fixed one

`a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact` was `#[ignore]`d
with the note "un-ignore when the 64x32/32x64 luma coefficient tables land". They landed
here, so it is un-ignored. Running it for the FIRST time (it had never executed) showed
its `frames.len() == 2` assert was an untested assumption: a raw
`ffmpeg -i <fixture> -pix_fmt yuv420p10le -f rawvideo` writes exactly 18524160 bytes =
ONE 3840x1608 10-bit frame, so the second frame OBU is a no-show. Corrected to 1, plus an
ffmpeg-frame-count equality assert so the compare cannot silently go vacuous.

EVIDENCE: ~/.cache/sub8x4-tmp/{o,r}.raw on the intra14 fixture | release decode_probe + raw ffmpeg decode, diff16.py | 1 frame each side, `0:0` (zero differing bytes); gate green, `480 intra 1:4 strips in an inter frame, the shown frame pixel-exact`

## STEP 4 -- full suite

Unit `sub8x4-suite-r3-1788364529.service`, log `$HOME/.cache/sub8x4-suite-r3.log`
(private path per COMMON):

`test result: FAILED. 422 passed; 2 failed; 37 ignored; 0 measured; 0 filtered out; finished in 676.59s`

Against r2's `419 passed; 3 failed; 38 ignored`: +3 passed (the straddling gate the merge
fixed, plus this round's two film gates net of the un-ignore), -1 failed, -1 ignored (the
intra14 gate). The 2 remaining failures are the same two zero-COUNTER 1:4 recipe defects
r2 already carried, with the same messages; no new failure, no gate regressed.

## Open residue

* `fix-now (next round)` -- the head cut's wall at decode-order **frame 36**: 33 frames
  are pinned and exact, frames 36..47 differ. No refusal fires, so this is a silent
  reconstruction/entropy defect and the fixture at N=52/N=56 (still exact, 33/35 frames)
  brackets it: N=58 (37 frames) is the smallest truncation that includes it.
* `deferred(a lane that owns the OBMC interp-filter record)` -- three of the four 1080p
  128-SB cuts now stop at "an OBMC neighbour whose switchable interp filter was never
  recorded", which is not this lane's shape.
* `deferred(the 16x16-level 1:4 INTER partition itself)` -- decode.rs:7390, the OTHER
  intra-16x4 refusal, on the inter block path; it is what the 8100 cut stops at and it is
  refused before the partition this round unblocked.
* `accepted` -- the two zero-COUNTER 1:4 recipe defects (stream.rs:8884, :11002) are pure
  gate-recipe defects carried in from before this lane and did not move this round.
