# lane-svt1 — one SVT-AV1 screen stream, one palette block, 10122 samples

Stream: `libsvtav1 -preset 8 -crf 35 -svtav1-params lp=1 -g 12 -f obu`, screen
capture crop, **1920x1024** (the charter said 1920x768; `ffprobe` says 1024 and
the reference yuv is 12 x 2949120 bytes, i.e. 1920x1024 4:2:0), 12 frames, kept
outside the repo at `$HOME/.cache/svt1/svt-p8-crf35-screen.obu`.

## The premise was wrong, in our favour

The charter's "ONE sample off, frame 0 Y 1471489" is a mis-read. Diffing
`ours.f*.yuv` (u16 LE) against ffmpeg's 8-bit `ref.yuv`:

| frame | differing samples | plane | rows | cols |
|---|---|---|---|---|
| 0 | 812 | Y only | 766..833 | 766..786 |
| 1..6 | 825 each | Y only | 763..834 | 766..786 |
| 7..11 | 872 each | Y only | 763..834 | 765..786 |

**10122 samples total, luma only, chroma exact in every frame.** Sample 1471489
is (row 766, col 769) at width 1920, not (766, 1729). Max |delta| 144.

## First divergent stage: pre-filter reconstruction

Bisected with the decoder's own debug rungs, not with the stage dump:
`EC_AV1_DEBUG_SKIP_DEBLOCK=1`, `EC_AV1_DEBUG_SKIP_CDEF=1` and a new
`EC_AV1_DEBUG_SKIP_LR=1` (added beside its two siblings, and to
`pipe_filters_ok`'s stage-flag list). With every filter off the band is still
wrong and still flat, so no post-filter creates it — it is reconstruction.

`EC_TRACE_MODE_STEP=1` through both decoders then diffs **clean**: all 49080
mode-ladder lines of frame 0 (`skip`, `cdef`, `dq`, `mode`, `angle_y`,
`uv_mode`, `angle_uv`, `tx_depth`) agree with the instrumented aomdec value for
value AND range for range. Right symbols, wrong pixels.

The block: `EC_PALSYN=1` says `mi=192,192 bsize=6 mode=0 uv=0 ctx=1 y=8` — a
16x16 **palette** block (8 colours) at pixel (768, 768), `skip=1`,
`tx_depth=1`. libaom prints no `EC_PRED`/`EC_PREDOUT8` for its luma (palette
does not predict off edges); it does print the chroma DC. Ours filled the whole
16x16 with 54 = the DC of its top edge, and the six 8x8 `V_PRED skip` blocks
below it (mi rows 196..206, col 192) then propagated that flat value down the
next 48 rows, which is the 64-row tall band in the table above.

## Root cause

`crates/ec-av1/src/decode.rs`, `decode_block` (square intra):

```rust
if let Some(buf) = &palette_y_buf && logical_tx == side { set_palette_pred(...) }
```

A **skipped** block predicts the whole `side`x`side` in ONE call — the skip
branch's per-unit walk (`split_tx_skip`) explicitly excludes palette/intrabc
blocks — so gating the arm on `logical_tx == side` left every skipped palette
(or intrabc) block whose `tx_depth` split the transform with an empty override
slot and an edge prediction. Fix: `&& (logical_tx == side || skip)`. Neither
prediction is edge-derived, so the whole-block buffer at the block's own stride
is exactly what that single call consumes.

Class: **override-slot-on-one-arm** (the memory entry of that name is the same
shape: an override installed on the skip arm only).

## Same-shape sweep

`grep -n 'logical_tx == side\|split_tx_skip'` — one site, now fixed. Every
sibling path already arms on both arms and was checked by reading:

* `decode_leaf8` (8x8, TX_4X4 split): arms inside the non-split `else`, and its
  `resolved == 4 && palette_y_buf.is_none()` guard is the same rule — correct.
* `decode_rect_split`: windows the map per transform unit including the skip
  arm — correct.
* `decode_block_rect`, `decode_block_rect64`: arm unconditionally before the
  skip branch — correct.
* the intra-in-inter var-tx walk (`vartx_leaves`): windows per unit, skip arm
  included — correct.

## Witness

`skip_split_tx_override_hits()` (new counter, incremented on the SHAPE, not on
the fixed arm, so it also counts pre-fix) fires **exactly once** in the whole
12-frame stream — that one block.

No lavfi recipe witnesses the defect in a stream this decoder otherwise
decodes. Measured: 6 sources x 5 sizes (384x152, 320x184, 256x190, 640x366,
320x240) x 6 crf (25..50), all `libsvtav1 -preset 8 -svtav1-params
lp=1:screen-content-mode=1`, 4 frames. 8 streams reach the fixed arm
(`hits=1`); of those, 2 are REFUSED by this decoder ("a Golomb tail longer than
this decoder reads") and 6 miss by ~200k-245k samples on unrelated inter-frame
defects. `smptebars,drawgrid` at 384x152 is the only family that reaches the
arm at all; `smptebars` alone, `pal75bars`, `testsrc`, `testsrc2` and
`color+drawgrid` never do.

So the gate rides the kept stream, per the charter's own fallback:
`stream::tests::an_svt_screen_palette_block_with_a_split_transform_decodes_exactly`,
gated on `EC_AV1_SVT1_STREAM` (SKIPs with a printed reason when unset, or when
ffmpeg is missing, or when the path does not read). It asserts the counter is
non-zero before comparing, so it cannot pass on a stream that never reaches the
path (class gate-blind-to-feature). The screen crop is NOT committed.

* RED (fix reverted, counter kept): `assertion left == right failed: frame 0
  luma vs ffmpeg`, `test result: FAILED` (`$HOME/.cache/svt1/redtest2.log`).
* GREEN: `test result: ok. 1 passed` (`$HOME/.cache/svt1/greentest.log`), and a
  full 12-frame `dump_yuv` vs ffmpeg diff reads **0 differing samples in every
  plane of every frame**.

## The stall finding

Not reproduced, and not tested: the bisect never needed the stage dump. Every
decode here ran under `systemd-run --user` and finished in seconds (12 frames
of 1920x1024: `dump_yuv` ~5 s, instrumented `aomdec --limit=1` ~10 s). The two
prior agents ran the dump command in the FOREGROUND; nothing in this lane
suggests the instrument itself hangs, but nothing here exercises it either —
`EC_AV1_PREFILT_DUMP`/`EC_AV1_POSTDEBLOCK_DUMP` remain untested by this lane.

## Open (not this lane's fix)

* `unsupported: AV1 tile (a Golomb tail longer than this decoder reads)` on 2
  of 8 synthetic SVT-AV1 screen streams (e.g.
  `smptebars=s=384x152:r=12,drawgrid=w=8:h=8:t=1:c=black`, preset 8, crf 35,
  `screen-content-mode=1`). Streams are under `$HOME/.cache/svt1/w3/`.
* ~200k-sample misses on the INTER frames of those same synthetic screen
  streams (their frame 0 is not the only wrong frame). A separate lane: this
  one only closes the key-frame palette block.

## Gates on 68c61d33

| gate | result |
|---|---|
| `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins` | ok, 1 passed (unchanged on this base) |
| s1 `--skip stream::` | ok, 335 passed, 0 failed, 32 ignored (571 s) |
| s2 `stream:: --skip 10bit` | ok, 200 passed, 0 failed, 15 ignored (752 s) |
| s3 `10bit` | ok, 42 passed, 0 failed, 1 ignored (60 s) |
| `cargo check --workspace --all-targets` | 0 errors, 0 ec-av1 warnings (21 pre-existing in ec-opus, 1 in ec-vorbis) |
| kept stream, 12 frames, all planes | 0 differing samples |
