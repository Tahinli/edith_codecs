# lane-txd — the `tx_depth` context, both halves: BUILT, MEASURED, REFUTED

Class `pricer-context-zero`, last site. lane-ctx2 built the KEY-frame half
alone and it lost; the named cause was that an INTER frame's intra block reads
a different band set (`TXFM_CONTEXT`, `tile::tx_size_ctx_txfm`, where an inter
neighbour votes its own BLOCK size and a skipped inter block publishes its
block size) which the encoder published nowhere. This lane built BOTH halves
in one arm. It still loses, and the census below says why — so the arm is
reverted whole (`75158510`; the tree is byte-identical to the control at
`3c106765`) and the class closes as "no site left that pays".

## The arm (commits 60c3cdf6 + 9c1d45f9, reverted by 75158510)

* Key half: lane-ctx2's, restored verbatim — `above_tx`/`left_tx` bands in
  `encode_key_frame_inner` mirroring `Neighbours::record_tx`, the rule shared
  as `tile::tx_size_ctx_of`, the row threaded through `code_square`.
* Inter half (new): one `u8` per mi cell beside `MiGrid::cells` (`txfm`, like
  `skips` — the cell stays 16 bytes), written by `record_mi` at every one of
  its six publication points, with `txfm_px(side, inter, skip, tx_depth)` —
  a skipped inter block records its BLOCK size, everything else its resolved
  transform, which is what all four `record_txfm` calls in
  `tile::write_tx_syntax_inter` leave over the block's whole span.
  `encode::tx_ctx_txfm` reads the above/left cell out of the grid (`MiGrid::get`'s
  tile window IS the writer's `has_above`/`has_left`) and the rule itself moved
  into `tile::tx_size_ctx_txfm_of`, which `Neighbours::tx_size_ctx_txfm` now
  calls too — one transcription for writer and pricer.

## Gate (12-frame native, BD-rate vs libaom cpu-used 6 / rav1e speed 6)

Control reproduced in this worktree at `3c106765`, all three rows, exactly the
charter's numbers.

| row | control | arm (both halves) |
|---|---|---|
| film A | +21.0 / −4.8 | +20.9 / −4.8 |
| film B | +24.9 / −1.9 | **+25.2 / −1.6** |
| screen | +14.5 / −33.2 | +14.5 / −33.2 |
| bars 1080p | −2.0 / −17.6 | −2.0 / −17.6 |
| bars 2160p | +8.7 / −13.4 | +8.7 / −13.4 |

Film B moves the wrong way on BOTH columns by 0.3 while film A gains 0.1 on
one: not "both films improve", not "one ≥0.5 down and the other flat ±0.3",
not "a correctness fix flat within ±0.2 everywhere". No keep rule is met.
`bd_rate_film_long_gop` was not run — it only decides a row that ships.

## Census — why correct pricing costs

Instrumented at the two ctx-derivation sites over the pin encode (key + inter
pictures), plus the price each row charges:

```
CENSUS key:         ctx0 767  ctx1 457  ctx2 156   (total 1380, ctx2 11.3%)
CENSUS inter-intra: ctx0 227  ctx1 731  ctx2 1902  (total 2860, ctx2 66.5%)
PRICE side  8 depth 0: row0 0.715  row2 0.430  delta -0.284
PRICE side 16 depth 0: row0 1.417  row2 0.811  delta -0.606
PRICE side 32 depth 0: row0 1.335  row2 0.431  delta -0.904
PRICE side 64 depth 0: row0 2.503  row2 0.964  delta -1.539
PRICE side  8 depth 1: row0 1.356  row2 1.956  delta +0.599
PRICE side 16 depth 1: row0 0.872  row2 1.429  delta +0.557
PRICE side 32 depth 1: row0 3.901  row2 4.656  delta +0.755
PRICE side 64 depth 1: row0 2.525  row2 2.460  delta -0.065
```

libaom's default `TX_SIZE_CAT*` rows 0 and 1 are identical, so only ctx2 moves
a price at all, and its direction is one-sided: **depth 0 gets 0.28–1.54 bits
cheaper, depth 1 gets 0.56–0.76 bits dearer**. Correct pricing therefore
discourages transform splitting everywhere it bites — and it bites two thirds
of an inter frame's intra blocks against one ninth of a key frame's, which is
precisely the asymmetry that makes film B (the inter-heavy row) the loser
while film A barely moves. The reading: the search's residual/λ calibration
was leaning on the row-0 handicap to reach the split rate that measures well;
the symbol price is not the mispriced term. Fixing it in isolation is a
correct change that the surrounding calibration is not ready for.

## Unit test (RED shown, then green; reverted with the arm)

`encode::tests::the_tx_depth_pricer_uses_the_writers_own_context_row` — the
key-frame rule table, and an inter-frame 16x16 intra block at mi (4,4) with
32x32 INTER neighbours published through `record_mi`. Red before, both halves:

```
assertion `left == right` failed: the grid's row is not the writer's
  left: 0
 right: 2
side 16 depth 0 at ctx 2: priced 1.4169172324970667, writer codes 0.8110248802158339
```

Green after the arm. Gone with the revert (it asserts on the reverted
signature); the numbers are kept here because they are the whole finding.

## Pins and suite

* Stream pins on the arm: `[(150, 8381, 0x36ac_5783_4a20_499a), (60, 33016,
  0xfcd0_be7a_3600_34ce)]` (was 8325 / 33014), green at the default preset and
  at `EC_AV1_SPEED=6`. No other byte pin went red. Back at 8325 / 33014 after
  the revert.
* Split suite on the ARM build, release lib, all RC=0: `--skip stream::` 339
  passed / 0 failed / 32 ignored; `stream:: --skip 10bit` 200 / 0 / 15;
  `10bit` 42 / 0 / 1.
* `--ignored encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  passed (reverted tree).
* `timeout 900 cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1
  warnings (the workspace's 25 are pre-existing, in ec-opus and ec-vorbis).

## Disposition

`deferred: the tx_size depth context — BOTH halves are now built and measured
and the arm loses; what would unblock it is not more context plumbing but the
split-decision calibration around it (λ or the residual term the depth trial
is compared under), since the correct price is a one-sided push away from
splitting. Do not rebuild the plumbing without first showing that the split
rate the pricer produces is the one that measures best.`

`deferred: the MV pricer — write_mv has PER-COMPONENT tables where the pricer
has one shared set (the long-standing static-MV approximation); untouched, as
in lane-ctx and lane-ctx2.`
