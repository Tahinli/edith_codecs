# lane-ctx — the RD pricer's remaining context-zero sites

Class: `pricer-context-zero`. lane-pricer fixed `single_ref_bits`; the sweep it
left named the rest. This lane took them.

## 1. Enumeration — every `symbol_bits(&cdf::` site in `crates/ec-av1/src/encode.rs`

| pricer site (encode.rs) | writer (tile.rs) | how the writer picks the context | verdict |
|---|---|---|---|
| `SKIP[0]` — `code_square_inter`, `search_skip_64`'s `fixed`, the b64 `unskip` term, `search_inter_block` | `enc.symbol(block.skip, &mut cdfs.skip[skip_ctx])` (4 sites: 6275/6673/6936/7189) | `above_skip[mi_c] + left_skip[mi_r]` | **arm A — FIXED** (`skip_bits_at`) |
| `INTRA_INTER[0]` — same four functions | `cdfs.intra_inter[ii_ctx]` (6284/6681/6944/7196) | `intra_inter_ctx(has_above, has_left, above_inter, left_inter)` | **arm A — FIXED** (`intra_inter_bits_at`) |
| `TX_SIZE_CAT0..3[0]` — `tx_depth_bits` (5 call sites in `code_square`) | `write_luma_select` / var-tx twin (4343/4436) | `Neighbours::tx_size_ctx(at_mi, max_tx)` = above/left published transform side >= this block's `max_tx`, and `tx_size_ctx_txfm` on the var-tx path | **arm B — DEFERRED** (see §5) |
| `COMP_MODE[0]` (3 sites) | `write_comp_mode` (1154) | `reference_mode_ctx(above_nbr, left_nbr)` | **arm C — FIXED** |
| `COMP_REF_TYPE[0]` (3) | `write_compound_ref_frames` (856) | `comp_reference_type_ctx(above, left)` | **arm C — FIXED** |
| `COMP_REF[0][0]=0`, `[0][1]=0` (3) | 889 / 899 | `single_ref_p3_ctx` / `single_ref_p4_ctx`; value right only because compound `ref0` is always LAST | **arm C — FIXED** (real pair) |
| `COMP_BWDREF[0][0]=1` (3) | 905 / 910 | `single_ref_p2_ctx`, then `single_ref_p6_ctx` when `ref1 != ALTREF` | **arm C — FIXED**: the constant priced BWDREF/ALTREF2 as ALTREF and dropped p6 |
| `UNI_COMP_REF[0][0]=0,[0][1]=1,[0][2]=1` (3) | 862 / 876 / 881 | `single_ref_p1_ctx` / `uni_comp_ref_p1_ctx` / `single_ref_p5_ctx` | **arm C — FIXED**: the constants priced every forward partner as GOLDEN |
| `SINGLE_REF[p*_ctx]` | `write_single_ref` | its own `single_ref_p*_ctx` | already fixed (lane-pricer) |
| `TXFM_PARTITION[ctx(..)]`, `DRL_MODE[stack.drl_ctx[..]]`, `NEW_MV`/`ZERO_MV`/`REF_MV`/`INTER_COMPOUND_MODE`, `ANGLE_DELTA[mode - V_PRED]`, `UV_MODE_CFL[mode]`, `Y_MODE[SIZE_GROUP_32]`, `FILTER_INTRA_MODE` | — | already the writer's own context/row | no change |
| `MV_JOINT`/`MV_SIGN`/`MV_CLASS`/`MV_BIT`/`MV_FR`/`MV_CLASS0_*` | `write_mv` | writer has PER-COMPONENT tables where the pricer has one shared set | out of this lane (long-standing static-MV approximation) |

Open, found while enumerating: the single-reference candidates pay **no**
`comp_mode = 0` term at all while the compound ones pay `comp_mode = 1`, so
compound is still overcharged by `comp_mode(0)` relative to single. Left alone
here so arm C measures one thing.

## 2. Arms and gates (12-frame native gate, BD-rate vs libaom cpu-used 6 / rav1e speed 6)

Control reproduced in this worktree at `e74e7229`, exactly the charter's numbers.

| row | control | arm A (skip + is_inter) | arm C (cumulative, + compound tree) |
|---|---|---|---|
| film A | +21.5 / −4.4 | +21.5 / −4.5 | **+21.0 / −4.8** |
| film B | +26.9 / −0.4 | +26.4 / −0.7 | **+25.0 / −1.8** |
| screen | +14.4 / −33.2 | +14.5 / −33.2 | **+14.5 / −33.2** |
| bars 1080p | −1.4 / −17.2 | −1.8 / −17.4 | −1.8 / −17.5 |
| bars 2160p | +8.8 / −13.2 | +8.5 / −13.5 | +8.8 / −13.3 |

**arm A ships** under the second keep rule: film B is 0.5 down on libaom and
0.3 down on rav1e, film A flat on both (0.0 / −0.1), screen 0.1 up on libaom
(inside the 0.3 the rule allows).

**arm C ships** under the first keep rule: both film rows improve on both
columns (−0.5/−0.3 and −1.4/−1.1), screen flat.

Cumulative long-GOP gate (`bd_rate_film_long_gop`), both arms in:

| row | control | shipped |
|---|---|---|
| film A | +26.4 / −6.4 | **+25.2 / −7.1** |
| film B | +89.6 / +9.1 | **+87.1 / +7.7** |

## 3. Unit tests

* `encode::tests::the_skip_and_intra_inter_pricers_use_the_writers_own_contexts`
  — RED at row 0: `skip false at ctx 1: priced 0.049125199808023, writer codes
  0.9885106508271125`. Green after arm A.
* `encode::tests::the_compound_ref_pricer_pays_the_writers_own_symbol_sequence`
  — RED against the old constant pricer: `pair (1, 2) ... priced
  10.19573638784928 bits, writer codes 5.455955052750427` (a LAST+LAST2 pair
  priced as LAST+GOLDEN). Green after arm C.

## 4. Pins and suite

* Stream pins re-taken: `[(150, 8311, 0xfb6d_75a4_5d13_3833), (60, 33087,
  0x4d11_a03c_bdc6_e2d5)]` (was 8535 / 33221), green at the default preset and
  at `EC_AV1_SPEED=6`.
* `mi_info_cell_stays_compact` / `mi_info_stays_small_enough_for_a_4k_grid`
  went RED mid-lane: `skip` as a `MiInfo` field took the cell 16 → 18 bytes,
  and that struct is the mv-stack scan's memory traffic. The flag moved to a
  `Vec<bool>` beside `MiGrid::cells`, filled only by the encoder's `record_mi`
  and read only by the stack. Both pins unchanged by that move, so the gate
  numbers above stand.
* Split suite, release lib, all RC=0: s1 (`--skip stream::`) 338 passed / 0
  failed / 32 ignored; s2 (`stream:: --skip 10bit`) 42 / 0 / 1; s3 (`10bit`)
  208 / 0 / 16. `--ignored
  encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  passed.
* `timeout 900 cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1
  warnings (the workspace's 22 warnings are pre-existing, in ec-opus and
  ec-vorbis).

## 5. Deferred

`deferred: arm B (tx_size depth category context) — the pricer's
tx_depth_bits lives in code_square, which takes neither the mi grid nor the
block's mi coordinates, and the writer's tx_size_ctx/tx_size_ctx_txfm read a
PER-TRANSFORM-UNIT published side (Neighbours::record_tx) that MiGrid does not
carry at all; reproducing it needs a new published tx band plus a parameter
through every code_square caller — what unblocks it: publish the resolved
transform side per mi cell beside MiGrid::cells the way this lane's skip band
now rides, then thread (grid, mi) into code_square.`
