# lane-corner — the whole-64 root at a superblock the frame edge cuts

## Verdict on the chartered defect: STALE PREMISE, not reproducible

The debt says a whole-64 root on a doubly-cut superblock desyncs chroma three
ways at 248x152. At `main` f6c67770 it does not, for two separate reasons:

1. **248x152 cannot carry the block at all.** Its bottom-right superblock has
   24 px of 64 inside vertically, so spec `decode_partition`'s `has_rows`
   (`MiRow + halfBlock4x4 < MiRows`: 32+8=40 vs MiRows 38) is FALSE and
   `PARTITION_NONE` is not even a legal symbol there — the writer's
   `Superblock::Whole` arm refuses it (`tile.rs:2919`). The chartered repro
   (`a_sweep_of_doubly_straddling_sizes_round_trips_through_ffmpeg` with the
   `aligned` guard removed) is GREEN because no edge root ever fires in it:
   its sizes top out at height 152, and the key root's own
   `x64 + SUPERBLOCK <= true_width` bound kept it to wholly-inside
   superblocks anyway (class `gate-blind-to-feature`).
2. **Where the block IS legal, all three sides agree.** Sizes whose last
   superblock keeps more than half of each axis inside (248x168, 232x168,
   248x184 — cut on BOTH axes — and 200x120) round-trip byte-exact in Y, U
   and V across the encoder's own reconstruction, our decoder and ffmpeg,
   over a 4-frame GOP.

## What shipped

* `encode.rs` `encode_key_frame_inner`: both guards lifted. The root's
  legality is now the spec's — `whole_inside || edge_inside`, where
  `edge_inside` is `has_half` on both axes — the same shape the inter root
  has had since lane-b64b. The `EC_AV1_B64EDGE` switch is now readable
  without the inter root's preset flag (`edge_root_enabled`), so `=0`
  restores the old behaviour on both roots.
* `I64_HITS` grew a third slot: whole roots that sat on a cut superblock.
  `take_i64_root_hits` returns `[whole, offered, edge]`, and the native gate
  prints the edge share (class `gate-blind-to-feature`).
* `stream::tests::a_doubly_cut_superblock_root_round_trips_in_every_plane`:
  the three-way, three-PLANE pin at four straddling sizes, asserting the edge
  fire count first. Its sibling
  `encoder.rs::a_non_superblock_aligned_clip_codes_edge_64x64_roots_and_decodes_sample_exact`
  compares LUMA only (class `metric-blind-to-a-plane`), which is why the
  chroma claim in the debt was never actually gated either way.

## Sweep

`aligned`-style whole-frame refusals: `grep -rn "SB_MI == 0" crates/ec-av1/src`
returns no other site — this was the only frame-alignment guard. The encoder
writes 64x64 superblocks only, so there is no whole-128 chroma path to sweep.

## Measurements

### What the lift buys: `EC_AV1_NATIVE_CROP=1920x792`, film A

An unaligned crop is exactly the case the `aligned` guard switched the whole
key root off on, so `EC_AV1_I64=0` reproduces the old code on this arm to the
byte. `bd_rate_screen_native --ignored`, `EC_AV1_NATIVE_FILM=1`:

| arm | bytes per ladder point | BD vs libaom | BD vs rav1e | wall ours | key 64 roots |
| --- | --- | --- | --- | --- | --- |
| before (`EC_AV1_I64=0`) | 69541 / 110325 / 180401 / 426318 | +23.2% | -1.6% | 170.7s | 0 of 0 |
| after (default) | 68847 / 109557 / 178345 / 426318 | +22.5% | -2.3% | 178.6s | 390 of 1440 (27.1%) |

PSNR is equal to the hundredth at every point (44.03 / 45.60-45.61 / 46.92 /
48.20 dB), so the ~1.0% of bytes is the whole of it: **0.7 BD points on both
reference columns** for +4.6% encode wall.

Zero of those roots sat on a cut superblock: 792 = 12*64 + 24, and a 24-px
straddle fails `has_rows`, so this crop cannot carry one. The edge half of
the lift is proven by the round-trip test, not by this row; the crop that
would measure it is 1920x1080 (a 56-px straddle) --
`deferred: the BD value of the EDGE root on a 1920x1080 crop -- 7 min/arm and
the box is shared -- unblocked by one `EC_AV1_NATIVE_CROP=1920x1080` pair`.

Both arms end with the same 9 pre-existing REFERENCE-ladder decode failures
(libaom crf 5 "a Golomb tail longer than this decoder reads", a rav1e +-1) --
identical before and after, ours decode fine; that is lane-dgolomb's open
debt, not this lane's.

### Invariants (release test binary, `$HOME/.cache/corner/inv.log`)

All green, 15 runs: pins `the_encoders_own_streams_are_byte_identical_to_their_pins`
(8562 / 33357 -- the 640x384 pin clip is superblock-aligned and its bytes did
NOT move, which is the same reason the 12-frame gate rows stay byte-identical:
every gate clip is aligned and takes the identical path),
`the_facade_codes_the_same_bytes_as_encode_sequence`,
`predicted_coeff_bits_track_the_tile_the_writer_wrote`,
`every_speed_preset_decodes_sample_exact_through_both_decoders --ignored`,
the four thread-determinism tests `--include-ignored`, and
`EC_COMP_MISMATCH=1` at `EC_AV1_SPEED` 0 and 6 over the two straddling-size
tests and the 232x168 encoder test: zero mismatch lines.

### Suite and check

`cargo test -p ec-av1 --release`: **570 passed, 0 failed, 46 ignored** (569 +
this lane's new test), 1135s, RC=0.
`cargo check --workspace --all-targets -j4`: 0 errors, 0 `ec-av1` warnings
(the 22 that print are pre-existing `ec-opus` missing-doc and one `ec-vorbis`
dead-code warning).

## Class

`refusal-lifted-without-a-gate`'s mirror: **a guard written from a desync that
the guard's own repro cannot reach**. The `aligned` line named a 248x152
corner as its evidence, but 248x152 makes that corner's `PARTITION_NONE`
illegal in the first place, so no build of this encoder ever coded the block
the guard forbade -- while the guard cost every key root on every unaligned
frame. Sibling shape to `stale-premise lanes` and to
`fixture-proves-symbol-not-signal`: the ceiling comment quoted a measurement
whose fire count was never asserted.
