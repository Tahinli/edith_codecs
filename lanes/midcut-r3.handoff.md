# lane-midcut r3 handoff -- dedup done, the rect chroma WARP guard was the residue's bulk

Branch `lane-midcut`, worktree `edith_codecs-midcut`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-midcut`. Tip: `d93a149b`.
Reproduction of every cut here needs `EC_INTRA16X4_DECODE=1`.

## STEP 0 -- merge + DEDUP VERDICT: my override was redundant, dropped

`git merge 75b3a1d` (lane-t900 r3 tip, already containing main `243f125`):
clean auto-merge (decode.rs / stream.rs / decode_probe.rs).

lane-t900 r2 switched EVERY rect intra reader's own call site to
`tx_size_context_txfm_rect` under `INTRA_IN_INTER_MODE`, so after the merge my
`cde8893` branch INSIDE `tx_size_context_rect` was unreachable: all seven
remaining `tx_size_context_rect` calls sit in the `else` arm of exactly that
condition (grep `tx_size_context_txfm_rect|tx_size_context_rect` in decode.rs).
Removed it (`d41cbc51`); kept the counter by moving
`intra_rect_in_inter_txctx_override_hits` INTO `tx_size_context_txfm_rect`,
where it still counts only blocks whose CDF row the deblock-grid approximation
would have read differently. Kept the r1 interintra rect blend (`43e9909`).

Verification with t900's fix alone (no override anywhere): the 2-frame cut
`~/.cache/midcut-tmp/h2400.t2.obu`, decode-order PREFILT16 compare vs
instrumented aomdec -- frame 0 EXACT, frame 1 wrong-sample count 1317
(Y 115, U 342, V 860), i.e. bit-identical to what my own override produced.
No site needs both.

## STEP 1 -- ROOT CAUSE of the chroma residue (fixed, `8043ad54`)

`decode_inter_block` allocates a SQUARE prediction buffer and gated the warp
per plane on `chroma_side >= 8` -- the BUFFER's side. libaom
`av1_init_warp_params` bails per PLANE at `block_width < 8 || block_height < 8`
on the BLOCK's dims, so the 8x4 / 4x8 chroma of a 16x8 / 8x16 luma block keeps
the translational prediction. Every rect block with a local or global warp
warped its chroma where libaom does not. Guards are now `warp_chroma_ok =
write_chroma_w >= 8 && write_chroma_h >= 8` (5 sites: 4 compound + 1
single-ref) and `warp_luma_ok = write_w >= 8 && write_h >= 8` (3 luma sites).
Class: rectchroma2 / narrow-block-sharp-kernel (dims read off the enclosing
square buffer). The 8x8-leaf reader (`CHROMA_SIDE >= 8`, const-generic) was
already correct and is untouched.

Frame 1 of the 2-frame cut: **1317 -> 126 wrong samples (Y 115, U 11, V 0)**.

## RESIDUE STATE (126 samples, exact coordinates -- do not re-bisect)

Three luma clusters + one chroma cluster, all in decode-order frame 1 of
`~/.cache/midcut-tmp/h2400.t2.obu` (dumps kept at `~/.cache/midcut-tmp/d2/`):

| cluster | px | n | shape |
|---|---|---|---|
| SB(8,15) | y 552..559, x 960..969 | 18 | dithered +-1 |
| SB(9,7) | y 576..607, x 484..500 | 79 | a DIAGONAL +-1/+-2 band |
| SB(24,10) | y 1549..1552, x 647..658 | 18 | +1/+2 wedge |
| U | rows 800..803, cols 1216..1219 | 11 | -1..-2 |

Every one of those blocks is an INTRA block inside the INTER frame, and every
one is RECT (`EC_TRACE_MODE_STEP` at those mi):
* SB(8,15) -> mi(136,240) `fn=rect bw=16 bh=32`, `mode=9` (SMOOTH), tx_depth 0.
* SB(9,7) -> mi(144,120) `fn=rect bw=8 bh=16` mode=4 angle_y=+2 (directional),
  mi(144,122) `bw=8 bh=16` filter-intra, mi(144,124) `fn=sq side=16` mode=6
  angle_y=-2.
The diagonal band under a directional mode plus a dithered edge under SMOOTH is
the signature of ONE wrong edge sample, not a wrong predictor: suspect the
edge availability / reach for a RECT intra block inside an inter frame
(`n_top`/`n_left`, has-top-right, or the intra edge filter strength/upsample
computed from the square side rather than `bw x bh` -- `intra.rs:483
directional`, `intra_edge_filter_strength`, `use_intra_edge_upsample`).
Classes to check first: reach-is-per-transform-unit, visit-order-changes-
availability, context-read-from-one-cell.

## STEP 2 -- gate/counter wiring state

* `intra_rect_in_inter_txctx_override_hits` compiles and counts in
  `tx_size_context_txfm_rect`; it is **not yet printed by decode_probe and not
  yet asserted by any gate** (turn cap). Next: print it next to
  `interintra_rect:` in `crates/ec-av1/examples/decode_probe.rs:46` and assert a
  positive delta in `intra_rect_in_inter_split_tx_gate` / `intra_rect_in_inter_gate`.
* `a_real_aomenc_stream_with_interintra_decodes_pixel_exact` was RED on this
  branch: its recipe spells `--enable-rect-partitions=0 --min-partition-size=32`,
  so r1's `interintra_rect_hits` delta assert was UNSATISFIABLE (40 pixel-exact
  matches, counter 0). Fixed with a RECT ARM (`d93a149b`) that re-encodes the
  same source with `--enable-rect-partitions=1 --min-partition-size=8
  --max-partition-size=64` APPENDED (last flag wins) and compares every
  decode-order frame vs aomdec; seed 46's arm fires 4 rect interintra blocks.
  Gate green (1 passed, 11.7 s).
* NO fixture pinned: frame 1 is still 126 samples off, so
  `crates/ec-av1/fixtures/hg_midcut_arf_witness.obu` and its decode-order gate
  must wait for exactness. No refusal lifted; `refusal_inventory.rs` /
  `gate_coverage.rs` untouched.

## Gates run this round (per-name, systemd units)

`$HOME/.cache/midcut-gates-r3.log` (at `d41cbc51`): `hidden_arf` 1 passed;
`rect64_corner_tus` 1 passed; `interintra` FAILED (the vacuous assert above,
now fixed); `1to4` / `refusal_inventory` / `gate_coverage` were still running at
the cap -- RE-RUN THEM at the tip. Full suite NOT run (coordinator: no suite).

## 24-offset decode-order table (`~/.cache/midcut-tmp/cen/after_r3.tsv`)

Written by `~/.cache/midcut-tmp/cen/run_r3.sh` (adds `EC_INTRA16X4_DECODE=1`,
which the r2 script lacked). FINISHED: 24 offsets, frame 0 EXACT at all 24,
frame 1 EXACT at **2** (0 s and 5100 s -- 5100 s went 2300495 -> 0), 12 of 24
offsets improved on frame 1, the rest unchanged (their frame-1 defect is not
warp-shaped). Best movers: 1800 s 11612125 -> 1151, 2400 s 11010633 -> 126,
3300 s 10663109 -> 27, 3900 s 4665360 -> 522 (its frame 3 is now 0). One offset
(1500 s) still REFUSES on the Golomb tail. Nine offsets still carry a
multi-million-sample frame among f1..f3 (300/900/2100/6000/6300/6600 ...), i.e.
at least one more entropy-level defect remains off this cut.

## Artifacts

`~/.cache/midcut-tmp/`: `h2400.t2.obu` (2-frame cut, 4K 10-bit),
`d2/{aom,our}.f{0,1}` (PREFILT16 dumps both decoders, at the tip),
`m5.txt`/`m6.txt` (our `EC_TRACE_MODE`+`EC_TRACE_MODE_STEP` at the residue mi),
`cen/run_r3.sh`, `cen/after_r3.tsv`.
