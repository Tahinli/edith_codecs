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
