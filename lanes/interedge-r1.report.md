# lane-interedge r1 -- INTER frame-edge half strips (h/w mod 16 == 8)

## Premise re-measured FIRST: the charter's defect was already fixed by lane-cdef

The charter's headline defect (192x72 8-bit inter, cq 32, frame 2 luma rows 64..=71
cols 8..=188, from lanes/edgeboth-r1.report.md) is **gone once lane-cdef 8c6065e is
merged** -- it was the CDEF `cdef_find_dir` crop-edge defect, not a partition defect.

EVIDENCE: `~/.cache/interedge-tmp/s192x72.obu` (sha256 a96cf5d0…2807, hashed twice, gen.sh)
| decode with the pre-cdef binary (`$HOME/.cache/cargo-target-edgeboth/…/decode_probe`) vs the
merged tree's binary, both against `ffmpeg -i s192x72.obu -pix_fmt yuv420p`
| pre-cdef: frames 2..5 mismatch, frame 2 `luma rows (64,71) cols (8,188) max 3`;
merged tree: **all 6 frames EXACT**.

## Root cause found and fixed this round (a different, real one)

`crates/ec-av1/src/decode.rs` inter tile, 32x32 level `PARTITION_HORZ` / `PARTITION_VERT`:
the SECOND 32x16 / 16x32 strip was decoded **unconditionally**. libaom
`decode_partition` (decodeframe.c, PARTITION_HORZ/PARTITION_VERT) guards it with
`has_rows` / `has_cols`, so at the true frame edge the second half is outside the frame
and carries no symbols and no grid writes. We read symbols the encoder never wrote.
The 64-level and 16-level arms already had the equivalent break; the intra path already
had both guards (decode.rs `if has_rows32` / `if has_cols32` in the key-frame tile) --
this was the inter path's missing twin (class *early-return / second-half at the edge*).

- `crates/ec-av1/src/decode.rs`: `if has_rows32 { … } else { bump_inter_edge_strip(2) }`
  and `if has_cols32 { … } else { bump_inter_edge_strip(3) }` around the second strip.
- `crates/ec-av1/src/decode.rs`: new `INTER_EDGE_STRIP_HITS` counter
  (`[h64, v64, h32, v32, h16, v16]`) + `bump_inter_edge_strip` / `inter_edge_strip_hits`
  / `reset_inter_edge_strip_hits`, bumped at all three levels x both axes.
- `crates/ec-av1/examples/decode_probe.rs`: prints `inter_edge_strip:` counters.
- `crates/ec-av1/src/stream.rs`: lane-oddh r2's `#[ignore]`d inter arm
  `a_real_aomenc_inter_frame_edge_half_strip_decodes_pixel_exact` is **un-ignored** and
  rewritten as a 5-size sweep + helper `inter_edge_strip_attempt`.

EVIDENCE: `~/.cache/interedge-tmp/sweep2.sh` output | same tree with the two guards forced
to `if true` (rebuild, then restored) | **72x192 8-bit cq 32 REFUSES** ("a non-skip
rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding" -- a desync
symptom) and **104x192 8-bit cq 32 frame 5 luma rows 127..191 wrong**; with the guards
both decode luma-exact.

## Gate (GREEN)

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-interedge EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 \
PATH="$HOME/.cache/aom-oracle/build:$PATH" cargo test -p ec-av1 --lib inter_frame_edge_half_strip -- --nocapture
```
Real aomenc, mandelbrot, sizes 192x72 / 192x88 / 72x192 / 88x192 / 104x192 (h or w mod
16 == 8), 8- and 10-bit, cq 20/32/45, 6 frames, `--lag-in-frames=0` (decode order ==
display order, EVERY frame compared), rect partitions ON (with them off libaom answers
the forced gathered edge symbol SPLIT every time and no edge strip fires -- lane-oddh
r2's finding, re-measured r1), every other lane's open tool pinned off.

EVIDENCE: gate stdout | 30 attempts aomenc -> ec-av1 -> ffmpeg, per-frame plane compare |
`30 attempts luma-exact over 5 sizes, edge hits (h64,v64,h32,v32,h16,v16)=[109,34,0,3,2,432],
0 refusals; chroma-only frames 98 (control 192x128: hits [0,0,0,0,0,0], chroma-only frames 4)`

DEVIATION from the charter's `out_of_scope_mismatch == 0`: the compare is **luma-exact per
frame**, chroma excluded. Reason: a chroma-only inter residue that has nothing to do with
the frame edge is open on main (lanes/tiny-r3, lanes/gmaffine-r4, lanes/palette2-r8) and
this gate's CONTROL arm reproduces it at 192x128 -- a size with no frame-edge node at all,
`inter_edge_strip_hits` all zero, luma exact, chroma-only frames 4. Disabling CDEF and
loop restoration does not remove it (`~/.cache/interedge-tmp/sweep3.sh`), so it is a rect
inter chroma prediction/residual defect, not an edge one. The control's luma-exact +
zero-hits assert is what makes the exclusion a claim about a foreign defect.

`h32` (a bottom-edge HORZ at the 32 level) is NOT asserted nonzero: no recipe found makes
libaom answer that gathered symbol HORZ (`~/.cache/interedge-tmp/h32.sh`: 3 sizes x 6 cq,
all SPLIT). It is the transposed twin of `v32`, which the gate does fire (3 hits), and both
sit in the same match arm pair.

## Refusals
None lifted -- no `refusal_inventory.rs` string covers the inter frame-edge half strip
(grep "half strip" / "frame edge": only lane-edgeboth's two both-axes strings, which are
that lane's, not on main). Inventory untouched (47).

## Film probe (Troy, 10-bit 1920x792, h mod 16 == 8)
`ffmpeg -ss 300 -t 2 -c:v copy -f obu` (3234673 bytes) -> `decode_probe`:
- stop string: `an inter 16x16-level 1:4 partition (HORZ_4/VERT_4 -- four 16x4 or 4x16
  inter strips; this decoder's inter path codes a 16x16 as NONE, HORZ, VERT, SPLIT or AB)`
  (the `-t 2` head extract at 0s stops earlier, at `a 128x128 superblock PARTITION_NONE
  root on an inter frame`).
- `EC_AV1_FINAL_DUMP` count: 1 (the key frame; the first inter frame refuses).
- cmp vs `ffmpeg -pix_fmt yuv420p10le`: frame 0 (key) is **NOT exact** -- small deltas,
  max 12 over the first 8 luma rows, 13710 of 15360 samples differ. New finding, see below.

## Suite
`cargo test -p ec-av1 --lib` under unit `interedge-suite-*`, log
`$HOME/.cache/interedge-suite-r1.log`: **385 passed, 0 failed, 32 ignored**, 954s
(lane-edgeboth's baseline on the same tree shape was 383/0/34 -- this round un-ignores
lane-oddh r2's inter arm, the cdef merge un-ignores the other).

## Residue
- deferred(a chroma lane): the rect-inter chroma-only residue above (192x128 control,
  cdef/restoration off, both depths) -- blocks a full-plane version of this gate.
- open(new, worth a lane): Troy 10-bit 1920x792 KEY frame at 300s is not pixel exact
  (max luma delta 12). Not edge-related: it is the key frame, `inter_edge_strip_hits`
  all zero.
- deferred(lane-edgeboth merge): 200x104 (both axes cut) still refuses on that lane's
  16x16 both-cut string, so it is not in this gate's size list.
