# lane-intra14 r1 -- intra-coded 1:4 rect strips on the INTER block path

Branch `lane-intra14` off main `7a47fc1`. **RED**: the decoder work is done and
demonstrably fires, but NO real-aomenc attempt on this tree can be
pixel-compared, so the lifted refusal is **ungated** and this branch MUST NOT
merge as-is.

## What changed

* `crates/ec-av1/src/decode.rs:5987` (`decode_intra_rect_in_inter`) -- the
  `(size_group, tx_cat)` table gains the 1:4 rows `(8,32) -> (1,2)` and
  `(16,64) -> (2,3)`, read off libaom `size_group_lookup`
  (common_data.h:61) and `bsize_to_tx_size_depth_table` (blockd.h:1347). A 1:4
  strip then decodes through the KEY-FRAME readers that already prove the
  shape: `decode_block_rect4` (32-level 32x8/8x32) and `decode_block_rect64`
  (SB-level 64x16/16x64) -- transform tables, per-unit split walk, chroma,
  Reach, neighbour records, palette/filter-intra refusals all reused unchanged.
* `crates/ec-av1/src/decode.rs` `INTRA_IN_INTER_MODE` (thread-local) -- the one
  genuine difference on an inter frame is the mode-info read.
  `read_intra_mode_rect` now, when it is set, (a) takes the `skip` the inter
  path already read instead of re-reading skip/segment id/cdef/delta-q and
  (b) reads the luma mode off `y_mode[size_group]`
  (libaom `read_intra_block_mode_info`, decodemv.c:1189) instead of
  `kf_y_mode[above][left]` (decodemv.c's key-frame reader). Everything after
  that -- angle deltas (decodemv.c:1191/1203), uv_mode + CfL (1196-1201),
  palette (1215), filter intra (the `read_filter_intra_mode_info` tail) -- is
  bit-identical between the two libaom readers, which is why no other symbol
  needed a branch.
* Counters `INTRA_RECT4_IN_INTER_HITS` (0=64x16, 1=16x64, 2=32x8, 3=8x32),
  exported as `stream::intra_rect4_in_inter_counters()` and printed by
  `examples/decode_probe`.
* `refusal_inventory.rs:76` -- "an intra-coded 1:4 (or other non-2:1) rect
  strip on the inter block path" replaced by the narrower
  "an intra-coded 16x4/4x16 strip on the inter block path (the 16x16-level 1:4
  inter partition is refused before this point)": 16x4/4x16 is unreachable
  because `decode.rs:20332` refuses the 16x16-level 1:4 inter partition first,
  and `decode_block_rect4`'s tables are 32x8-specific.
* `stream.rs` -- gate `a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact`
  (+ `_10bit`), currently `#[ignore]`d with the blocker named (see below).

## Why the gate cannot pass on this tree (MEASURED)

40 gate attempts per depth + ~150 recipe-sweep attempts (frame sizes 64x64,
96x96, 128x128, 192x128, 256x192; mandelbrot fast-zoom, static-background +
moving overlay, 64x16/16x64/32x8/8x32 moving bars, testsrc2; cq 35..63;
cpu-used 1..4; min/max-partition-size 32/32, 32/64, 64/64):

* Every stream in which an intra 1:4 strip actually fires (counters 1..4 per
  stream, e.g. `32x8=4`, `64x16=2`, `16x64=3`, `8x32=4`) ALSO carries a
  non-skip INTER 2:1 rect strip ("a non-skip rectangular (HORZ/VERT/HORZ_B)
  strip needs rectangular residual coding"), an SB-level AB partition, or a
  sub-8 inter partition -- each another lane's named refusal, hit before the
  frame completes, so the attempt is never pixel-compared.
* Every recipe under which all attempts decode whole (`--max-partition-size=32`,
  or a static background with a moving overlay) fires ZERO 1:4 intra strips:
  aomenc only reaches for 1:4 partitions in the same neighbourhoods where it
  also picks the SB-level rect/AB shapes this decoder still refuses.
* Class [[counter-from-refused-stream]] applies: the counters above come from
  REFUSED streams and prove only that the new code path is entered, never that
  its pixels are right. The gate counts deltas on decoded+compared attempts
  only, which is why it currently fails/ignores rather than reading green.

So: **un-ignore and re-run this gate once the inter rect-leaf lane (lane-r14 r3,
`decode.rs` var-tx leaves) lands** -- that is the refusal that aborts every
candidate stream.

## Film check (his Hunger Games extract)

`decode_probe scratchpad/census3/kf/seg_4500.obu` (3840x1608 yuv420p10le,
213 frame headers):

* BEFORE (charter premise): stops at "an intra-coded 1:4 (or other non-2:1)
  rect strip on the inter block path".
* AFTER: `intra_rect4_in_inter: 64x16=0 16x64=4 32x8=0 8x32=0` -- four intra
  16x64 strips decode -- and the stop string is now
  "a split intra strip whose transform unit is 64x32 (no luma coefficient
  tables for that shape here)" (`decode_rect_split`'s luma-shape guard: the
  SB-level 2:1 strip at tx depth 0, i.e. a TX_64X32 luma transform, which no
  path here codes). Frames decoded (EC_AV1_FINAL_DUMP file count): 1 (the head
  key frame) -- the new stop is still inside decode-order frame 1.

EVIDENCE: /tmp/claude-1000/.../scratchpad/census3/kf/seg_4500.obu | decode_probe before/after the change, EC_AV1_FINAL_DUMP file count | 4 intra 16x64 strips decoded; stop string moved from the 1:4 refusal to "transform unit is 64x32"; 1 frame dumped
EVIDENCE: $HOME/.cache/intra14-suite-r1.log | `cargo test -p ec-av1 --lib` under a systemd unit | see totals below

## Residue

* fix-now (next round / successor lane): the gate is `#[ignore]`d. deferred --
  what unblocks it: the inter rect-strip refusal ("a non-skip rectangular
  (HORZ/VERT/HORZ_B) strip needs rectangular residual coding"), lane-r14 r3.
* deferred: pixel-byte compare of the film's pre-stop frames through
  EC_PROBE_OUT16 -- only ONE frame decodes before the stop, and it is the key
  frame already proven bit-exact by lane-hgkf; what unblocks a meaningful
  compare is the 64x32 luma transform above.
* accepted: `--enable-cfl-intra=0` and `--enable-palette=0` in the new gate
  (ledger dead-end: a new gate spelling `--enable-cfl-intra=1` retires the tool
  from `NEVER_EXERCISED_8BIT` on the flag alone with no CfL counter to assert;
  the screen-content frame is refused whole on this path, so a palette flag
  would buy no coverage). Deviation from the charter's flag list, stated here.
* accepted: `--enable-tx-size-search=0` and `--min-partition-size=32` in the
  gate recipe -- measured, both named in the source comments.
* open (NOT mine, found here): at 192x128 mandelbrot start_scale 4.76, cq 55,
  cpu-used 1, WITHOUT any rect/1:4 partition (`--enable-rect-partitions=0`),
  decode-order frame 4 mismatches ffmpeg by 3729 luma samples (max |d| = 6) and
  frames 5-7 drift (24k samples, max |d| ~220). Pre-existing square-path defect
  on this tree, unrelated to this lane.
