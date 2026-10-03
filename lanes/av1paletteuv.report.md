# lane/av1paletteuv — `tile.rs::palette_uv_side`

Charter: take ONLY the first of the three unprobed hardcoded-chroma sites named in
`lanes/av1subsizesweep.report.md`. The mu-chroma square sites and the floor-`>>`
plane allocators stay named, not edited.

## Verdict

**ALREADY CORRECT, with the measurement — and unreachable for any other chroma
shape.** The `(side / 2).max(4)` form is libaom's 4:2:0 plane-1 block exactly,
and no reachable path can hand the writer a 4:2:2 or 4:4:4 frame. Gated
(`encode::tests::the_chroma_palette_map_side_is_the_420_plane_block_and_nothing_else_reaches_it`),
five mutations measured biting. No production code changed.

## 1. What the site computes, and what it should compute at each shape

`crates/ec-av1/src/tile.rs:5004-5006`

```rust
pub(crate) fn palette_uv_side(side: usize) -> usize {
    (side / 2).max(4)
}
```

Callers (all writer-side; the decoder has no counterpart call):

| site | role |
|---|---|
| `tile.rs:4979` (`write_palette_syntax`) | sizes the chroma colour-index map actually written |
| `tile.rs:4557` (`palette_uv_bits`) | prices that same map in the mode search |
| `encode.rs:7017` (`search_chroma`) | `debug_assert_eq!(palette_uv_side(side), at.side)` |

What libaom does — `av1_get_block_dimensions(bsize, plane, xd, ...)`
(`av1/common/blockd.h:1512`), which `av1_decode_palette_tokens`
(`av1/decoder/detokenize.c:88-98`) hands to `decode_color_map_tokens` as
`plane_width`/`plane_height`:

```c
const int plane_block_width  = block_width  >> pd->subsampling_x;
const int plane_block_height = block_height >> pd->subsampling_y;
const int is_chroma_sub8_x = plane > 0 && plane_block_width  < 4;
const int is_chroma_sub8_y = plane > 0 && plane_block_height < 4;
*width  = plane_block_width  + 2 * is_chroma_sub8_x;
*height = plane_block_height + 2 * is_chroma_sub8_y;
```

So the correct map extent is **per-axis**: `(block_w >> ss_x, block_h >> ss_y)`,
each then bumped by 2 if it came out under 4. For a square block the two axes
agree only when `ss_x == ss_y`:

| shape | `ss` | correct map | `palette_uv_side` | verdict |
|---|---|---|---|---|
| 4:2:0 | (1,1) | `(side>>1, side>>1)` | `(side/2).max(4)` | **exact** |
| 4:2:2 | (1,0) | `(side>>1, side)` — NOT square | square | wrong by half the rows |
| 4:4:4 | (0,0) | `(side, side)` — NOT `side/2` | square at half size | wrong on both axes |
| 4:4:0 | (2,2) | uncodable (libaom forbids the shape outright) | — | n/a |

Note `.max(4)` is a stand-in for libaom's `+2 * is_chroma_sub8` bump and it is
**unreachable in the writer**: `palette_bsize_ctx_wh` refuses `bw*bh < 64`, so
the smallest admitted square side is 8 and `8>>1 == 4` already. Dropping the
floor therefore changes nothing today; the gate says so explicitly rather than
letting a green floor-mutation read as coverage.

The decoder's counterpart is already per-axis and is NOT this function:
`decode.rs:20914-20933` reads the map at `(side >> ss_x(fctx), side >> ss_y(fctx))`.
That is why 4:2:2/4:4:4 decode is byte-exact today (measurement below) while the
writer is not — the site under review is the only square one left.

## 2. Reachability measurement — how many encodes reach it, and with what shape

**Writer-side chroma shape census (non-test source).** Every `subsampling_*`
literal the encoder names outside its test modules:

- `encode.rs` (`unspecified_color_config`): `{(1, 1)}`
- `encoder.rs` (`Colour::color_config`, all three `Colour` variants): `{(1, 1)}`

`encode_key_frame_inner` — the single funnel every tile-producing entry passes
through — has **5 non-test call sites** (`encode.rs:9112, 9154, 15811`;
`encoder.rs:1825, 2001`) and every one passes one of those two 4:2:0
constructors. Counted, not described.

**The one public door for another shape.** `key_frame_headers_colour`
(`encode.rs:2256`) is the only public entry taking a caller-chosen
`ColorConfig` — it is how a 4:2:2 or 4:4:4 sequence header is written at all.
Its body builds HEADERS ONLY: no tile writer is reachable from it (measured by
scanning its body for `write_palette_syntax`, `write_color_index_map`,
`encode_key_frame_inner`, `tile::`).

**The picture door.** `Picture::check` / `check_even` require
`u.len() == v.len() == width*height/4`. Measured by CALL, not by reading: a
4:2:2-shaped (`w*h/2`) and a 4:4:4-shaped (`w*h`) picture are both refused.

**Encode count that actually fires the site** (so the above is not a source
scan over dead code): the gate's own `screen_card(192,96)` key frame codes
**9 chroma-palette blocks** (sizes `[2, 0, 7, 0, 0, 0, 0]`), all at 4:2:0.
The sibling end-to-end gate measures **9 chroma palette blocks** in its key
frame and **48** across its 4-frame GOP, all byte-exact through ffmpeg.

**Committed fixtures / real aomenc streams at 4:2:2 and 4:4:4**: measured, and
they exercise the DECODER's per-axis path, never this writer site — they are
decode-direction fixtures. Counts, both byte-exact against aomdec:

- `fixtures/422_palette_intra_in_inter_384x240_17f.obu` — 17 decode-order
  frames, **16 chroma palette unit windows**
- `fixtures/444_lossy_palette_chroma_352x242_10b.obu` — 17 decode-order frames,
  **4 windowed palette chroma units**

Conclusion: a real 4:2:2/4:4:4 palette stream reaches `decode.rs`'s per-axis
read (and passes), and cannot reach `palette_uv_side` at all, because the only
producer of tiles in this crate is 4:2:0.

## 3. The gate

`crates/ec-av1/src/encode.rs`, test
`the_chroma_palette_map_side_is_the_420_plane_block_and_nothing_else_reaches_it`.
Three arms:

1. **The value** — transcribes libaom's per-axis formula and asserts
   `palette_uv_side(side)` equals it for every side in
   `palette_bsize_ctx`'s square domain (every integer 8..=64, 57 values, not
   just the powers of two). Also asserts the premise as arithmetic: for sides
   8/16/32/64 the square is NOT the 4:2:2 map and NOT the 4:4:4 map, so
   "correct" cannot be misread as "correct at any shape". Then states why
   `.max(4)` cannot fire in the admitted domain.
2. **Nothing can hand the writer another shape** — (a) the `subsampling_*`
   census over both encoder files' non-test prefixes, (b) the
   `key_frame_headers_colour` body scan, (c) the 5 non-test
   `encode_key_frame_inner` call-site count, (d) `Picture::check` refusing
   4:2:2- and 4:4:4-shaped pictures by call.
3. **The site really fires** — encodes `screen_card(192,96)`, decodes it back,
   asserts the decoder's thread-local chroma-palette count moved and the
   round-tripped U/V match the encoder's own reconstruction. This also proves
   the reader took the map at the extent the WRITER sized, which is what makes
   the square value right rather than merely unchallenged.

Counter hygiene: arm 3 deliberately reads the DECODER's thread-local
`decode::palette_uv_hits()`, not the writer's process-global
`tile::take_palette_uv_hits()` (a swap-to-zero). The first draft used the
global one and was flaky under `cargo test palette` — a parallel sibling's
encode stole the count and it read `[0; 9]`, passing alone. Measured, then
fixed; the comment in the test names it.

## 4. Mutation proofs (gate bites)

| # | mutation | result |
|---|---|---|
| 1 | `palette_uv_side` → `side` (halving dropped) | RED at `side 8 -- palette_uv_side disagrees with libaom's plane-1 block` |
| 2 | → `(side / 2).max(8)` (floor raised) | RED at `side 8`, same arm |
| 3 | → `4` (map size pinned) | RED at `side 10`, same arm |
| 4 | `encoder.rs` `Colour::color_config` → `subsampling (0,0)` | RED: `the non-test encoder.rs names chroma shapes {(0, 0)}` (arm 2a) |
| 5 | one `crate::tile::` line inside `key_frame_headers_colour`'s body | RED: `key_frame_headers_colour now reaches tile::` (arm 2b) |

Each mutation was reverted immediately; the tree carries only the new test
(`git diff --stat`: 1 file, +234, all inside the test module).

**Known non-mutation**: dropping `.max(4)` entirely leaves the gate GREEN —
correctly, since the floor is unreachable in the admitted domain (§1). Recorded
in the test body so it is not mistaken for an untested path.

## 5. Suite runs

- `cargo test -p ec-av1 --lib palette` — **26 passed, 0 failed** (parallel,
  includes the new gate). Before the arm-3 counter fix this same run was red on
  `a_screen_content_picture_codes_palette_blocks_ffmpeg_decodes_exactly`
  (counter theft, described in §3).
- `cargo test -p ec-av1 --lib screen` — **19 passed, 0 failed, 4 ignored**.
- The earlier all-palette red under `EDQUOT` was environmental (`os error 122`,
  this box's `/tmp` tmpfs at its user quota); re-run with
  `TMPDIR=$HOME/.cache/tmp CARGO_TARGET_DIR=$HOME/tgt-paletteuv RUSTC_WRAPPER=`
  it is green, per `skill://local-edquot-rust-build-tests`.
- Full `--lib` suite: NOT completed locally — it exceeded a 3000 s timeout with
  `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact`
  still running (no failure reported in the visible tail). Per the project rule
  that full cargo suites run on the VPS fleet and not locally, this lane's
  verification is the scoped runs above; a full-suite run is a VPS job.

## 6. Still open (named, NOT edited — out of charter)

1. **mu-chroma sites using `chroma_side * chroma_side`** — the square
   chroma-block assumption at sites other than this one. `palette_uv_side` is
   the palette-map site; the mu sites are a separate family, still unprobed.
2. **Floor-`>>` chroma plane allocators** — allocators that assume a
   frame-edge shape via a floor shift. Also unprobed, also untouched.
3. **The gate's own forward obligation.** If an encoder colour config that is
   not 4:2:0 ever appears, arm 2a/2b red. The FIX then is not a `palette_uv_side`
   tweak: `write_color_index_map` and `palette_uv_bits` both take a single
   `side`, so the map writer has to become per-axis (`bw`, `bh`) the way
   `decode_color_index_map_wh` already is, before any non-4:2:0 palette can be
   written at all. That is a bigger change than this charter, and it is the
   named unblock.

## 7. Environment notes

- Worktree `/home/tahinli/wt-av1paletteuv` on `lane/av1paletteuv` (an earlier
  `/tmp` worktree was moved off the tmpfs, which was at its user quota).
- Own target dir `$HOME/tgt-paletteuv`; no pushes, no merges, primary untouched.