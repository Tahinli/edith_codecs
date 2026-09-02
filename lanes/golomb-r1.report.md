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
