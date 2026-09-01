# lane-intrabc r1 — the block vector decodes; the transform tree does not

## Verdict

The headline refusal `a block that actually uses intrabc (this decoder never
reconstructs one)` is **replaced, not lifted**. Its successor names the real
remaining wall:

```
an intrabc block under TxMode::Select (its transform size is coded by the
inter var-tx partition tree, which this decoder never reads)
```

Everything the charter listed *before* the residual is implemented and runs on
real aomenc streams: `use_intrabc`, the DV's own mv stack, the `ndvc` full-pel
`read_mv`, `av1_find_ref_dv`'s fallback, and the block-copy prediction. No
pixel-exact gate exists yet, because no aomenc recipe found this round produces
an intrabc block that does *not* also need `read_var_tx_size`.

## What changed (worktree `edith_codecs-intrabc`, branch `lane-intrabc`)

- `crates/ec-av1/src/cdf_state.rs:422` — `dv_joint`/`dv_comp`: libaom's `ndvc`
  (`av1_init_mv_probs` gives it the same spec-8.4 defaults as `nmvc`, separate
  adaptation), added to the per-frame counter-reset list at `:684`.
- `crates/ec-av1/src/decode.rs:590` — `INTRABC_MI_GRID` / `INTRABC_DV` /
  `INTRABC_HITS` thread-locals + `intrabc_hits()`, `reset_intrabc_hits()`,
  `record_intrabc_mi()`. The mi grid exists only while a frame header set
  `allow_intrabc`.
- `crates/ec-av1/src/decode.rs:4046` (`read_intra_mode`) — `use_intrabc == 1`
  now reads the DV instead of refusing, and returns immediately with
  `YMode`/`UVMode` forced `DC_PRED` (libaom `read_intra_frame_mode_info`
  returns at `decodemv.c:895`, so no mode/angle/palette/CFL/filter-intra
  syntax follows).
- `crates/ec-av1/src/decode.rs:7681` — `read_intrabc_dv`: MV stack against
  `INTRA_FRAME`, `nearest` else `near`, else `av1_find_ref_dv`
  (`(0, -(64+256)*8)` in the first SB row, else `(-64*8, 0)`), predictor
  floored to full pel, then `read_mv` on `dv_comp`/`dv_joint` with
  `MV_SUBPEL_NONE` semantics (`force_integer_mv=true`,
  `allow_high_precision_mv=false`). No DRL symbol is coded for a DV, so a
  wrong predictor can only move pixels, never the bitstream position.
- `crates/ec-av1/src/decode.rs` (`decode_block`) — takes the DV, refuses
  var-tx by name, switches the luma `TxbSet` to the **inter** ext-tx sets
  (`txbset_for_inter`, since libaom's `is_inter_block` is true for intrabc,
  `blockd.h:373`), builds the Y/U/V block-copy prediction with
  `mc::predict_with_filter(.., Bilinear, ..)` off the *current* planes and
  ships it through the existing `PALETTE_PRED` per-block prediction override,
  and records the block into the mi grid.
- `crates/ec-av1/src/mvstack.rs:330,368` — **two real index bugs**:
  `sign_bias()` and `gm_mv()` both indexed `table[ref_frame - LAST_FRAME]`
  with no lower guard, so any query with `ref_frame == INTRA_FRAME` (0), which
  is exactly what an intrabc block does, panicked with
  `index out of bounds: the len is 7 but the index is 18446744073709551615`.
  Both now return the neutral value below `LAST_FRAME`. Same-shape sweep: those
  are the only two `- LAST_FRAME` table indexes in the crate.
- `crates/ec-av1/src/refusal_inventory.rs:55`, `gate_coverage.rs:36` — refusal
  string replaced; `enable-intrabc` stays in `NEVER_EXERCISED` with its reason
  rewritten to the true state.

## Fixture recipe (the round's most reusable finding)

Intra block copy is **impossible in a 64x64 frame**: a valid DV must clear
`INTRABC_DELAY_PIXELS` (256) plus one superblock, so a single-SB frame can
never contain one. Every 64x64 sweep this lane ran (3 sources x 6 cq) produced
zero intrabc blocks. A 512-wide frame with horizontally repeated content does:

```
ffmpeg -f lavfi -i "testsrc2=size=64x64:rate=1" -t 0.04 \
  -vf "tile=8x1,scale=512:64" -pix_fmt yuv420p -f rawvideo wide512.yuv
~/.cache/aom-oracle/build/aomenc --codec=av1 -w 512 -h 64 --i420 --limit=1 \
  --kf-max-dist=1 --cpu-used=1 --passes=1 --end-usage=q --cq-level=20 \
  --enable-intrabc=1 --tune-content=screen --enable-palette=0 \
  --enable-rect-partitions=0 --enable-ab-partitions=0 \
  --enable-1to4-partitions=0 --min-partition-size=16 --max-partition-size=32 \
  --obu -o w.obu wide512.yuv
cargo run -p ec-av1 --release --example decode_probe -- w.obu
```

`--enable-palette=0` and the partition switches are a **deviation from the
charter's gate recipe** (`--enable-palette=1`): with palette on, or with rect/AB
partitions available, the stream refuses on palette or on a sub-16x16 AB
partition *before* the first intrabc block, so it cannot isolate this lane's
capability at all. `cpu-used` must be <= 2 — at 3 and above aomenc's speed
features stop searching intrabc entirely (sweep: cpu 3/5/6/8 x cq 20/40, all 8
decode clean, i.e. no intrabc block at all).

## EVIDENCE

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/w.obu | recipe above, cpu-used 1 and 2 x cq 20/40, decode_probe before vs after | before: `REFUSED: ... a block that actually uses intrabc`; after: `REFUSED: ... an intrabc block under TxMode::Select` — the `use_intrabc` symbol, the DV read and the prediction all ran, 4/4 streams
EVIDENCE: same streams | sweep cpu-used 0,1,2,3,5,6,8 x cq 20,40,55 | intrabc only appears at cpu-used <= 2; cpu 0 stops earlier on an SB-level partition type

## Residue

- fix-now (next round, needs its own charter): `read_var_tx_size` — the
  `txfm_partition` CDF (21 contexts) does not exist anywhere in the crate, nor
  do the above/left txfm-context arrays it needs, nor does the residual loop
  handle a non-uniform TU tree. This is the single blocker between here and a
  pixel-exact intrabc gate.
- deferred(var-tx): the pixel-exact 8-bit/10-bit gate the charter asks for, and
  the `gate_coverage.rs` entry retirement — no stream can reach reconstruction
  until var-tx reads.
- accepted, documented as corner-cuts in source: only `decode_block` (square
  path) records into the intrabc mi grid; a frame mixing intrabc with rect or
  sub-8x8 leaves would predict off an incomplete grid (DV value only, never the
  parse). Chroma of an inter/intrabc block should inherit the co-located luma
  `tx_type` (libaom `av1_get_tx_type`), while `read_plane` still derives
  `DCT_DCT` for chroma — no symbol difference, but a suspected pixel difference
  the moment luma picks a non-DCT type. Both are prime suspects for the first
  pixel mismatch once var-tx lands.
