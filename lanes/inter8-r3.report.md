# lane-inter8 r3 -- the 10-bit twin localized, and the mi-band tile audit

Branch `lane-inter8`, on top of r2's `12809b2`. This report covers r3 only;
`lanes/inter8-r2.report.md` is untouched.

## 1. Gate state

- `a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact`
  (8-bit): **GREEN**, un-ignored (r2). Re-run this round, still green.
- `a_real_aomenc_10bit_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact`:
  **still RED**, stays `#[ignore]`d. Localized this round (section 2); the
  root cause is a symbol lane-gmaffine r3 owns, so it is NOT fixed here.
- `a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_across_tile_columns_decodes_pixel_exact`:
  NEW this round (`--tile-columns=1`, 128x64). **RED for a reason outside this
  lane** and `#[ignore]`d with that reason in the attribute: at 128x64 aomenc
  codes an SB-level rect partition on the KEY frame despite
  `--enable-rect-partitions=0`, so the stream stops at "a superblock-level
  partition type other than NONE or SPLIT" (lane-sbpart's arm) before any 8x8
  leaf is reached. 64x64 -- the only width the recipe decodes -- is one
  superblock wide, so two tile columns cannot exist there.

Gate-helper audit (class *counter-from-refused-stream*): `inter_sb_none_gate`
(`crates/ec-av1/src/stream.rs:2774`) is single-attempt -- `before = hits()` is
taken immediately before the one `decode_stream` call, a refusal panics with a
pinned stream, a pixel mismatch panics. No global-delta-over-many-attempts
shape, nothing to fix.

## 2. The 10-bit twin: first divergent element

The refusal ("an INTER 32x32 partition type this decoder does not code
(value=9)") is downstream noise; the real divergence is 15 elements earlier.

Ladder (ours `EC_TRACE_MODE` via `decode_probe` vs instrumented aomdec
`EC_TRACE_MODE`), 64x64 mandelbrot, cq40 cpu0, `--min-partition-size=8
--max-partition-size=16`, `--bit-depth=10`:

- 8-bit stream: **57/57** `EC_MODE` lines agree (r2's claim re-measured).
- 10-bit stream: **41/57**. First divergence at the ENTRY of the next block
  after the 8x8 leaf `mi=(4,14)`: ours `rng=57758`, aomdec `rng=52508`.

Element-level, on leaf `(4,14)` (a compound `NEW_NEARESTMV`, ref0=1 ref1=4):

| point | ours | aomdec |
|---|---|---|
| entry (`EC_MODE`) | 37004 | 37004 |
| after mode + DRL + mv (`EC_MODE_VAL` ours / `EC_MODE_MV` oracle) | **57308** | **57308** |
| end of mode info (`EC_MODE_VAL` oracle) | -- | 58952 |
| next block entry | 57758 | 52508 |

Values agree everywhere (mode=20, ref0=1, ref1=4, mv0=(0,-9), mv-stack size 2
on both sides). So: the mv-stack, the DRL count and the mv reads of the 8x8
leaf are **exact**; the divergence is entirely in the leaf's **post-mv mode
info**, where aomdec's range moves 57308 -> 58952 and ours does not move the
same way.

The only symbol aomdec reads there for a compound 8x8 leaf is
`read_mb_interp_filter` (interintra is single-ref-only, motion_mode is not
read for compound). `decode.rs:11181` says it outright: *"`decode_inter_block8`
never resolves a switchable filter"* -- the leaf hardcodes `Regular` and reads
no filter symbol. That is a **symbol-consumption gap** (memory class), and per
the coordinator it is lane-gmaffine r3's to fix (it owns the real leaf interp
read and is merging ae69b25). fix-now, owner lane-gmaffine.

Two instruments were added to get here and are kept:
- ours: `EC_MODE_VAL` in `decode_inter_block8`'s compound arm (`decode.rs`
  ~11049), byte-format-identical to the oracle's rung-4 line.
- oracle: `EC_MODE_MV` in `~/.cache/aom-oracle/src/av1/decoder/decodemv.c`
  right after `assign_mv`, plus `stack=%d` (`dcb->ref_mv_count[...]`) on
  `EC_MODE_VAL`. **These two oracle patches are NOT in
  `scripts/instrument-aom-oracle.sh`** -- a rebuild of the oracle from the
  script drops them. deferred(a 3-line append to that script; the aomdec
  binary in `~/.cache/aom-oracle/build` currently has them).

## 3. Tile-relative guard audit (lane-sub8 verifier's class warning)

Checked every reader/writer of the mi-granular inter bands added in r2
(`record_inter_rect_mi` `decode.rs:2300`, `record_compound_ctx_rect_mi`
`decode.rs:2389`, and the `above_side_mi`/`left_side_mi`/`above_skip`/
`left_skip`/`*_inter`/`*_ref`/`*_ref1`/`*_comp_group_idx`/`*_compound_idx`
readers):

- **Guards are tile-relative.** `grep -n 'mi_r > 0\|mi_c > 0\|leaf_mi.0 > 0\|leaf_mi.1 > 0' decode.rs`
  returns **zero hits**; the leaf uses
  `leaf_mi.0 > neighbours.tile_row0_mi` / `leaf_mi.1 > neighbours.tile_col0_mi`
  (`decode.rs:10908`).
- **Above bands reset per tile.** `start_tile` (`decode.rs:1958`) clears
  `above_side_mi` and, from r2, every mi-granular above inter band over
  `col0_mi..col1_mi`.
- **Left bands reset per SB row.** `start_row` (`decode.rs:2229`) clears
  `left_side_mi`, `left_skip`, `left_skip_mode`, `left_inter`, `left_ref`,
  `left_ref1`, `left_comp_group_idx`, ... unconditionally.

So the lane-sub8 defect shape is not present here. The runtime arm that would
have proven it is the `across_tile_columns` gate above, blocked by an unrelated
refusal -- stated as inspection-only, not claimed as exercised.

## 4. Remaining alphabet -- named next slices

- `fix-now (lane-gmaffine r3)`: the 8x8 leaf's switchable interp filter read;
  it is the 10-bit twin's first divergent element and un-ignores that gate.
- `deferred(lane-sbpart's SB-level rect arm)`: the `--tile-columns=1` runtime
  proof of the mi bands, and with it any multi-tile inter gate.
- `deferred(rect inter MC + rect transforms below 16x16; lane-rectx owns the
  transform half)`: 16x8/8x16 rect inter leaves at the 16x16 level -- the
  refusal r1 narrowed to ("... other than SPLIT") still stands.
- `deferred(the above)`: 8x8 HORZ/VERT -> 8x4/4x8 leaves. Needs 4x8/8x4 luma
  and 4x4 chroma transforms plus a 4-wide MC path; strictly after the 16x8/8x16
  rect leaves.
- `deferred(unstarted)`: AB (HORZ_A/B, VERT_A/B) and 1-to-4 (HORZ_4/VERT_4)
  inter arms at 64/32/16. aomenc emits them even with the flags off, so these
  are the dominant remaining inter refusals, but each needs its own transform
  set (a 64x16 strip needs TX_64X16 -- see the ledger's lane-part32 r5 line).
- `accepted`: the aggregate per-type-per-level inter hit-counter gate. Every
  arm above is refused, so the counter would assert nothing new.

## EVIDENCE

EVIDENCE: scratchpad s8.obu sha256 344566f8...8802 / s10.obu sha256 f1a53394...20ce | aomenc 64x64 mandelbrot 4f cq40 cpu0 min=8 max=16 (8- and 10-bit), decoded by ours (EC_TRACE_MODE, decode_probe) and by instrumented aomdec (EC_TRACE_MODE) | EC_MODE range ladders: 8-bit 57/57 agree, 10-bit 41/57, first divergence at the block AFTER leaf mi=(4,14)
EVIDENCE: same two traces | aomdec patched with EC_MODE_MV (post-assign_mv) + stack= on EC_MODE_VAL, rebuilt (cmake --build . --target aomdec) | leaf (4,14): entry 37004==37004, post-mv 57308==57308, mode/refs/mv0/stack all equal -- divergence is strictly after the mv, in the post-mv mode info aomdec ends at rng 58952
EVIDENCE: cargo test -p ec-av1 --lib -- 8x8_leaf_split (EC_AV1_REQUIRE_AOMENC=1) | 8-bit gate re-run | 1 passed, 10-bit + tile-columns ignored with their reasons
