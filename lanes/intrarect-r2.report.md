# lane-intrarect r2 — the intra rect strip's txb_skip context

Branch `lane-intrarect` off `lane-rectsplitx` cbf8ffd (r1 = 798ea0c). **Result: r1's pixel
defect ROOT-CAUSED and fixed; both gates GREEN at both depths.**

## Root cause (one line)

`decode_rect_split`'s RECT transform-unit arm read `txb_skip` from the neighbour-magnitude
table for a unit that covers its whole block. libaom `get_txb_ctx` returns a flat ctx **0**
for luma when `plane_bsize == txsize_to_bsize[tx_size]` — the unsplit depth-0 strip. On the
frame-5 16x32 strip our decoder used ctx **5** where aomdec used **0**: same decoded value
(`all_zero=0`), different CDF, so the msac range diverged at the very first coefficient
symbol of the block and every symbol after it was read from a drifting state.

`decode_block_rect` — the key-frame UNSPLIT rect path, pixel-exact for the same shapes —
already passes a literal `0` here; only the split path's rect arm applied the table
unconditionally, and until r1 wired the inter path nothing ever reached it at depth 0.

## What changed

- `crates/ec-av1/src/decode.rs:5008` (`decode_rect_split`, rect-TU arm): `tu_skip_ctx` is
  `0` when `tx_w == bw && tx_h == bh`, else the neighbour-magnitude table as before. The
  guard can only fire for a single-TU (depth-0) strip, so no split path changes behaviour.
- `crates/ec-av1/src/decode.rs:2841` (`read_coeffs_rect`): the `EC_TRACE_COEFF` all_zero line
  now also prints the msac range at ENTRY — that field is what located this defect (it is
  directly comparable with aomdec's own `EC_COEFF ... rng=` line, which prints pre-read).
- `crates/ec-av1/src/stream.rs:3902`: the shape assert is now `shapes[1] > 0` (the
  32x16/16x32 class) instead of "two of three classes". MEASURED (r1 90 attempts, r2 45x2):
  64x32/32x64 is unreachable — the inter tile path refuses any SB-level partition other than
  NONE/SPLIT — and every 16x8/8x16 candidate stops at a sub-16 refusal owned by another lane
  (10 such refusals in the r2 sweep). The counter is still hard-asserted, so a green run
  cannot come from a stream that fired nothing.

## Evidence

Repro (charter STEP 1), 8-bit seed 69 = attempt 27 (`--cpu-used=2 --cq-level=12`,
`mandelbrot=size=96x96:start_scale=3.38:end_scale=0.004:end_pts=8`, min-partition-size=16,
tx-size-search=0):

EVIDENCE: scratchpad `o_pre.f5` vs `m_pre.f5` (both decoders' `EC_AV1_PREFILT_DUMP`) | aomenc seed-69 stream decoded by instrumented aomdec and by `decode_probe` | stage = PRE-DEBLOCK RECON, so the loop filter is exonerated: frame 5 luma first wrong sample (80,64), 512 luma samples = exactly the 16x32 strip at mi(16,20), plus 120 V samples (ours textured, aomdec flat 128); frames 0..4, 6, 7 byte-identical.

EVIDENCE: scratchpad `o_tr8.txt` vs `m_tr.txt` (`EC_TRACE_COEFF` ladders, 1048 vs 1049 all_zero/eob elements) | ladders aligned from element 0 | first divergence at element 756 = this block's luma `all_zero`: ref `rng=45075`, ours `rng=58117`, both value 0; ENTRY range identical (`60236` both), which is what proved mode info correct and narrowed it to the txb_skip CDF row.

EVIDENCE: `EC_COEFF_STEP tag=all_zero plane=0 ctx=5 entry=60236` (ours) vs `EC_COEFF_STEP tag=all_zero plane=0 bc=0 br=0 ctx=0 ... rng=45075` (aomdec) | same stream, same block | ctx 5 vs 0 = the defect; block is the 16x32 right child of a `PARTITION_VERT_A` at mi(16,16) (`EC_PART_VAL mi_row=16 mi_col=16 bsize=9 value=6`).

EVIDENCE: scratchpad `o_pre.f*` vs `m_pre.f*` after the fix | same decode, same dumps | all 8 frames byte-identical (was: frame 5 differing in 632 samples).

## Gates

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib intra_rect_strip -j3 -- --nocapture`
-> `test result: ok. 2 passed; 0 failed` in 12.20s
(`$HOME/.cache/intrarect-gate-r2.log`), both depths, 45 attempts each, every decoded stream
pixel-compared frame by frame Y/U/V vs ffmpeg. Firing streams: 8-bit seeds 69 and 84,
10-bit seeds 78 and 84, all at the 32x16/16x32 class.

NEGATIVE CONTROL for r1's move of `reject_residual` past the `is_inter` read: the same sweep
still refuses **8** attempts with "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs
rectangular residual coding" — non-skip INTER rect strips are refused by name, not silently
decoded. Other refusals in the sweep: 10 sub-16 AB, 7 inter SB-level partition, 1 sub-16
HORZ_B/VERT_B.

Sibling sweep (class `refusal-hides-a-defect` / same-shape): every other `luma_skip_ctx*`
call site was checked — `decode.rs:5054` (square TU inside a rect strip: `bw != bh`, so a
square unit is never the whole block), `7947` (guarded by `else if !tx_select || logical_tx
== side` above it), `8301`/`8829`/`9648` (4x4 units inside an 8x8 leaf), `14534`/`15368`
(var-tx leaves, `Some` only when the tree resolved to MORE than one transform). None can
reach the unit == block case. One defect, one site.

Suite: `systemd-run --user --unit=intrarect-suite-r2 ... cargo test -p ec-av1 --lib -j3` ->
`test result: ok. 337 passed; 0 failed; 29 ignored; 0 measured; 0 filtered out; finished in
257.15s` (`$HOME/.cache/intrarect-suite-r2.log`) -- r1's two RED gates are the two that
flipped, nothing else moved.

## Residue

- deferred(orchestrator merge) — **this branch is NOT rebased onto main**. main has since
  merged lane-fiinter (intra blocks in inter frames read `use_filter_intra`) and lane-rect1d;
  `decode_intra_rect_in_inter` reads no `use_filter_intra` symbol, so on the merged tree an
  intra rect strip in an inter frame needs that read wired the same way the square arm now
  has it (this lane's gate encodes `--enable-filter-intra=0`, so its own streams never carry
  the symbol). Rebasing was not attempted here: r1's whole diff lives in the same
  `decode.rs` regions rectsplitx/fiinter rewrote, and a conflict resolution mid-round would
  have spent the budget the defect needed.
- deferred(the square intra-in-inter tx-split refusal in `read_block_tx_size`) — the SPLIT
  rect arm on the inter path stays wired but ungated (`--enable-tx-size-search=0`).
- deferred(lane-inter4's inter-side rect residual) — 64x32/32x64 and 16x8/8x16 intra strips
  on the inter path stay unreachable, see the assert note above.
- accepted — 1:4 strips refuse by name on this path.
