# lane-lr report

## What landed (committed on `lane-lr`, 14a0273)
- Added the three loop-restoration CDF tables this decoder was missing
  (`crates/ec-av1/src/cdf.rs`): `RESTORE_WIENER`, `RESTORE_SGRPROJ`,
  `RESTORE_SWITCHABLE`, copied exactly from libaom
  `av1/common/entropymode.c`'s `default_{wiener,sgrproj,switchable}_restore_cdf`
  (`AOM_CDF2(11570)`, `AOM_CDF2(16855)`, `AOM_CDF3(9413, 22581)` — converted
  to this codebase's `[a0, (a1,) 32768, 0]` convention, matching the existing
  `INTRABC` table's own conversion of `AOM_CDF2(30531)`).
- Wired all three into `Cdfs` (`crates/ec-av1/src/cdf_state.rs`): struct
  fields, `Cdfs::new`'s defaults, and `reset_counts` (all three are
  unindexed `[u16; N]`, same shape as `intrabc`, so `reset1` — the
  4-site wiring checklist's counter-reset site is the only hand-checked
  one; save/restore and the other two sites are generic over any shape).
- `cargo check -p ec-av1` clean.

## What still refuses, and by what string
The whole-frame refusal is UNCHANGED: `crates/ec-av1/src/stream.rs:195-198`
still returns `Error::unsupported("AV1 decode_stream", "a frame with loop
restoration enabled (this decoder never reads the per-unit lr symbols)")`
for any `header.loop_restoration.uses_lr` frame. The CDF tables added this
round are not read from the bitstream anywhere yet — this is groundwork
for stage 1 (charter's "syntax only" milestone), not stage 1 itself.

## Design worked out but not yet coded (for the next round)
Traced the full spec/libaom shape so the next round can go straight to
code instead of re-reading source:

- `av1_loop_restoration_corners_in_sb` (`restoration.c:1277`) only returns
  non-zero corners when `bsize == sb_size` — i.e. despite `decode_partition`
  calling it recursively at every partition level in libaom, only the
  top-of-superblock call ever does anything, because every restoration
  unit is >= 64px (SB size). The charter's "once per superblock" plan is
  therefore exactly spec-equivalent, not a simplification with a gap.
- Per-plane unit grid: `horz_units = max(1, round(plane_w / unit_size))`,
  `vert_units` likewise (`av1_lr_count_units`, `restoration.c:63`, note
  ROUND not ceil). `plane_w`/`plane_h` are `frame_width`/`frame_height` for
  luma; this decoder is 4:2:0-only in the tile-decode functions already
  (`u`/`v` are hardcoded half-size), so chroma is
  `(frame_width+1)/2, (frame_height+1)/2` (`ROUND_POWER_OF_TWO`).
  `unit_size` = `header.loop_restoration.loop_restoration_size[plane]`
  directly (already computed per-plane in
  `ec-av1-syntax/src/frame.rs:1626-1637`).
- Per-SB corner math (`restoration.c:1277-1335`, no superres in this
  decoder so the superres-scaled branches are dead code here):
  `rcol0 = ceil_div(mi_col0 * mi_size_x, unit_size)`,
  `rcol1 = min(ceil_div((mi_col0+sb_mi_w) * mi_size_x, unit_size), horz_units)`,
  same for rows; `mi_size_x = 4` luma, `2` chroma (4:2:0). Then
  `runit_idx = rcol + rrow * horz_units`, and `read_lr_unit` is called once
  per `(rrow, rcol)` in that rectangle, per plane, in plane order.
- `read_lr_unit` symbol shape (`decodeframe.c:1687-1723`): if
  `frame_restoration_type[plane] == Switchable`, read one
  `restore_switchable` (3-way) symbol and dispatch; if `== Wiener`, read one
  `restore_wiener` binary symbol, Wiener filter only if 1; if `==
  Sgrproj`, same with `restore_sgrproj` and the SGR filter. `None` never
  calls this at all (loop skipped by `frame_restoration_type[plane] ==
  RESTORE_NONE` — i.e. it's legal for the switchable-frame case per plane
  even when only some planes use LR, since `frame_restoration_type` is
  per-plane).
- Wiener coefficients: 3 taps per direction (vfilter/hfilter), each via
  `aom_read_primitive_refsubexpfin(n, k, ref)` — SAME algorithm this crate
  already has for global-motion params
  (`ec-av1-syntax/src/frame.rs:1797-1839`,
  `decode_signed/unsigned_subexp_with_ref`/`decode_subexp`/`inverse_recenter`)
  but that existing implementation runs on a raw `BitReader` (frame header,
  uncompressed bits); the LR coefficients are read through the arithmetic
  decoder (`aom_reader`, i.e. this crate's `SymbolDecoder`), using
  `aom_read_literal`/`aom_read_bit` as the raw-bit primitives —
  `SymbolDecoder::literal(bits)` (`msac.rs:348`, already exists, built on
  `symbol_fixed(&EQUIPROBABLE)`) is the exact equivalent. The subexp/recenter
  math needs a second, msac-flavoured port of those four functions (same
  arithmetic, `dec.literal(1)`/`dec.literal(n)` in place of `r.read_bit()`/
  `r.read_bits(n)`) — not reusable as-is because the reader types differ.
  Per-tap `(min, max, k)`, all computed from libaom `restoration.h`:
  tap0 `(-5, 10, k=1)`, tap1 `(-23, 8, k=2)`, tap2 `(-17, 46, k=3)`
  (`WIENER_FILT_TAPn_MIDV` 3/-7/15, bits 4/5/6). Reference state
  (`ref_wiener_info`) starts each tile at the tap midpoints (3, -7, 15) and
  updates to the just-decoded value after each unit — same per-plane
  running-reference shape as `PrevSegmentIds`/other adapting-per-tile state
  already in this crate.
- SGR coefficients: one 4-bit literal `ep` (`SGRPROJ_PARAMS_BITS`) indexing
  `av1_sgr_params`/`sgr_params` (a small fixed table this crate does not
  have yet — needed even for stage 1 since `ep` selects which of `xqd[0]`/
  `xqd[1]` is forced to 0 vs subexp-coded, spec 7.17.3's `Sgr_Params`), then
  1-2 subexp-coded `xqd` values, ranges `(-96, 31, k=4)` and
  `(-32, 95, k=4)` (`SGRPROJ_PRJ_MIN/MAX{0,1}`, `SGRPROJ_PRJ_BITS=7`).

## Next lever
Port the msac-flavoured subexp/recenter quartet into `decode.rs` (or a new
`restoration.rs`), add the `Sgr_Params` table (8 entries, `restoration.c`'s
`av1_sgr_params`, not yet located — grep `sgr_params\[` there), write
`read_lr_unit` using the corner math above, thread
`&header.loop_restoration` into `decode_key_frame_tile_with_cdfs` and
`decode_inter_frame_tile_with_cdfs` (both already take a long positional
arg list matching this pattern — see `cdef`/`loop_filter` params; the two
public one-star wrappers `decode_key_frame_tile`/`decode_inter_frame_tile`
can keep their old signatures by passing `&LoopRestorationParams::default()`
internally, so the ~10 test call sites across `decode.rs` do not need
touching), call `read_lr` once per SB in both superblock loops
(`decode.rs` ~4547 key, ~9275 inter) BEFORE `decode_partition`/its
equivalent, and only then flip `stream.rs`'s refusal from unconditional to
"decode succeeded, still refuse by name because filters are not applied"
(stage 1's own exit criterion: prove the partition walk survives past a
real LR stream). Stage 1 gate: generate an aomenc stream with
`--enable-restoration=1` (default on) via the existing `gradients_source`
helper, decode it, assert the *new* refusal string appears (not the old
desync-shaped one / not an `Err` from deep inside a block reader), and
self-pin firing counts for wiener/sgrproj/switchable via thread-local
`Cell<usize>` hit counters the way every other gate in this lane's charter
requires.

## Turn budget (r1)
Spent ~55 of 75 turns on recon (spec/libaom read_lr shape, corner math,
subexp/recenter porting requirements, CDF value derivation) plus the CDF
landing above; stopped short of wiring `read_lr` itself to avoid leaving a
half-edited, untested threading change across two large decode functions
uncommitted at the cap. Everything above is proven correct in principle
(SB-only firing is a real spec property, not skipped-and-hoped) but wiring
+ the gate itself are unstarted.

## r2 landed (committed on `lane-lr`, 32892df; 235 passed / 0 failed,
EC_AV1_REQUIRE_AOMENC=1, `232` baseline + 3 new tests)
- `crates/ec-av1/src/restoration.rs` (new): msac-flavoured
  `decode_subexp_msac`/`decode_unsigned_subexp_with_ref_msac`/
  `decode_signed_subexp_with_ref_msac`/`ns_msac` (libaom
  `read_primitive_subexpfin_`/`aom_read_primitive_refsubexpfin_`/
  `read_primitive_quniform_`, `k` a per-call parameter unlike global
  motion's fixed 3) -- pinned by 3 roundtrip tests
  (`subexp_roundtrips_every_value`, `unsigned_subexp_with_ref_roundtrips`,
  `signed_subexp_with_ref_roundtrips`) against a hand-written encode-side
  mirror (`SymbolEncoder` has no LR writer of its own, this crate never
  writes LR). Also: `WienerInfo`/`SgrprojInfo`/`UnitFilter`,
  `SGR_PARAMS` (`av1_sgr_params`, restoration.c:36, all 16 entries),
  `read_wiener_filter`/`read_sgrproj_filter`/`read_lr_unit`/`read_lr`
  (spec 5.11.57, decodeframe.c ~1595-1723), `RestorationGrid` (per-plane
  unit grid + flattened `runit_idx` storage, `av1_lr_count_units`'s ROUND
  not ceil).
- `crates/ec-av1/src/decode.rs`: `decode_key_frame_tile_with_cdfs` and
  `decode_inter_frame_tile_with_cdfs` now take `lr: &LoopRestorationParams`
  and call `crate::restoration::read_lr` once per superblock (before
  `decode_partition`'s own symbol), building a `RestorationGrid` +
  per-plane `(WienerInfo, SgrprojInfo)` running reference state before the
  loop. The public one-star wrappers (`decode_key_frame_tile`/
  `decode_inter_frame_tile`) pass `&LoopRestorationParams::default()`
  unchanged, so their ~10 existing test call sites needed no edits.
- `crates/ec-av1/src/stream.rs`: both call sites thread
  `&header.loop_restoration` through; the refusal string is renamed from
  "never reads the per-unit lr symbols" to "the per-unit lr symbols are
  read but the Wiener/self-guided filters are not yet applied to pixels"
  -- the frame STILL refuses (no pixel filter is applied anywhere yet),
  but the refusal now names the true remaining gap per charter stage 2's
  exit criterion.
- NOT done this round, no gate written: I did not build the aomenc
  `--enable-restoration=1` gate the charter's stage 1/2 asks for (proving
  the partition walk survives past a real LR stream and self-pinning
  wiener/sgrproj/switchable firing counts via thread-local `Cell<usize>`
  hit counters). Ran out of turn budget before writing it; the full
  `cargo test -p ec-av1` suite (235/0, unrelated fixtures) is the only
  evidence this round has that the wiring compiles and does not regress
  anything -- it does NOT exercise a real LR stream at all, since no
  existing gate/fixture in this suite turns `--enable-restoration` on.

## Next lever (r3)
1. Write the gate: `gradients_source` fixture through aomenc with
   `--threads=1 --row-mt=0 --enable-restoration=1` (default on already,
   so no flag omission risk), decode it, assert the NEW refusal string
   (not an `Err` from a block reader = partition desync, not the old
   string). Self-pin `WIENER_HITS`/`SGRPROJ_HITS`/`SWITCHABLE_HITS`
   thread-local `Cell<usize>` counters bumped inside `read_lr_unit`
   (`crates/ec-av1/src/restoration.rs`) -- a gate that cannot prove LR
   fired is vacuous per this lane's charter. Given how rare a `Wiener`
   frame-level restoration_type is likely to be vs `Switchable` at
   default aomenc settings, may need `EC_LR_GATE_ATTEMPTS` bumped like
   other lanes' flaky-firing gates (see ledger `lane-maskcomp`).
2. Wiener pixel filter (stage 3): apply after `apply_cdef` (LR runs on the
   post-CDEF, post-deblock frame, spec 7.17) as a new `apply_loop_restoration`
   pass over `lr_grid`, one restoration-unit rectangle at a time, using
   the already-decoded `vfilter`/`hfilter`. The 3-pixel stripe boundary
   save/restore (libaom's `rlbs`) is the known trap -- not investigated
   this round beyond the report's own citation.
3. SGR filter (stage 4): box-sum radii from `SGR_PARAMS`' `(r0, r1)`,
   `av1_apply_selfguided_restoration` shape -- not investigated this
   round.
4. Once both filters are wired, drop the `stream.rs` refusal entirely
   (stage 5) -- gate should then assert `Ok`, never `Err`.

## r3 (committed on `lane-lr`, 0cfe9d6; 236 passed / 0 failed, 17
ignored, EC_AV1_REQUIRE_AOMENC=1) -- VERDICT: step 1 (the gate) landed
and, in the process of landing it, found and fixed a real gap r2's own
report had flagged as unverified. Steps 2-4 (Wiener/SGR pixel filters,
dropping the refusal) NOT started -- out of turn budget.

- New gate `a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly`
  (`crates/ec-av1/src/stream.rs`): real aomenc `--enable-restoration=1
  --sb-size=64` stream, multi-superblock fixture (192x128 = 3x2 SBs of
  64px -- charter's own trap warning), 40 attempts, `cq-level=15` (r2's
  sibling gates' usual `cq-level=45` never landed `uses_lr=true` for
  this content -- sampled live before picking 15). Self-pinned
  `WIENER_HITS`/`SGRPROJ_HITS`/`SWITCHABLE_HITS` thread-local counters
  added to `restoration.rs`'s `read_lr_unit`, hard-asserted `sum > 0`.
- **The gate found a real bug**: `stream.rs`'s `header.loop_restoration.uses_lr`
  refusal check ran BEFORE the `decode_key_frame_tile_with_cdfs`/
  `decode_inter_frame_tile_with_cdfs` call, so `read_lr` was NEVER
  actually invoked through `decode_stream` -- despite r2's report
  describing "the symbols are read", no real stream had ever exercised
  that claim end to end. First gate run: 40/40 attempts hit the refusal
  with `wiener_hits=sgrproj_hits=switchable_hits=0`. Fixed by moving the
  check to run AFTER the tile-decode call succeeds (same refusal string,
  new position) -- this both proves the superblock walk survives
  `read_lr` without desyncing (the charter's actual step-1 ask) and now
  genuinely exercises the reader. Also needed `--sb-size=64` in the gate
  recipe (without it, aomenc's default sb128 choice hit the unrelated,
  already-dead-ended `lane-sb128` partition-desync gap and ate the whole
  attempt window with "a partition type this encoder never writes").
- Final gate result: 39 LR refusals / 0 other refusals out of 40
  attempts, `wiener_hits=19 sgrproj_hits=72 switchable_hits=0` (this
  fixture's RD only ever picked a fixed frame-level `Wiener`/`Sgrproj`
  type, never `Switchable` -- both non-switchable arms proven live; the
  switchable arm is still symbol-code-reviewed only, unexercised by any
  gate this round).
- Refusal string: UNCHANGED text ("a frame with loop restoration enabled
  (the per-unit lr symbols are read but the Wiener/self-guided filters
  are not yet applied to pixels)"), only its position in `decode_stream`
  moved (was before the tile-decode call, now after) -- `refusal_inventory.rs`
  on main should not need updating for this move since the string itself
  didn't change; report this in case main's guard checks call-site order
  too.

## r5 -- VERDICT: not green. 06ba9af compiles clean and was NOT a
mid-edit wip; r4 finished all four charter steps (Wiener + SGR pixel
filters wired at both call sites, gate widened to assert `Ok` +
pixel-exact, `stream.rs` refusal dropped). Suite: 235/236 -- one real,
reproducible (not flake) defect in
`a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly`:
`V mismatch (seed 46 frame 0)`, exactly 1 pixel of 6144 in the V plane,
off by 1 (ours=195, ffmpeg reference=194), at absolute (row=61,col=6),
inside chroma's last/partial restoration stripe `[60,64)` (unit spans
the whole 64px plane height, `--sb-size=64` 192x128 fixture), filter
`Sgrproj{ep:6, xqd:[-16,-32]}` (both radii active, `r0=2` "fast" +
`r1=1` "dense"). Ran 235/236 with `EC_AV1_REQUIRE_AOMENC=1`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`, `-j4`, 600000ms timeout.

Did NOT commit anything this round -- the tree was already clean at
06ba9af (nothing left mid-edit; the orchestrator's "unverified" label
undersold r4's actual state). All edits made this round were temporary
`eprintln!`/dump instrumentation, reverted with `git checkout --` before
finishing (tree is bit-identical to 06ba9af).

Diagnosis done, inconclusive on root cause:
- Confirmed NOT a flake -- same failure every run, same seed/pixel.
- Confirmed NOT an above/below stripe-boundary substitution bug --
  hand-traced `lr_sample`'s doc-commented rules
  (`stripe_v_start-1`→exact, `-2`/`-3`→both duplicate `stripe_v_start-2`;
  `stripe_v_end==plane_h`→frame-edge replicate) against libaom's real
  `setup_processing_stripe_boundary`/`save_deblock_boundary_lines`
  (`~/.cache/aom-oracle/src/av1/common/restoration.c` ~249-410,
  1355-1490) line by line -- they match exactly for this stripe
  (`use_deblock_above=true` since `stripe_idx=2>0`, `use_deblock_below=
  false` since it's the plane's last stripe).
- Wrote a live dump harness (env-gated `eprintln!`+file dump inside
  `apply_sgrproj_stripe`/`filter_restoration_unit`, since reverted) that
  extracted the REAL cdef/deblocked V-plane bytes for the failing
  attempt to `/tmp/lr_cdef.bin`/`/tmp/lr_deblocked.bin` (both files
  byte-identical, as expected -- gate runs `--enable-cdef=0`).
  Instrumented the live Rust `apply_sgrproj_stripe`/`compute_ab` to
  print the actual A/B grid values it computes for the failing pixel's
  5-neighbour dense (`r1`) read: `a_u=254 b_u=303 a_c=251 b_c=922
  a_d=224 b_d=6502 a_l=252 b_l=700 a_r=249 b_r=1344`, and the final
  combine: `a1=7674 b1=103359 flt1=3110 xq0=-16 xq1=176`. Hand-verified
  every one of these numbers independently in Python against the raw
  9-tap box sums read back from the dumped buffers via `lr_sample` --
  ALL MATCH; the dense-radius arm's math is provably correct for this
  pixel. Did not get to instrument the fast (`r0=2`) arm's actual grid
  reads the same way before the turn cap forced a stop (only its
  `a=4000 b=16759 flt0=3097` final numbers were captured, not the 5
  underlying A/B neighbour reads) -- **the fast arm is the one unverified
  half**; check that first.
- A hand-written standalone C transcription of libaom's real algorithm
  (`/tmp/lr_ref.c`, hex box-sum + av1_x_by_xplus1/av1_one_by_x tables
  copied verbatim from `restoration.c`) produced 194 (matching ffmpeg)
  against the same dumped buffers -- but a *second*, differently-scoped
  probe (`/tmp/lr_probe.c`) printing individual A/B taps gave
  self-inconsistent numbers across separate runs of what should be
  identical code, so that C reproduction is NOT trustworthy evidence of
  a real Rust bug; treat both `/tmp/lr_ref.c` divergence claims as
  unconfirmed, not as a finding. The live-Rust-instrumented numbers
  above are the reliable data.
- Next agent: re-run this round's dump harness (recreate the 3
  env-gated `eprintln!` blocks -- see this section's description, they
  were reverted, not left in the tree) to print the FAST (`r0=2`) arm's
  5 individual A/B neighbour reads (`a_um1/b_um1`, `a_up1/b_up1`,
  `a_uml`, `a_umr`, `a_upl`, `a_upr` for the row-61 ODD-branch case --
  wait, row61 is ODD relative to `v_start=60`, so it's actually the
  OTHER branch: own-row `a_c/b_c` + `a_l/a_r` neighbours, matching
  `restoration.rs`'s `else` arm ~line 738) and hand-verify those 3
  numbers the same way the dense arm was verified above. If those also
  check out, the bug is not in `apply_sgrproj_stripe` at all and the
  next place to look is upstream: whether the V-plane's CDEF/deblocked
  pixel data feeding `lr_sample` is itself off by something that only
  this specific rounding-sensitive combination surfaces (Y and U match
  bit-exact on every attempt; only V, only this one pixel, only this
  one `(ep,xqd)` combination, across the whole 40-attempt run).

## Next lever (r3 -> r4)
1. Wiener pixel filter (charter step 2): `apply_loop_restoration` after
   `apply_cdef` in `decode.rs` (two call sites, ~4983/10502), driven by
   the `RestorationGrid`/`(WienerInfo, SgrprojInfo)` this round's gate
   proved decode correctly. Watch the 3-pixel stripe boundary save/
   restore (`rlbs`) -- not investigated this round.
2. Self-guided (SGR) filter (charter step 3): box-sum radii from
   `SGR_PARAMS`.
3. Once both are wired and this gate's LR frames start decoding
   pixel-exact, widen the gate to assert `Ok` + pixel match instead of
   the named refusal, then drop the `stream.rs` refusal (charter step 4).

## Turn budget (r3)
Spent on: reading the charter + r2 section, writing the gate, and
(unplanned but necessary) diagnosing + fixing the check-before-decode
bug the gate exposed -- confirming it via a scratch `EC_AV1_PIN` decode
of a hand-built fixture (`loop_restoration.uses_lr=true`, `frame_restoration_type`
`Sgrproj`) before touching `stream.rs`. Did not reach either pixel
filter; r4 should start directly at Wiener (`decode.rs`'s two
`apply_cdef` call sites), no further libaom recon needed this report
plus `restoration.rs`'s own doc comments don't already cover.

## Turn budget (r2)
Spent the full 75-turn allowance on: recon of libaom's exact `k`-per-tap
subexp/SGR-table/read_lr_unit shape (not fully covered by r1's report --
the report's own citations turned out right, but exact constants/ep-branch
logic needed a fresh libaom read from `~/.cache/aom-oracle`), writing +
pinning `restoration.rs`'s bitstream-read half, wiring it into both SB
loops, and confirming no regression (235/0). Did not reach the gate or
either pixel filter -- see "Next lever" above; r3 should start there
directly rather than re-reading libaom (this doc + `restoration.rs`'s own
doc comments now carry every file:line needed).
