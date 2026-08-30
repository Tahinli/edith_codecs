# lane-hbd r5 report

Branch `lane-hbd`, worktree `edith_codecs-hbd`. All 5 charter steps attempted,
in order, each its own commit. `cargo check --workspace --all-targets` is
clean. Landing red on this branch (the 10-bit gate fails) per charter
permission -- not merged, not asked to be.

## Commits this round
1. `3478044` feat(av1): wire decode::set_bit_depth from stream.rs per frame
2. `147a2ea` feat(av1): narrow bit_depth != 8 refusals for film_grain and superres
3. `0ce7fef` feat(av1): 10-bit gate + lift the blanket bit_depth refusal

## Step 1 -- wire set_bit_depth (DONE)
`stream.rs` now reads `seq.color_config.bit_depth` per frame (defaulting to 8
with no sequence header yet) and calls `decode::set_bit_depth`, right next to
the existing `set_enable_edge_filter` call, for the identical
cross-sequence-on-one-thread reason. `BIT_DEPTH` is no longer dead code.

## Step 2 -- narrow refusals (DONE)
Two new named refusals in `stream.rs`, both guarding the exact unported code:
- `film_grain.rs`'s grain LUT/blend (`[i32; 256]`, clamps at 255) -- checked at
  both `apply_grain` call sites (`show_existing_frame` and the normal
  `show_frame` path).
- `superres.rs`'s `upscale_row` (`&[u8]`, clamps at 255) -- checked before the
  `use_superres` key-frame branch.

Both declared in `refusal_inventory.rs`'s `REFUSALS` list;
`the_decode_path_refuses_exactly_the_listed_cases` passes.

## Step 3 -- the 10-bit gate (DONE, gate is RED)
`ffmpeg_decode_sequence_10bit` (stream.rs, next to the existing 8-bit
`ffmpeg_decode_sequence`, which is untouched) parses `yuv420p10le` rawvideo as
u16 LE. `a_real_aomenc_10bit_stream_decodes_pixel_exact` drives a real
`aomenc --input-bit-depth=10 --bit-depth=10 --threads=1 --row-mt=0
--sb-size=64` stream (gradients content, ffmpeg y4m into aomenc's stdin, same
recipe as the existing palette-Y gate), parses the produced stream's own
sequence header and hard-asserts `bit_depth == 10` *before* trusting any
pixel comparison, then compares `decode_stream`'s output against
`ffmpeg_decode_sequence_10bit` on the identical bytes.

The gate runs for real -- aomenc wrote a genuine 10-bit sequence header, not a
SKIP -- and fails on the pixel comparison: this decoder's luma comes out
~82-117, ffmpeg's own 10-bit decode of the same bytes is ~331-471 for the same
gradient content, roughly a factor of 4 apart (2^(10-8) squared, or two
missed left-shifts by `bit_depth - 8`). This is NOT the "small rounding
delta" the charter expected from unclamped intermediate math -- it is a
systematic magnitude defect, most likely a fixed `<< (bit_depth - 8)` /
`>> (bit_depth - 8)` this decoder never applies anywhere (DC pred's initial
value, a reconstruction clamp, or dequant), still written as if every stream
were 8-bit. Root-cause bisection (which stage first drops the scale) is next
round's first job -- I did not spend remaining budget chasing it live.

## Step 4 -- lift the blanket refusal (DONE, same commit as the gate)
The old `stream.rs` blanket `"a stream whose bit depth is not 8..."` check is
gone; `refusal_inventory.rs` updated to match (still green). The narrow
film_grain/superres refusals from step 2 are the only bit-depth gates left.

## Step 5 -- his own films (DONE)
Extracted 3s of each into this worktree's `fixtures/hbd-r5/` (outside the
repo via the `fixtures` symlink, so durable):
- `ffmpeg -i ".../Hunger Games...HDR10....mkv" -t 3 -c:v copy -an -f obu hunger.obu`
- `ffmpeg -i ".../Troy.Director's.Cut...mkv" -t 3 -c:v copy -an -f obu troy.obu`

Probed with `cargo run -p ec-av1 --example decode_probe -- <file>`:

- **Hunger Games** (Ballad of Songbirds and Snakes, 2160p HDR10):
  `REFUSED: unsupported: AV1 tile (a partition below 8x8 (this decoder codes
  no leaf smaller than 8x8))`
- **Troy** (Director's Cut, 1080p):
  `REFUSED: unsupported: AV1 tile (a 32x32 partition type this decoder does
  not code (value=4))`

Neither refusal is the bit-depth wall anymore -- lifting the blanket refusal
is real, verified progress: both real 10-bit films now reach genuine
partition-coverage gaps instead of being stopped before decode even starts.
**These two refusal strings, verbatim, are the next lane's charter starting
point** -- sub-8x8 leaf partitions (Hunger Games) and 32x32 partition
type=4 (Troy, likely `PARTITION_VERT_4` or similar AB/1-to-4 type this
decoder's part32 match arm doesn't cover), on top of fixing the 10-bit scale
defect found in step 3.

## What's owed next round
1. Root-cause and fix the ~4x luma scale defect the 10-bit gate found
   (bisect against the oracle per-stage, not by inspection, per the
   charter's own warning).
2. Sub-8x8 partition leaves (Hunger Games' real gap).
3. 32x32 partition value=4 (Troy's real gap) -- check against `decode.rs`'s
   part32 match arm coverage.

## Deferred
- deferred: 10-bit gate root-cause fix — turn budget (charter's step-3/4/5
  ordering + report-writing left no room this round) — next lane's first job.
