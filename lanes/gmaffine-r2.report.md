# lane-gmaffine r2 — item (1) CLOSED green, item (2) root-caused (not what the charter guessed), item (3) blocked on lane-inter8

Commit: `0a5cbe3` on `lane-gmaffine` (on top of r1's `65ef3f5`). Not rebased onto main
(charter order). Suite: **268 passed, 2 failed, 23 ignored** (`cargo test -p ec-av1
--release --lib`, `EC_AV1_REQUIRE_AOMENC=1`) — r1 was 266/4/23; the two remaining
failures are the two RED 8x8-leaf motion gates, everything else green, no regression.

## (1) obmc_8x8 V-plane residual — FIXED, gate GREEN
Root cause exactly where the charter pointed: libaom `av1_skip_u4x4_pred_in_obmc`
(`~/.cache/aom-oracle/src/av1/common/reconinter.c:820`, `DISABLE_CHROMA_U8X8_OBMC 0`)
returns `dir == 0` when the block's CHROMA plane size is `BLOCK_4X4/4X8/8X4` — i.e. OBMC
on chroma is ONE-SIDED below 16px luma: the ABOVE pass (`dir==0`) skips U/V entirely, the
LEFT pass (`dir==1`) still blends them. We blended chroma in both passes.
Fix: `crates/ec-av1/src/decode.rs:8419` (`let chroma_above = write_w >= 16 && write_h >= 16;`)
guarding the above pass's U/V blends. Luma is unaffected at every size, which is why
r1 saw luma+U exact and only V off by ~2.
Also hardened: the gate's terminal "never fired" branch was an `eprintln!("SKIP …")`
(class `gate-skips-on-its-own-failure`); it is now a `panic!` — `crates/ec-av1/src/stream.rs:4401`.

EVIDENCE: `cargo test -p ec-av1 --release --lib -j3 -- obmc_8x8 --nocapture` (EC_AV1_REQUIRE_AOMENC=1)
| real aomenc `--enable-obmc=1 --min-partition-size=8` stream, seed 53, 24 frames, decoded vs ffmpeg
| `a_real_aomenc_stream_with_obmc_8x8_decodes_pixel_exact FIRING seed 53: 8x8 obmc hits 4`, all 24 frames Y+U+V byte-equal, test result ok.

## (2) warp gate's `switchable_interp` crash — root cause was NOT a desync
`RUST_BACKTRACE=1` put the panic at `obmc_blend -> neighbour_filter ->
from_switchable_symbol`, and an instrumented print pinned it: block `mi_row=0 mi_col=2`,
LEFT pass, `Neighbours` slot 0 holding `[3, 3]`. That is the enclosing 16x16's coarse
filter slot — written only AFTER all four 8x8 leaves decode — so an 8x8 leaf that
OBMC-blends its own SIBLING leaf reads the "intra/unset" sentinel and panics.
libaom takes the OBMC neighbour's filter from the neighbour mbmi's own `interp_filters`
(`av1_setup_build_prediction_by_{above,left}_pred`), which is mi-granular. Ported:
- `crates/ec-av1/src/mvstack.rs:180` — `MiGrid::filters` (`[u8;2]` per mi, `[3,3]` = unset)
  + `set_filter`/`filter`.
- `crates/ec-av1/src/decode.rs` — writers at the 16x16+ single-ref `grid.set` loop and at
  the 8x8 leaf; reader in `obmc_blend` via `grid_or_slot` (mi record preferred, the coarse
  `Neighbours` slot kept as fallback for paths that record none, e.g. compound).
- `decode_inter_block8` now RETURNS its real filter symbols and reference; the caller's
  post-leaf `record_inter` used `[3, 3]` and a hardcoded `LAST_FRAME` for the whole 16x16
  (r1 corner-cut) — both were feeding wrong switchable_interp/ref contexts to the NEXT
  block, and the leaf's `fill_lf_grid` ref id was likewise always LAST.
Charter hypothesis checked and REJECTED with the source: `read_inter_block_mode_info`
(`decodemv.c:1525/1562/1621`) reads interintra → motion_mode → `read_mb_interp_filter`,
which is the order both our leaves already use; `av1_is_interp_needed`'s three suppressors
(skip_mode, WARPED_CAUSAL, `is_nontrans_global_motion` incl. IDENTITY) match our
`gm_nontrans || warped_selected || skip_mode` argument. No symbol-order defect here.

EVIDENCE: `cargo test … -- a_real_warped_causal_8x8 --nocapture` before/after
| same 6 aomenc encodes | before: `panicked at mc.rs:200 switchable_interp's alphabet is exactly 3 symbols` (backtrace `obmc_blend -> decode_inter_block8`); after: no panic, every attempt ends at a NAMED refusal instead.

## (3) both new gates still RED — the blocker is an 8x8-leaf ENTROPY DESYNC, deferred to lane-inter8
With the panic gone, the sweeps end like this (`EC_AV1_REQUIRE_AOMENC=1`,
`-- _8x8_leaf --nocapture`, `--min/--max-partition-size=8`):
- globalmv gate: `a partition below 8x8` (2), `a non-DC chroma mode on an 8x8 inter-frame leaf` (3), `a nonzero angle delta` (1 at maxpart=32)
- warp gate: `an inter partition below 16x16 other than a clean split into four 8x8 leaves` (4), `a non-skip rectangular (HORZ/VERT/HORZ_B) strip …`, `an INTER 32x32 partition type … (value=8)`
Every one of those recipes passes `--enable-rect-partitions=0 --enable-ab-partitions=0
--enable-1to4-partitions=0 --enable-angle-delta=0 --enable-directional-intra=0
--min-partition-size=8` to aomenc, so the encoder CANNOT have written a sub-8x8 partition,
a rect strip or a nonzero angle delta: these are misread symbols, i.e. a real entropy
desync in/around the 8x8 leaf, surfacing as refusal strings (class
`refusal-names-a-correlate`). The same 8x8 leaf decodes 24 frames pixel-exact in the obmc
gate (gradients source, `--max-partition-size=32`, warp off, gm default), so the desync is
config/content-dependent, not "the leaf is broken".
Recipe search knobs added so the next round needs no rebuild: `EC_8X8_MINPART`,
`EC_8X8_MAXPART` (defaults 8/8, i.e. unchanged) — `crates/ec-av1/src/stream.rs:5449`.
Neither gate ever reaches a GLOBALMV or WARPED_CAUSAL 8x8 leaf; both die earlier, so
nothing about r1's motion code is disproved (or proved) by them.

## DEVIATION from the charter (named, with reason)
The charter's fallback said: if a gate cannot go green, RESTORE the refusal strings r1
removed. I did NOT. Reasons: (a) restoring them puts a refusal BEFORE the code it guards,
making r1's GLOBALMV/WARPED_CAUSAL 8x8 implementation dead code (class
`refusal-short-circuits-its-own-code`); (b) the two gates die on partition/chroma refusals
that belong to lane-inter8's cluster and never reach the motion code, so the restored
refusals would not be what is failing; (c) `decode_inter_block8` is being edited
concurrently by lane-inter8 and a refusal re-insertion there is a merge conflict for no
verification gain; (d) a GLOBALMV-at-8x8 refusal would likely turn the now-GREEN obmc_8x8
gate RED (its streams do reach 8x8 leaves with global motion enabled by default).
If the orchestrator wants the r1 lift out, revert `65ef3f5`'s `refusal_inventory.rs` hunk
plus its two `decode.rs` refusal deletions; **commit `0a5cbe3` (this round) is independent
of that lift** — the chroma-OBMC fix, the mi-granular filter record and the leaf
filter/ref plumbing are all gated by `a_real_aomenc_stream_with_obmc_8x8_decodes_pixel_exact`
and merge on their own.

## Residue
- fix-now next round, deferred(lane-inter8): the 8x8-leaf entropy desync above. Attack:
  `EC_AV1_GATE_DUMP` the first refusing stream, decode it under `EC_TRACE_MODE` against
  instrumented aomdec, compare msac RANGE element by element from the first inter frame's
  first 8x8 leaf (never `tell()`).
- deferred(lane-hbdinter): `warp::warp_affine` hardcodes `const BD: i32 = 8;` — every
  10-bit warp/global-warp block is predicted at the wrong bit depth. Not touched here.
- deferred(own lane): compound `GLOBAL_GLOBALMV` never warps — `decode_inter_block`'s
  compound branch builds both taps translationally through `mc::predict_compound_intermediate`
  and there is NO refusal for it, so a compound global block with a ROTZOOM/AFFINE model is
  silently wrong. Unblocked by a compound warp path (`av1_warp_plane` into the i32
  intermediate with `conv_params`).
- accepted corner-cut: `Neighbours` stays 16px-granular; the leaf loop records the LAST
  INTER leaf's filter/ref into the shared slot. Ceiling: a 16x16 whose 8x8 siblings choose
  different filters/refs gives the NEXT block one sibling's context instead of the
  bordering one's. Upgrade: mi-granular `above_filter`/`above_ref` arrays (the `MiGrid`
  filter record added this round is half of that already).
