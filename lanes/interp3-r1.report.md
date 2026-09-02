# lane-interp3 r1 -- the `switchable_interp` PANIC: the compound 8x8 leaf never read its filter

## Root cause (cited)

libaom `decodemv.c:1575` (`read_inter_block_mode_info`) calls `read_mb_interp_filter`
for **every** inter block, compound ones included, right after `read_compound_type`.
`decode_leaf8`'s COMPOUND arm (`crates/ec-av1/src/decode.rs`, the branch that returns at
the old line 19589) called `resolve_interp_filter` **not at all**: it predicted both taps
with a hard-coded `Regular` and returned the `[3, 3]` "no filter recorded" sentinel as the
leaf's `leaf_filter_syms`. Two consequences, both real on his data:

1. **Entropy**: on a `SWITCHABLE`-filter frame one symbol (two under `enable_dual_filter`)
   is left unread at every compound 8x8 leaf -- silent desync.
2. **Crash**: the sentinel is stamped into `Neighbours::above_filter`/`left_filter`, and a
   neighbouring OBMC block's `neighbour_filter` fed it to
   `mc::InterpFilterKind::from_switchable_symbol`, whose `panic!("switchable_interp's
   alphabet is exactly 3 symbols")` aborted the process. That is the panic lane-r14 r3
   flagged (its `--enable-dual-filter=1 --enable-obmc=1` 192x128 cq30 stream). The same
   shape was already fixed once for the SINGLE-ref 8x8 leaf (lane-gmaffine r3, decode.rs
   ~21509 comment) -- the compound arm was the surviving sibling instance.

## Changed

- `crates/ec-av1/src/decode.rs` (compound 8x8 leaf, after the `comp_group_idx`/
  `compound_idx` reads): reads `interp_filter` in libaom's order, with `get_ref_filter_type`
  neighbour contexts keyed on `ref0` and `is_compound = true`; suppression term is
  `skip_mode || is_nontrans_global_motion` (GLOBAL_GLOBALMV + both refs' models non-
  TRANSLATION; WARPED_CAUSAL cannot occur on compound). The 12 `predict_compound_intermediate`
  taps now use the resolved `h_filter`/`v_filter` instead of `Regular`.
- `crates/ec-av1/src/decode.rs` `neighbour_filter`: returns `Result`; the `[3, 3]` sentinel
  is now the named refusal `an OBMC neighbour whose interp filter was never recorded (no
  switchable symbol for that block)` -- **never a panic**. Both OBMC call sites `?` it.
- `crates/ec-av1/src/decode.rs`: counters `DUAL_FILTER_DIFF_HITS` (a block whose two
  dual-filter directions differ -- impossible with dual filter off) and
  `COMPOUND8_FILTER_HITS`.
- `crates/ec-av1/src/refusal_inventory.rs`: the new refusal listed (inventory test green).
- `crates/ec-av1/src/stream.rs`: `inter_sb_none_gate` takes a `frame_count`
  (4 frames never code a compound block); new gate
  `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
  (`--enable-dual-filter=1 --enable-obmc=1 --enable-warped-motion=1 --enable-onesided-comp=1`
  on the `--min-partition-size=8 --enable-ab-partitions=0 --enable-1to4-partitions=0`
  recipe, every frame compared Y/U/V vs ffmpeg, hard asserts on all three counters).
- `crates/ec-av1/src/decode.rs` test
  `an_obmc_neighbour_with_no_recorded_filter_refuses_instead_of_panicking`.

## Gate: RED, `#[ignore]`d with the measurement, not deleted

`EC_INTERP3_FRAMES=4` -> aomenc codes no compound block, stream decodes.
From 5 frames on it does, and the decode stops at a refusal that the recipe makes
impossible: `an inter 16x16-level AB or 1:4 partition` WITH this round's filter read, and
`an inter partition below 8x8` WITHOUT it (measured by ablating the read behind a temporary
env switch, since removed). `--enable-ab-partitions=0 --enable-1to4-partitions=0
--min-partition-size=8` cannot emit either -- so both are our own desync
(class `refusal-from-own-desync`): the compound 8x8 leaf carries at least one more
missing/misplaced symbol beyond the interp filter. The fix is therefore **necessary but not
sufficient** and is not claimed as pixel-proven.

EVIDENCE: $HOME/.cache/interp3-suite-r1.log | `cargo test -p ec-av1 --lib -j3` under a
systemd user unit, MemoryMax=10G | totals below
EVIDENCE: gate run `EC_INTERP3_FRAMES=5..16 cargo test -p ec-av1 --lib a_real_aomenc_dual_filter_obmc_8x8` with and without the new read | 6 encodes x 2 arms | refusal string differs per arm (AB/1:4 vs below-8x8), 0 frames compared either way -- gate RED, ignored
EVIDENCE: `cargo test -p ec-av1 --lib an_obmc_neighbour_with_no_recorded` | direct call with the `[3, 3]` sentinel | 1 passed: refuses by name, no panic

## Panic sweep of the decode path (charter clause)

`neighbour_filter` was the only `panic!` reachable from a *stream-derived value* on the OBMC
path; converted. The rest of `decode.rs`'s 57 `panic!/unreachable!/unwrap/expect` sites,
inspected: `2733, 11268, 11617, 11636, 11683, 11996, 12015, 12052, 13820, 13998, 15225,
15533` are `unreachable!` on values a CDF alphabet or an earlier refusal already bounds
(cannot fire on any stream); `5238, 5282, 6043, 6110, 7310, 7311, 7777, 7778, 10479, 10535,
10679, 10888` are `unwrap/expect` on this decoder's own just-built state; everything above
line 22700 is inside `#[cfg(test)]`. Two remain flagged, not converted (they need a `Result`
in a counter/table helper and no stream is known to reach them):
`decode.rs:2486` `panic!("ref_hits: no per-reference counter for ref_frame {other}")` and
`decode.rs:3247` `panic!("intra mode {other} has no Intra_Mode_To_Tx_Type entry")`.
`mc.rs:203`/`:220` stay as documented invariants -- both callers now check first.

## Residue

- fix-now(next round): the remaining compound-8x8 desync. Method: `EC_TRACE=1` aomdec range
  ladder against ours on the FIRST compound leaf of frame 5 of the gate's stream, comparing
  msac RANGE element by element (never `tell()`).
- deferred(needs that fix): the 10-bit arm of the new gate, and the counter-proving
  pixel-exact claim.
