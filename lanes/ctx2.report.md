# lane-ctx2 — the last two `pricer-context-zero` sites

Class `pricer-context-zero`: the RD pricer in `crates/ec-av1/src/encode.rs`
priced symbols at CDF row 0 (or with the wrong tree) while the writer in
`crates/ec-av1/src/tile.rs` derives every context from neighbours. lane-pricer
and lane-ctx took single refs, `skip`, `is_inter` and the compound tree. This
lane took the two left: the `comp_mode` term (arm D, SHIPS) and the `tx_size`
depth context (arm B, REFUTED and reverted).

## arm D — a single-reference candidate pays `comp_mode = 0`

On a `reference_select` frame `tile::write_comp_mode` codes one `comp_mode`
symbol ahead of EVERY inter block's reference syntax. `compound_ref_bits`
already paid `comp_mode = 1`; `single_ref_bits` paid nothing at all, so the
compound side of every leaf carried the whole symbol as a handicap.
`single_ref_bits` now adds `comp_mode = 0` at the writer's own
`reference_mode_ctx`, and pays zero on a frame that carries no
`reference_select` bit -- exactly where `write_comp_mode` returns early.

12-frame native gate, BD-rate vs libaom cpu-used 6 / rav1e speed 6:

| row | control (09590c10) | arm D |
|---|---|---|
| film A | +21.0 / −4.8 | +21.0 / −4.8 |
| film B | +25.0 / −1.8 | **+24.9 / −1.9** |
| screen | +14.5 / −33.2 | +14.5 / −33.2 |
| bars 1080p | −1.8 / −17.5 | −2.0 / −17.6 |
| bars 2160p | +8.8 / −13.3 | +8.7 / −13.4 |

Ships under the charter's THIRD keep rule (a correctness fix flat within ±0.2
everywhere): film B improves 0.1 on both columns, both bars rows improve, film
A and screen are unmoved. The long-GOP gate is where it reads as a real win:

| row | control | arm D |
|---|---|---|
| film A | +25.2 / −7.1 | **+25.2 / −7.2** |
| film B | +87.1 / +7.7 | **+86.7 / +7.3** |

Control note: the control was NOT re-run at 09590c10 in this worktree (each row
is ~2 min of wall on top of six gate runs). It is corroborated instead -- film A
and screen come back byte-identical to the charter's control numbers on the arm
row, which a stale control could not do.

## arm B — the `tx_size` depth context, REFUTED

`tx_depth_bits` priced every block's `tx_depth` symbol at row 0, the row where
both neighbours are NARROWER than this block's transform and splitting reads
cheapest. Built as lane-ctx's recipe asked, for the key-frame path: the
key-frame tile search keeps the same two bands `Neighbours::record_tx`
publishes (each coded block's resolved transform side over its own 4x4 units,
frame-absolute so a superblock row needs no reset, a losing trial overwritten
by the winner's own publish exactly as `above_mode`/`left_mode` are), the
context rule itself moved into `tile::tx_size_ctx_of` so writer and pricer read
ONE function, and `code_square` takes the row its caller derives.

| row | control (arm D) | arm B |
|---|---|---|
| film A | +21.0 / −4.8 | +21.1 / −4.6 |
| film B | +24.9 / −1.9 | +25.0 / −1.9 |
| screen | +14.5 / −33.2 | +14.5 / −33.2 |
| bars 1080p | −2.0 / −17.6 | −2.0 / −17.6 |
| bars 2160p | +8.7 / −13.4 | +8.7 / −13.4 |

Every film column moves the wrong way for no win anywhere, so the keep rule is
not met and the commit is reverted (`77347164`). The likely reason it costs:
this is HALF the fix. An INTER frame's intra block takes its row from the
`TXFM_CONTEXT` bands instead (`tx_size_ctx_txfm`, where an inter neighbour
votes its own BLOCK size), which the encoder publishes nowhere, so with arm B
in, key frames and inter frames judge the same depth-versus-split margin
against different rows. What would unblock a second attempt: publish the
`TXFM_CONTEXT` band on the inter path too (the `MiGrid` already carries
`size`/`is_inter` for the inter-neighbour half) and price BOTH paths at once.

## Unit tests

* `encode::tests::the_single_ref_pricer_pays_the_writers_own_symbol_sequence`
  — extended with the `comp_mode` term and a compound left neighbour (which is
  what takes `reference_mode_ctx` off row 0). RED before arm D:
  `ref 1 above (None, None) left (None, None): priced 2.0905436498760297 bits,
  writer codes 2.5376944668507715`. Green after.
* `encode::tests::the_tx_depth_pricer_uses_the_writers_own_context_row` — arm
  B's, green while arm B was in, gone with the revert. Recorded here because it
  named the thing that limits the whole arm: libaom's default `TX_SIZE_CAT*`
  tables give rows 0 and 1 IDENTICAL probabilities, so the only row that moves
  a price at all is row 2 (both neighbours at least as wide).

## Pins and suite

* Stream pins re-taken on arm D: `[(150, 8325, 0xf004_8258_bc60_ff05),
  (60, 33014, 0x5bc2_5dfd_bd13_18de)]` (was 8311 / 33087), green at the default
  preset and at `EC_AV1_SPEED=6`. No other byte pin went red.
* Split suite, release lib, all RC=0: `--skip stream::` 338 passed / 0 failed /
  32 ignored; `stream:: --skip 10bit` 200 / 0 / 15; `10bit` 42 / 0 / 1.
  `--ignored
  encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  passed.
* `timeout 900 cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1
  warnings (the workspace's 22 are pre-existing, in ec-opus and ec-vorbis).

## Deferred

`deferred: the tx_size depth context — the key-frame half is built and
measured (arm B above) and LOSES on its own; what unblocks it is the inter
half, a TXFM_CONTEXT band published on the inter path so both frame types
price the depth symbol at the writer's row at the same time.`

`deferred: the MV pricer — write_mv has PER-COMPONENT tables where the pricer
has one shared set (the long-standing static-MV approximation); untouched
here, as in lane-ctx.`
