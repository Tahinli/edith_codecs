# lane-golomb r1 — the edge partition bit was read and thrown away

## Verdict
The "a Golomb tail longer than this decoder reads" refusal on the Hunger Games head was, as
suspected (`refusal-from-own-desync`), a symptom. Root cause: at a frame edge the partition
symbol is a single *gathered* bit, and this decoder read it and **discarded** it, always
inferring `PARTITION_SPLIT`. libaom `ec_read_partition_impl` (decodeframe.c:1255):
`!has_rows && has_cols` → `read ? PARTITION_SPLIT : PARTITION_HORZ`; the mirror case →
`PARTITION_VERT`. Class: `parsed-then-discarded`.

Charter STEP-2 fallback ("libaom read_golomb caps at 32 bits, extend the reader") is FALSE:
`~/.cache/aom-oracle/src/av1/decoder/decodetxb.c:22` aborts the frame at `length > 20`, exactly
our cap. The cap was never the bug and is unchanged.

## First divergent element (EVIDENCE)
EVIDENCE: /tmp/.../scratchpad/{ao-all.txt,ours-all.txt} | instrumented aomdec vs decode_probe,
EC_TRACE_MODE+EC_TRACE_MODE_STEP+EC_TRACE_COEFF on the 2-frame extract f2.obu, ladders aligned
element by element on msac RANGE | first diff at element 12053: key frame, **mi_row=400
mi_col=0** (last, 8-pixel-tall superblock row of a 1608-tall frame), `name=skip`
oracle rng=34753 vs ours rng=43258. Oracle codes that superblock as one `BLOCK_64X32`
(`EC_IMODE ... bsize=11`, PARTITION_HORZ); we forced SPLIT and descended to 16x16.
After the fix the range ladder is element-exact for all 13712 shared elements of the key frame
(EVIDENCE: /tmp/.../scratchpad/{a4.n,o4.n} | awk first-diff | idx 13713, inside a later INTER
frame, i.e. past the whole key frame).

## Fix
`crates/ec-av1/src/decode.rs` — every edge-partition site, both tile paths, all three levels
(64/32/16, intra and inter): the gathered bit now names HORZ/VERT vs SPLIT. The 64-level
HORZ/VERT arms only decode the second half when it is inside the frame (`has_rows`/`has_cols`),
mirroring libaom `decode_partition`. The intra 16x16 edge site can only walk four SPLIT leaves,
so a 0 bit refuses by name there (new string, listed in `refusal_inventory.rs`) instead of
desyncing.

## Gates
- NEW `stream::tests::the_hunger_games_head_key_frame_decodes_pixel_exact` — the film itself:
  `crates/ec-av1/fixtures/hg_head_key_frame.obu` (147 B, first frame of his 3840x1608
  yuv420p10le release) decodes and matches `ffmpeg -pix_fmt yuv420p10le` on Y, U and V.
  EVIDENCE: cargo test -p ec-av1 --lib -- the_hunger_games_head_key_frame | 1 passed |
  Y/U/V all equal to ffmpeg's decode, 3840x1608.
- Full lib suite: `$HOME/.cache/golomb-suite.log` — **337 passed / 0 failed / 31 ignored**.

## Film probes after the fix
- `hg-head.obu` (18 frames): key frame + first inter frames now entropy-exact; stops at
  "an inter SB-level partition type other than NONE or SPLIT" — the same edge partition, now
  correctly decoded as HORZ, on an INTER superblock row the inter path cannot code (rect inter
  residual coding does not exist here). Next lane.
- `hg5.obu`: "a 32x32-level 1:4 strip with a split transform (depth=2)" (unchanged by this lane).

---

# lane-golomb r2 — the 32x32-level frame-edge HORZ/VERT second half

## Must-fix (verifier golomb1): fixed
Since r1 the 32-level edge read can return HORZ/VERT (bit 0), so the second 32x16/16x32 half is
out of frame and must not be decoded. libaom `decode_partition`:
`case PARTITION_HORZ: decode_block(top); if (has_rows) decode_block(bottom)` (mirror for VERT).

- `crates/ec-av1/src/decode.rs:10887` (intra `PARTITION_HORZ` @32) — second 32x16 now behind `if has_rows32`.
- `crates/ec-av1/src/decode.rs:10928` (intra `PARTITION_VERT` @32) — second 16x32 behind `if has_cols32`.
- `crates/ec-av1/src/decode.rs:17900` (inter `PARTITION_HORZ` @32) — same guard, `has_rows32`.
- `crates/ec-av1/src/decode.rs:18008` (inter `PARTITION_VERT` @32) — same guard, `has_cols32`.
- `crates/ec-av1/src/decode.rs:1600` — new `EDGE32_HITS` counter (+ `edge32_hits()`, `bump_edge32`):
  slot 0/1 = 32-level edge bit read as HORZ/VERT vs SPLIT, slot 2 = a bottom-edge HORZ strip
  decoded (`has_rows32 == false`), slot 3 = a right-edge VERT strip.

AB / 1:4 arms at the 32 level need NO guard: at an edge libaom's `ec_read_partition_impl` can only
yield HORZ, VERT or SPLIT, so those arms are unreachable there (stated per charter).
16-level edge rect arms swept and left as they are: intra refuses by name
("a 16x16 block at the true frame edge coded as a rect strip rather than SPLIT"), inter falls to
"an inter partition below 16x16 other than SPLIT".

## Gate
NEW `stream::tests::a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition_decodes_pixel_exact`
(crates/ec-av1/src/stream.rs:14800+): 192x80 (bottom edge, mi_rows=20) and 80x192 (right edge),
testsrc2 + seeded `noise`, cq {35,40,45,50,55,57,59,61}, 8-bit AND 10-bit (bit_depth asserted),
plus a 5-frame INTER arm; every decode pixel-compared against ffmpeg, counters sampled per attempt
and summed only over compared attempts.

EVIDENCE: $HOME/.cache/golomb-suite-r2.log + gate stdout | cargo test -p ec-av1 --lib -- --nocapture
a_real_aomenc_stream_with_a_32x32_frame_edge | 32 pixel-exact attempts, 8 named refusals,
edge32 bits [horz_or_vert=0 split=178] — the 32-level edge read fires 178 times and every attempt
decodes pixel-exact.

Recipe deviations from the charter, with reasons (measured, not assumed):
- `--min-partition-size=32`, not 8: at 8 (and at 16) the sub-16 AB / split-transform strip arms
  refuse by name before the 32-level edge is reached — 40/40 attempts refused.
- `--tune-content=film` + `noise`: without them aomenc's screen-content detection makes every
  attempt refuse at "a HORZ/VERT intra strip in a screen-content frame" (the verifier's smptebars
  trap, reproduced here with plain testsrc2).

## OPEN DEFECT (fix-now, next round) — the edge HORZ strip's pixels
With detail in the straddling band aomenc always answers the edge bit with SPLIT (178/178), so the
HORZ/VERT arm never fires. Leaving that band FLAT (`pad=...:gray`) forces it:
EVIDENCE: gate stdout | cargo test -p ec-av1 --lib -- --ignored --nocapture
a_32x32_frame_edge_rect_partition_with_a_flat_band | 192x80 cq40 8-bit, `edge32=[2, 0, 2, 0]`
(two bottom-edge HORZ strips decoded), plane Y 1081 pixels differ, first at row 62 col 128,
diffs per row confined to rows 62..79 (~64 of 192 columns) — exactly the two 32x16 strips plus the
deblock bleed above them; every other pixel of the frame matches ffmpeg.
Reading: the guard itself is validated (no desync — the rest of the frame is entropy- and
pixel-exact where before it would have consumed an out-of-frame half), but the strip's own
reconstruction (prediction/neighbour availability at the frame edge) is wrong. Pinned as
`#[ignore]`d test `a_32x32_frame_edge_rect_partition_with_a_flat_band_decodes_pixel_exact` so the
suite stays green and the defect stays runnable. Disposition: fix-now for r3 (owner: this lane).

## Suite (r2)
`$HOME/.cache/golomb-suite-r2.log` — **337 passed / 1 FAILED / 32 ignored** (618 s).
The failure is NOT this round's arm and I could not attribute it to this diff:
`stream::tests::a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact`
panics at stream.rs:8479 "the stream decoded but no coded (non-skip) rect leaf fired". Its fixture
is a 64x64 mandelbrot frame — one whole superblock, `mi_cols = mi_rows = 16`, so
`has_rows32`/`has_cols32` are true everywhere and none of this round's four guards (nor r1's
edge reads) can execute on it. Deterministic (fails standalone in 0.34 s, same message).
Disposition: deferred(a bisect of eacd7fd vs 3e4ce89 in a detached worktree, or the owner of
`rect_leaf_coeff_hits`) — it is either pre-existing on eacd7fd (r1's suite log predates today's
oracle/ffmpeg state) or a sibling-gate regression from a path I did not touch. NOT claimed green.
