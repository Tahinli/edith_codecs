# lane-intersub8 r2 — HANDOFF (gate GREEN, 8x4/4x8 not started)

## Merge state
* `c048e29` main (8e9ba81) merged — no conflicts, `cdf.rs` untouched.
* `e8bb7e7` lane-r14 **b86eb38 merged** (it was NOT on main: main only carries the earlier
  `6699ce6`/`a2e2e29`). So `read_var_tx_size` here is `(tx_w,tx_h)`-general with the full
  `sub_tx_size_map`, and `read_inter_plane_rect` is available.
  NOTE (ledger, lane-inter16ab r3): that merge deletes the "split transform on a 16x16-level
  1:4 inter strip" refusal, so a TX_16X4→TX_8X4 split can reach
  `rect_inter_residual_supported(8,4)==false` / `rect_inter_luma_set` `unreachable!()`.
  The `LumaRect8x4Inter`/`LumaRect4x8Inter` TxbSets (step 3 below) close that too.

## Attempt-0 (cq 8) zero-group mismatch — ROOT CAUSE IDENTIFIED, out of scope
Not a sub-8x8 defect: **CDEF**. 3 luma samples `|d|=1`, decode-order frames 3 and 4
(x=189 resp. 186, y=116/120/124), zero sub-8x8 inter split groups in those frames; the
identical recipe re-encoded with `--enable-cdef=0` decodes all six frames pixel-exact
(`~/.cache/intersub8-tmp/gen.sh 8 3 8 a0_nocdef.obu --enable-cdef=0`, then `cmp.py`).
Also reproduces at cq 10. Deferred to a CDEF lane. The gate moved to cq {12,14} instead of
switching CDEF off (cdef=0 changes aomenc RD and then NO attempt fires a split group).

## Attempt-1 frame-5 ladder — FIXED (`96eff0c`)
First diverging element: the **switchable interp filter** of the 4x4 sub-block mi(14,46).
Ladder vs aomdec `EC_TRACE_MODE` on `~/.cache/intersub8-tmp/a1_8.obu`:
entry `39095` = , post-`assign_mv` `60392` = , after interp filter ours `51740` vs oracle `57815`.
Cause: the **compound 8x8 inter leaf never called `resolve_interp_filter`** — it hard-coded
`Regular` and recorded the `[3, 3]` sentinel in the neighbour filter band. libaom calls
`read_mb_interp_filter` for every inter block right after `read_compound_type`, and when
`av1_is_interp_needed()==0` (`skip_mode`) `set_default_interp_filters` stores
EIGHTTAP_REGULAR **0**. The left neighbour here was a skip_mode compound 8x8 leaf, so our
`av1_get_pred_context_switchable_interp` used 3 instead of 0 → wrong CDF row → desync.
Fix reads the symbols, uses `h_filter`/`v_filter` in all six `predict_compound_*` calls of
that branch, records the real syms. This also removes the old lane-av1comp corner-cut
(a NON-skip compound 8x8 leaf used to skip the symbol entirely = latent desync).
New rungs for the next hunt: `EC_TRACE_MODE` now prints `EC_MODE` + `EC_MODE_MV` on the
sub-8x8 path at the oracle's two print points, and `EC_AV1_IFDBG` prints `IFDBG4` with the
neighbour ref/filter bands.

## 8x4/4x8 (charter step 3) — NOT STARTED
Still refused by name: `an inter 8x8 HORZ/VERT partition (8x4/4x8 inter leaves; the 4x4
SPLIT arm decodes, but a rectangular sub-8x8 inter transform is unimplemented)`.
Remaining pieces: `TxbSet::LumaRect8x4Inter{,1}` / `LumaRect4x8Inter{,1}` (copy the key-frame
8x4/4x8 intra sets' scan/ctx, swap in `inter_tx_type_4`; `av1_get_ext_tx_set_type` →
EXT_TX_SET_ALL16 for inter with `sqr_up == TX_8X8`), ONE `txfm_partition` bit at max
TX_8X4/TX_4X8 (r14's general `read_var_tx_size` handles it once max=(8,4)), mode info per
sub-block, chroma once on the LAST sub-block as 4x4 with 2 sub8x8 pieces, per-mi neighbour
writes. Gate arm: same recipe with `--enable-rect-partitions=1` (oracle histogram on the r1
recipe: bsize=3 v1=28 HORZ, v2=49 VERT, v3=3 SPLIT).

## Gate state
`cargo test -p ec-av1 --lib sub8x8_inter_split` → **ok, 1 passed / 0 failed** (414 filtered).
Schedule measured by a 56-encode sweep (`~/.cache/intersub8-tmp/sweep.sh`): cq {12,14} ×
sp {3,6,9,12} × 8/10-bit. Firing: 8-bit cq12/sp3 = 1 group, 10-bit cq12/sp6 = 3 groups, both
pixel-exact; `out_of_scope_mismatch == 0`; every decode-order frame compared Y/U/V.
`decode_probe` now prints `sub8_inter_split: groups=N`.
`fixtures/realworld/hunger-games.obu` now stops at the 8x4/4x8 refusal (was: the SPLIT one).

## Suite
Unit `intersub8-suite-1788328909.service`, log `$HOME/.cache/intersub8-suite-r2.log`.
FINISHED GREEN after this handoff was first written:
`test result: ok. 382 passed; 0 failed; 33 ignored; 0 measured; 0 filtered out; finished in 1018.88s`.
That run IS the sibling sweep for the interp-filter fix (it touches every compound 8x8 leaf).

## Exact next step
1. Read the suite log (above). 2. Build `LumaRect8x4Inter`/`LumaRect4x8Inter` TxbSets and the
8x4/4x8 leaf, add the `--enable-rect-partitions=1` gate arm plus a `--tile-columns=1` arm
(COMMON neighbour-map rule, still owed), then drop the 8x4/4x8 refusal + inventory line.
