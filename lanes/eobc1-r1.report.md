# lane-eobc1 r1 — eob_pt class-1 rows at every size

Branch `lane-eobc1`, rebased onto main `df5d630` (was off `06d856d`). Two commits:
`b3b18b3` (the killed agent's work, rebased) + the checker widening below.

## What changed

- `crates/ec-av1/src/cdf.rs` — 5 remaining class-1 tables ported from libaom
  (`EOB_PT_128_CHROMA_CLASS1*`, `512_LUMA/CHROMA_CLASS1*`, `1024_LUMA/CHROMA_CLASS1*`,
  4 q-contexts each); `cdf::tests::eob_pt_class1_rows_cover_every_size` pins the
  trained-vs-uniform split (8 trained rows differ from their 2D sibling; the four
  32-point rows are libaom's untouched uniform initialiser, because
  `av1_get_ext_tx_set_type` gives DCT_IDTX/DCTONLY at `tx_size_sqr_up >= TX_32X32`).
- `crates/ec-av1/src/cdf_state.rs` — 11 class-1 fields, 11 `pick(q_ctx, ...)` sites,
  11 `reset1` entries in the per-frame reset; **`eob_pt_class1: None` count is 0 across
  all 21 `TxbSet` arms** (`grep -c "eob_pt_class1: None"` → 0, `Some` → 21).
- `crates/ec-av1/src/decode.rs` — `chroma_eob_class1_hits()` counter, bumped from
  `read_plane` + `read_inter_plane` when an inherited 1D `tx_type` puts a chroma block
  on the class-1 row.
- `crates/ec-av1/src/stream.rs` — gate `chroma_class1_eob_gate` in 8-bit and 10-bit arms.
- `scripts/extract-eob-class1.py` — **widened this round to check BOTH classes** (was
  class-1 only), i.e. the enumerate-table-domain check now also covers the 2D rows.

## Verification

`python3 scripts/extract-eob-class1.py ~/.cache/aom-oracle/src/av1/common/token_cdfs.h --check crates/ec-av1/src/cdf.rs`
→ `checked 88/112 consts, 0 mismatched`.

EVIDENCE: $HOME/.cache/eobc1-suite.log | EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib | 301 passed, 1 failed, 27 ignored (1252s); `a_real_aomenc_inter_sequence_with_a_class1_chroma_eob_decodes_pixel_exact` ok (line 949) and `a_real_aomenc_10bit_..._class1_chroma_eob_...` ok (line 936)
EVIDENCE: script stdout above | extract-eob-class1.py --check against ~/.cache/aom-oracle token_cdfs.h | 88/112 consts bit-exact, 0 mismatched; 24 absent consts are whole tables the crate has no TxbSet for (32-coeff, 128-luma)
EVIDENCE: $HOME/.cache/eobc1-mainbase.log | same single test at detached main df5d630, own target dir | `nz_map_ctx_offset_tables_match_the_rect_rule` FAILED at main too — the suite's one red is inherited, not this lane's

Siblings re-run green in the same suite: `tx_select` (3 gates), `an_8x8_leaf_split`,
`tiny_frame_size_sweep`, all `inter_sequence` gates, whole-superblock 8/10-bit,
`refusal_inventory` (3), `gate_coverage` (9).

## Charter notes / residue

- rectsplit r4's suspicion that main's `EOB_PT_512_CHROMA` was transcribed from the
  class-1 row is **disproved**: with the checker widened to class 0, all 44 2D consts
  match libaom bit-exact. accepted.
- No refusal string lifted this round (the fix is a table wiring defect, not a
  capability), so `refusal_inventory.rs` / `gate_coverage.rs` are unchanged.
- deferred(a TxbSet for 32-coefficient transforms i.e. TX_4X8/8X4, and for 128-coeff
  luma TX_8X16/16X8): the 24 consts the checker reports missing are those two absent
  table families — both classes missing, so this is not a class-1 gap. Unblocked by
  lane tx4x8 / a 128-luma TxbSet; the checker will flag them the moment they land.
- fix-now-for-another-lane: `decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule`
  is red on main df5d630 (`32x64 nz_map offset at display (row 0, col 2): left 6, right 11`).
