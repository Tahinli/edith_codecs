# lane-inter16ab r7 — HANDOFF (turn cap hit on the merges; root cause HALF found)

## 1. Merges done (all committed on lane-inter16ab)
| commit | what |
|---|---|
| `fb2c738` | merge main `787c66f` (contains `dfe9ce8` interedge, `48216c2` rectchroma2, `2d76270` cdef, sbrect10 uv_mode) |
| `5969a3c` | merge `lane-intersub8` `a5b9770` (sub-8x8 inter leaves) |
| `2d31045` | merge `lane-sbab` `1847e97` (inter SB-level AB + var-tx side band) |
| `6a8f821` | this round's fix (below) |

Conflict resolutions (all hand-resolved, `cargo check -p ec-av1` clean after each):
- `decode_probe.rs`: kept both counter blocks, one `vartx_rect_leaf` print only.
- `cdf_state.rs`: intersub8's `TxbSet::LumaRect8x4Inter`/`LumaRect8x4InterSet1` are the SAME
  tables this branch already had as `LumaRect8x4Inter`/`LumaRect8x4Inter1` — kept OURS, deleted
  the duplicate arms (they were duplicate patterns in one `match`), renamed the one intersub8
  use site in `decode.rs`.
- `decode.rs` compound row fix: kept OURS (`bsize_all_index`/`wedge_used_bsize`/
  `is_any_masked_compound_used_here`); intersub8's `bsize_index`/`wedge_used_wh`/
  `masked_compound_used_wh` helpers remain in the tree, unused by this site.
- `refusal_inventory.rs`: dropped `"an inter partition below 8x8 ..."` (intersub8 lifted it) and
  `"an inter SB-level AB partition ..."` (sbab lifted it), kept both branches' remaining strings.

MERGE CROSS-PRODUCT DEFECT (class merge-cross-product-defect), fixed in `6a8f821`:
lane-intersub8 r4 added `"a COMPOUND_WEDGE mask on a non-square inter block (rect wedge codebook
unimplemented)"` at `decode.rs` inside the `compound_type == 0` arm. This branch's r5 IMPLEMENTS
that codebook (`wedge::wedge_masks().codebook(w, h)`), so after the merge the refusal shadowed live
code and g14.obu refused instead of decoding. Refusal + inventory line removed.

## 2. Root cause work on g14.obu (192x128 8-bit cq18 tx-search=1, `~/.cache/inter16ab-tmp/g14.sh`)
Detached decode unit `inter16ab-g14-r7`, log `$HOME/.cache/inter16ab-g14-r7.log`:
`OK: 6 frames decoded` / `rect32x8_inter_tu: 32x8=2 8x32=0` / `g14o.yuv g14f.yuv differ: byte 122402`
(= frame 3, luma row 61) — STILL RED, same byte as r6.

Established this round (all by cross-decoder msac RANGE ladder vs `~/.cache/aom-oracle/build/aomdec`,
`EC_TRACE_MODE`/`EC_TRACE_MODE_STEP`/`EC_TRACE_COEFF` on both; artefacts
`~/.cache/inter16ab-tmp/{aom_all.txt,our_all.txt,m3.txt,u.txt}`):
1. The desync is NOT in the new 32x8 path. The two coded TX_32X8 units live in DECODE frame 4
   (rng 49036/50156, block mi(24,0) of trace group 3); frame 3 is already wrong before them.
   Trace groups are frames 1..5 (the key frame emits no `EC_MODE`).
2. First divergence: frame 3 (group 2), block **mi(16,0)** — first block of superblock row 1.
   Mode/ref/MV/compound/interp all match (both at rng 51700). The very next symbol, the
   var-tx `txfm_split`, differs: ours ctx=13 → rng 60378, aomdec → rng 47777.
3. Forcing the ctx (temporary `EC_TXFORCE` probe, removed again) proved **ctx=12 reproduces
   aomdec's 47777 exactly**. So `txfm_partition_context` inputs are wrong, not the CDF.
4. Two input bugs found; ONE fixed in `6a8f821`:
   - FIXED: our `above_txfm`/`left_txfm` bands were never reset. libaom
     `av1_zero_above_context` (av1_common_int.h:1624) memsets the txfm row over the TILE's mi
     column span to `tx_size_wide[TX_SIZES_LARGEST]`==64, and `av1_zero_left_context`
     (called per superblock row, decodeframe.c:2788/3230) memsets the left band to 64.
     Ours kept the PREVIOUS FRAME's values. Reset added in `Neighbours::start_tile` (above) and
     `Neighbours::start_row` (left). `left_px` at mi(16,0) now reads 64 (was stale).
   - **OPEN, the remaining half**: `above_px` at mi(16,0) is **8**, libaom has **16**.
     The blocks covering mi cols 0..3 of the row above are the 16x16-level **AB partition**
     (HORZ_A) sub-blocks at mi(12,0), mi(12,2) and the 16x8 strip at mi(14,0) — all SKIP.
     An `EC_TXUPD` probe on `txfm_partition_update_rect` showed those three blocks emit **no
     txfm-band write at all**, so the band keeps 8 from an earlier block. libaom's
     `set_txfm_ctxs(tx_size, n4_w, n4_h, skip && is_inter, xd)` runs for EVERY block: with
     `skip` it writes `bw = n4_w*MI_SIZE` above and `bh = n4_h*MI_SIZE` left — the 16x8 strip
     at mi(14,0) therefore writes **16** over mi cols 0..3, which is exactly the missing 16.

## 3. EXACT NEXT STEP
Find the 16x16-level AB partition decode path (`inter_ab16` arms, `AB16_HITS`, `decode.rs`
around the 16x16 AB sub-block loop) and make each of its sub-blocks — including the skip ones
and the rect strip — call `set_txfm_ctxs(neighbours, at_mi, tx_px, w_mi, h_mi, skip && is_inter)`
(or `read_block_tx_size{,_rect}`, which does it) with the sub-block's OWN mi width/height.
Ours already has both helpers: `set_txfm_ctxs` (decode.rs:12175) and
`txfm_partition_update_rect` (decode.rs:12150). Verify with:
`EC_TRACE_MODE=1 EC_TRACE_MODE_STEP=1 decode_probe g14.obu | grep 'mi_row=16 mi_col=0 name=txfm_split'`
→ must print `ctx=12 above_px=16 left_px=64 rng=47777` (the trace line already prints
`above_px`/`left_px`, added this round). Then re-run the cmp; then the same sweep for the
SB-level (64) AB arms merged from lane-sbab, which almost certainly share the defect.

## 4. Not done (turn cap)
- deferred: full suite run (`cargo test -p ec-av1 --lib`) — nothing since the merges has been
  suite-checked; only `cargo check` + `cargo build --example decode_probe` are green.
- deferred: HG -ss 900 film probe (`~/.cache/inter16ab-tmp/hg900.obu`).
- deferred: the 1:4 / 32x8 gates and the refusal-string drop for the now-supported shapes.
- accepted (r6 residue, unchanged): the CDEF-over-skip-32x8-strip ±1 defect (`a8.sh`/`a8n.sh`).
