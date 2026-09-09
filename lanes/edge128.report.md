# lane-edge128 — the 8-sample diff at the 128 superblock's edge

The charter's premise was that reader and writer shared a wrong assumption
about something read per 64x64 unit under a 128 block (deblock levels, tx
sizes, skip, `delta_lf`, LR). **They do not.** The bitstream was always
right: on the exact stream that fails, ffmpeg's decode and this crate's
`decode_stream` are byte-identical over all 12 frames and all three planes.
What disagreed with both of them is the ENCODER's own reconstruction — the
picture the next frame predicts from — and the stage is CDEF, on the
encoder's side of it alone.

## 1. Reproduced

`bd_rate_screen_native` with `EC_AV1_B128RES=1 EC_AV1_NATIVE_FILM4K=1`:

    B128R MISMATCH bars 2160p q=90 frame 11 plane Y: 8 samples,
      rows 126..127, cols 1611..1616
    panicked: sample 243531: ffmpeg decoded 106, the encoder reconstructed 107

The gate's own q=90 stream (dumped from the failing run) is **byte-identical**
to the one `enc_probe` writes for that row (411920 bytes, `fnv1a
d36717e86823f28f`), so the defect reproduces outside the gate. Decoding that
stream two ways:

| pair | differing samples, frame 11 |
|---|---|
| ffmpeg vs `decode_stream` | **0** (and 0 over the whole 12-frame sequence) |
| encoder reconstruction vs either decoder | **710** — Y 8, U 9, V 693 |

The luma 8 the gate reports are the visible corner of a 710-sample block:
V rows 0..63 cols 801..815 and U rows 62..63 cols 802..809 in chroma
samples, i.e. **luma columns 1600..1663 of the 128x128 superblock at columns
1536..1663** — the RIGHT half of one 128 superblock, its whole height. Not a
band along an edge: two whole 64x64 CDEF units.

## 2. The first divergent stage, and the rule

There is no divergent DECODE stage — the pre-filter, post-deblock and
post-CDEF planes are moot when both decoders agree byte for byte with each
other. The divergence is between the stream and the encoder's
`Encoded::reconstruction`, which is not a decode of the final tile at all:
`pick_and_apply_filters` gets it from `crate::decode::replay_final`, a
replay of deblock+CDEF over the captured pre-filter planes driven by the
CDEF search's own per-64x64 `idx_grid`.

**The rule the encoder broke** — `read_cdef`, spec 5.11.56:

    cdefSize4 = Num_4x4_Blocks_Wide[BLOCK_64X64]
    r = MiRow & ~(cdefSize4-1); c = MiCol & ~(cdefSize4-1)
    if (cdef_idx[r][c] == -1) {
      cdef_idx[r][c] = L(cdef_bits)
      for (i = r; i < r + h4; i += cdefSize4)
        for (j = c; j < c + w4; j += cdefSize4)
          cdef_idx[i][j] = cdef_idx[r][c]      // every unit the BLOCK spans
    }

One literal per block, copied over every 64x64 unit that block spans. A
whole 128x128 root block therefore puts **one** `cdef_idx` in the stream for
all **four** of its units — which is exactly what `tile::write_cdef_idx`
writes (one literal per superblock, at the first non-skip block) and exactly
what `decode::maybe_read_cdef_idx` reads back since lane-b128r stamped the
block's mi span. Both are right.

`filter_search::pick_filters` priced a preset per 64x64 unit and handed all
four to the replay. Only the origin unit's index reached the tile, so every
decoder filtered the other three with the origin's strengths while the
encoder's reconstruction carried three other strength pairs. Frame 11 is
where the residual 128 root and a per-unit `cdef_idx` list finally met.

**Class: `plan finer than its syntax`** — an encoder-side per-unit plan for
a parameter the bitstream can only carry once per block. It is invisible to
every stream-level gate (the stream is legal and decodes identically
everywhere); it surfaces only where the encoder's reconstruction is compared
against a decoder's, and then as a reference-chain error, not a syntax one.

## 3. The fix (one shared map, both consumers)

`tile::cdef_unit_owner(blocks, mi_cols, mi_rows)` maps each 64x64 unit to
the unit whose literal covers it — identity for every block of one unit or
less, and the root's top-left unit for the four units of a `Whole128` root.
`pick_filters` takes it and decides per GROUP: a covered unit's per-unit SSE
is summed into its owner's before the preset histogram and the `cdef_bits`
cost are read off it, and after the grid is built each covered unit takes
its owner's index. The identity map (`&[]`, the key-frame path, which has no
128 root) is the behaviour this function always had.

The map is built where the coded blocks are (`encode_inter_frame`) and
travels through `pick_and_apply_filters` to the search, so the writer's
`recode` and the encoder's filter replay read the same indices a decoder
does.

## 4. Same-shape sweep — every per-64x64-unit plan under a 128 block

Three writer-side plans are keyed per 64x64 unit; the sweep is all three:

| plan | state |
|---|---|
| `cdef_idx` (`arm_cdef_idx`) | **the defect, fixed here** |
| `delta_q` (`arm_delta_q`) | already correct: `write_delta_q` gates on the 128-wide superblock (lane-b128m) AND `encode_inter_frame` collapses `sb_q` to the root's top-left cell before the search quantizes with it — the same shape, closed one lane earlier |
| loop restoration (`arm_lr`) | not per-64 at all: `lr_unit()` is 128 under `EC_AV1_SB128`, `write_lr` writes it once at the root, and `pick_restoration` builds its grid from the same `lr_unit()` |

The reads the charter suspected — deblock filter levels, the tx-size lookup
for a TX_64X64 unit inside a 128 block, `delta_lf`, the per-unit CDEF skip
decision — are all in the DECODER, which the encoder runs verbatim as its
own filter stage, so reader and writer cannot disagree there by
construction; and ffmpeg agreeing with `decode_stream` sample for sample on
this exact stream clears them empirically as well.

## 5. Witness

`encode::tests::a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact`
(`--ignored`, 128 superblocks + the 128 root + its residual arm forced),
`testsrc2` 1280x768, 12 frames, q=90:

    128x128 blocks WITH a residual: writer coded 1440;
      64x64 CDEF units taking another unit's literal: 168
    test ... ok

Both readers, every frame, every plane: `decode_stream` and ffmpeg against
the encoder's own reconstruction. `take_cdef_covered_units` is the gate on
the fixture (class `gate-blind-to-feature`): a clip whose CDEF spends one
strength pair codes no literal and would witness nothing.

**RED before the fix** (collapse disabled on this same tree, everything else
identical):

    assertion `left == right` failed: frame 1: U -- 521 samples differ

## 6. The residual arm, re-measured with the fix

`bd_rate_screen_native`, 12 frames, `EC_AV1_B128RES=1` (control = the
standing table, i.e. the arm off):

| row | control | + the residual arm, fixed |
|---|---|---|
| bars 2160p | +8.6 / −13.7 | +8.6 / −13.7 |
| film B (2160p) | +23.6 / −3.1 | +23.4 / −3.2 |

This is the first COMPLETE measurement of the arm — before the fix the row
panicked instead of finishing. It stays **behind `EC_AV1_B128RES=1`**: the
arm is still inert on content (0–2 blocks per frame against ~90 whole 128
roots) and the two rows are inside the flat band. What the lane buys is not
the arm; it is that the encoder's reconstruction is no longer a filter apart
from what a decoder produces the moment a 128 root carries a residual.

**The default encoder is byte-identical**: both pins hold at default and at
`EC_AV1_SPEED=6`. They must — a default 128 root is always skipped, CDEF
does not filter a unit whose blocks are all skip, and the fold it now goes
through adds candidate-independent constants that cannot move an argmin.

## 7. Suite / checks

    s1  --skip stream::        344 passed; 0 failed; 37 ignored  RC=0
    s2  stream:: --skip 10bit  202 passed; 0 failed; 15 ignored  RC=0
    s3  10bit                   42 passed; 0 failed;  1 ignored  RC=0
    --ignored every_speed_preset_decodes_sample_exact_through_both_decoders  ok
    --ignored a_128_root_residual_block_under_a_per_unit_cdef_list...  ok (1440 blocks, 168 covered units)
    --ignored a_128_superblock_clip_whose_root_search...      ok
    --ignored a_128_root_block_with_a_real_residual...        ok
    --ignored a_128_root_compound_block...                    ok
    the_encoders_own_streams_are_byte_identical_to_their_pins ok, and ok at EC_AV1_SPEED=6
    cargo check --workspace --all-targets -j4  0 errors, 0 ec-av1 warnings

## Pins

NOT re-taken: 8364 / 33257 stand, at default and at `EC_AV1_SPEED=6`. The
fix cannot reach a default stream (see §6).
