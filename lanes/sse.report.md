# lane-sse — the distortion metric scores the inside of the frame only

## What was wrong

`Plane::block_sse` — the RD distortion every mode, transform, CfL alpha and
partition trial in `encode.rs` is ranked by — summed the squared error over
the WHOLE block, including the part past the true frame edge. Those samples
are the encoder's own edge-replicated padding: the decoder reconstructs them
and the display crop throws them away. lane-b64b fixed the 64x64 root alone
(`inside_sse`/`clip_sb64_sse`); every other size, shape and tool still paid.

The clip now lives in `block_sse` itself, so all callers are fixed once
(libaom does the same in `av1_dist_block`, over `max_blocks_wide/high`).
`inside_sse`/`clip_sb64_sse` and the `edge` flag at the 64 root are deleted —
they became no-ops. `ibc_sse` (intrabc's whole-block distortion) clips too.

Knobs: `EC_AV1_EDGESSE=0` restores the padding-inclusive score (the before
arm); `EC_AV1_EDGE_CENSUS=1` prints the straddling-block census;
`EC_AV1_NATIVE_CROP=<w>x<h>` puts the native BD gate on a window that is NOT
superblock-aligned (`probe::gate_crop` rounds down to whole 128-wide
superblocks by design, so no default gate row straddles at all).

## Instrument — straddling blocks and their padding share (12 frames, 4 q points)

`EC_AV1_EDGE_CENSUS=1`, one line per block size that fired. "scores" counts
every `block_sse` call on a block the edge cuts, trials included.

CHARTER CORRECTION: film A is coded **1920x792**, not 1080p, so the straddling
film-A arm is its own full frame (1920x792 = 6.19 superblock rows), not a
1920x1080 crop (`EC_AV1_NATIVE_CROP=1920x1080` asserts "does not fit").
A 1920x1080 arm was still run on the 1080p bars fixture.

| arm | size | 8x8 | 16x16 | 32x32 | 64x64 |
|---|---|---|---|---|---|
| bars 1080p | 1920x1080 | 960 scores, 100% pad | 3911, 25.0% | 22030, 12.5% | 8831, 12.5% |
| bars 1080p | 1920x792 | 960, 100% | 51780, 24.4% | 33197, 20.7% | — |
| film A | 1920x792 | 960, 100% | 75698, 25.1% | 44740, 21.6% | — |
| bars 2160p | 3840x1608 | 5760, 100% | 10080, 70.9% | 8838, 41.5% | — |
| film B | 3840x1608 | 5760, 100% | 11483, 70.4% | 9276, 57.2% | — |

Read: on his real 4K film, **57% of the raw squared error of every straddling
32x32 block sat in padding**, and 70% of every straddling 16x16 block's. No
64x64 line at 792/1608 because those heights leave under half a superblock
inside, so `has_rows` splits the root before it is ever scored.

## Before/after — native BD gate, 12 frames, gop=12, 4 q points

`EC_AV1_EDGESSE=0` vs default, one release binary, same crop.

| arm | row | BD vs libaom before -> after | BD vs rav1e before -> after |
|---|---|---|---|
| 1920x792 | bars 1080p | +14.0% -> +14.0% | +1.5% -> **+1.4%** |
| 1920x792 | film A | +36.2% -> **+36.1%** | +7.5% -> **+7.4%** |
| 3840x1608 | bars 2160p | +14.7% -> +14.7% (byte-identical) | -6.9% -> -6.9% |
| 3840x1608 | film B | +35.8% -> +35.8% (byte-identical) | +17.6% -> +17.6% |
| 1920x1080 | bars 1080p | +8.7% -> +8.7% (byte-identical) | -4.1% -> -4.1% |

Keep rule met: **no row worse anywhere**; both film-A columns better by 0.1.
The charter expected >=0.3 on the straddling arm — it is 0.1. Honest reading:
the padding is edge-replicated, so every candidate predicts it almost equally
well and the metric change rarely flips a winner. The 4K rows code
byte-identical streams at all four points even though their census DOES differ
between the arms (10086 vs 10080 straddling 16x16 scores) — the candidate
pruning walks a slightly different path and lands on the same decisions.

SB-ALIGNED ROWS: byte-identical BY CONSTRUCTION, not just by measurement —
on a crop whose width and height are multiples of the superblock,
`inside_rows == side` for every block, so `block_sse` takes exactly the old
path. The 1920x1080 arm above is the empirical confirmation on a straddling
crop that still codes the same stream.

## Invariants, pins, suite

- `EC_COMP_MISMATCH=1` over pins + facade + thread-count + both straddling
  witnesses: 6 passed, 0 failed, no mismatch line printed.
- pins `the_encoders_own_streams_are_byte_identical_to_their_pins`: 9808 /
  35791 HOLD (the 640x384 pin clip is 10x6 whole superblocks, so no block of
  it straddles and the change cannot reach it).
- `tile_bytes_do_not_depend_on_the_thread_count --include-ignored`: pass.
- `the_facade_codes_the_same_bytes_as_encode_sequence`: pass.
- three-way exact witnesses: lane-b64b's `232x168` still passes, and the new
  `a_1080p_shaped_clip_straddles_at_every_block_size_and_decodes_sample_exact`
  (384x216 = the 1080p shape at a fifth: 3 whole SB columns, 1.6875 SB rows,
  straddling at 64/32/16) decodes sample-exact against our decoder AND ffmpeg.
- metric unit check `a_block_the_frame_edge_cuts_is_scored_over_its_inside_only`.
- `cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1 warnings
  (the 25 remaining are pre-existing ec-opus/ec-vorbis doc warnings).

## Not done

- `Plane::block_sad` (the MOTION SEARCH's distortion) still scores the whole
  block. Deliberate: libaom's motion search runs full-block SAD on its padded
  source too, and this lane is the RD distortion. Flagged, not fixed.
