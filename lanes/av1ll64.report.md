# lane-av1-ll64: the rect64-route 64x32 chroma row — named, not invented (r1)

## Ticket

Default-partition lossless 4:4:4 stops at a missing 64x32 chroma lossless
coefficient row on the rect64 route (the `EC_RECTCHROMA_GAP luma=64x32
tx=4x4 chroma=64x32` diagnostic `lane-av1-llinter2` deferred here). Name
the row; add it only if libaom has that transform; do not invent one.
`decode_rect4_16_strip` is off-limits (owned by the lossless-444 strip
lane).

## The named row

The `chroma` match in `decode_rect_split` (decode.rs:10086) has no
`(64, 32)`/`(32, 64)` arm: a 4:4:4 64-axis strip's chroma plane block
cannot be coded as ONE rect unit here. That missing arm is the row the
gap print names.

## libaom verdict (oracle source, measured): no such transform — row NOT added

- Lossless: `av1_get_tx_size` returns TX_4X4 for EVERY plane
  (`av1/common/blockd.h:1383`), so a 64x32 chroma plane block codes as a
  plane-major raster of TX_4X4 units (16x8 = 128 per plane at 4:4:4).
  There is no 64x32 lossless transform at all.
- Non-lossless: `av1_get_max_uv_txsize` routes chroma through
  `av1_get_adjusted_tx_size` (`blockd.h:1361-1367`), which collapses
  TX_64X32 and TX_32X64 to TX_32X32. A chroma transform wider/taller
  than 32 is never one coded unit.

Per ticket step 3: libaom marks the single-unit 64x32 chroma transform
not-codeable ⇒ STOP; no row invented.

## The route truth: nothing stops at HEAD — the print was a false gap

`decode_rect_split`'s arms already cover every shape that reaches it:

- LOSSLESS: the 4x4 walk (decode.rs `else if lossless(fctx)`) is
  shape-agnostic — for a (64,32) chroma block it walks 128 plane-major
  `TxbSet::Chroma4` units with `4 << ss` spans, which IS the libaom
  behavior above. The same code the key-frame gate proves on 16x8
  strips.
- Non-lossless: `chroma_tiled` tiles those four shapes as 32x32 /
  32x16 units (the 10-bit film gates' verified path).

The old diagnostic fired whenever `chroma` was None and `!m.skip` —
INCLUDING on those two working routes, printing a "GAP" for shapes the
function decodes correctly. That false print is what `lane-av1-llinter2`'s
report cited as a "named non-panic stop" of default-partition `ll444.obu`.

## Change (one diagnostic, no decode-path edit)

decode.rs only: the `EC_RECTCHROMA_GAP` print's condition is now the
refusal's exact precondition (`chroma.is_none() && !chroma_tiled` inside
the existing `!m.skip`, so print↔refusal can never diverge), and its
message states the libaom verdict (`libaom_lossless_tx=none`). The
inventory-pinned refusal wording is byte-identical; every coefficient
read, CDF, span and neighbour stamp untouched. `chroma_tiled`'s `let`
moved above its new first use; its explanatory comments stay with it.
4:2:0: zero-change (a 64x32 luma strip is 32x16 chroma there — a row
that always existed; the print never fired at 4:2:0).

## Verification (measured, this tree @ HEAD + this change)

- Encoder sweep (aomenc `--lossless=1 --profile=1`, palette/intrabc off,
  contents from testsrc2 to forced HORZ-biased noise, `--cpu-used` 0..8,
  partition caps, bottom-edge smooth bands): NO lossless fixture presents
  a 64x32 intra strip — at lossless RD the encoder picks NONE/SPLIT, so
  the print never fired on any stream, old or new.
- Probes post-change (decode_probe): `ll444.obu` (default partition, 6
  frames) OK 6/6; `force_kf.obu`, `h2.obu` OK; committed
  `lossless_sb128_rect_kf.obu` OK 4/4; pinned `ll444_minp64_inter.obu`
  OK 6/6; `EC_RECTCHROMA` count 0 on every stream.
- Gates (7 passed, 0 failed): `a_lossless_444_min_partition64_inter_
  stream_decodes_pixel_exact` (6-frame inter gate: pixel-exact through
  BOTH oracle aomdec --rawvideo and ffmpeg, counter non-vacuity),
  `a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` (run for
  real, not SKIP: pinned bytes `fixtures/ll444-lossless-key.obu` copied
  from the av1llband2 worktree, sha256 `9fc9ce1f5f6475a1d483f404b44acb9
  0f72a80be2e5c51dedf0aeb1da9a3f5dc`; fixtures/ is gitignored, nothing
  committed from it), both 4:2:0 lossless gates
  (`a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact`), both
  `a_lossless_sb128_*` gates, `a_lossless_16x4_chroma_pair_*`.
- `cargo check -p ec-av1 --all-targets` (target dir
  `$HOME/.cache/cargo-target-av1ll64`): 0 warnings, 0 errors.

## Same-class suspects, measured on scratch streams, deferred (spoken)

Repro recipe (scratch, regenerable): `testsrc2 128x96 yuv444p`, aomenc
`--profile=1 --lossless=1 --enable-palette=0 --enable-intrabc=0`,
`--max-partition-size=64`, 6 frames. Both defects verified PRE-EXISTING
at 2ac149e3 via `git stash` probe, so neither is caused by this change;
both are the inter sub8 lossless class (a lossless inter route coding a
rect TX unit where `av1_get_tx_size` forces TX_4X4 per plane — the
`decode_inter_block8`/84acbd09 defect class one route over):

- `decode_inter_sub8_rect2` → `read_inter_plane_rect` →
  `push_mc_rect_tx` → `TxParams::run` lossless WHT assert
  `(8, 4) != (4, 4)` at decode.rs:2566. deferred(fix-now in the lane
  owning the inter sub8 lossless rect route; the fix shape is the
  established per-plane TX_4X4 unit walk).
- The 12-frame sibling stream refuses mid-tile: `REFUSED: unsupported:
  AV1 tile (a Golomb tail longer than this decoder reads)` — same
  route class, downstream desync form. deferred(with the same lane;
  likely disappears when the (8,4) read is fixed).

`decode_rect4_16_strip`'s 4:2:0-hardcoded lossless chroma walk remains
with its owning lane (untouched here, per lane ownership).
