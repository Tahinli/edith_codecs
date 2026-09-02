# lane-fiinter r1 -- `use_filter_intra` on an intra block inside an inter frame

Branch `lane-fiinter`, rebased onto main `eebf24e` (charter's base `lane-intrainter e841b2a`
replayed onto main; no conflicts).

## Premise, re-measured

`decode_inter_block`'s INTRA arm and `decode_inter_block8`'s 8x8 INTRA arm read
`y_mode` -> `angle_delta_y` -> `uv_mode` (+cfl/angle) -> palette and then went straight to
`read_block_tx_size` -- no `use_filter_intra` symbol at all, while libaom's
`read_intra_block_mode_info` (decodemv.c) ends with `read_filter_intra_mode_info` in exactly
that place, key frame or inter frame alike. Every inter gate recipe in `stream.rs` spelled
`--enable-filter-intra=0` (69 occurrences), so nothing had ever exercised it; aomenc's own
default is ON, so his films' inter frames desynced silently at the first such block.
Classes: [[symbol-consumption-gap]] + [[tool-disabled-in-every-gate]].

## What changed

- `crates/ec-av1/src/decode.rs:64-80` -- `INTRA_IN_INTER_FILTER_INTRA_HITS` counter +
  `intra_in_inter_filter_intra_hits()`, the gate's firing proof (the existing
  `FILTER_INTRA_HITS` is also incremented, so the key-frame gates' deltas stay meaningful).
- `crates/ec-av1/src/decode.rs:830-856` -- `ENABLE_FILTER_INTRA_INTER` thread-local, set by
  `set_inter_tx_mode(tx_select, reduced_tx_set, enable_filter_intra)`; same frame-constant
  stash `TX_SELECT_INTER`/`REDUCED_TX_SET_INTER` already use rather than a 51st
  `decode_inter_block` parameter.
- `crates/ec-av1/src/decode.rs:17497+` -- `decode_inter_frame_tile_with_cdfs` takes
  `enable_filter_intra` (last param); `decode_inter_frame_tile` passes `false` (this crate's
  encoder never writes the sequence bit), `stream.rs:694` passes the sequence header's bit.
- `crates/ec-av1/src/decode.rs:15578-15594` (>=16x16 square intra-in-inter arm) and
  `:17188-17204` (8x8 leaf arm) -- `use_filter_intra` read via
  `cdfs.filter_intra[filter_intra_size_class(side)]` right after the palette syntax, then
  `filter_intra_mode`. `av1_filter_intra_allowed`'s `palette_size[0] == 0` term is implied (a
  real palette-Y block already returned by refusal); `filter_intra_size_class` returns `None`
  at 64x64, so no symbol is read there, matching `av1_filter_intra_allowed_bsize`.
- Same arms: `filter_intra` now threaded into every LUMA prediction/coefficient call --
  the per-TU split path (`y.reconstruct` and `read_plane` inside the `vartx_leaves` loop,
  i.e. lane-intrainter's split-tx path), the whole-block skip `reconstruct`, and the
  whole-block `read_plane`. Chroma keeps `None`, exactly as the key-frame square arm does.
  `read_plane`'s own `FIMODE_TO_INTRADIR` mapping (decode.rs:7445) supplies the tx_type CDF
  row per TU, so lane-tiny's filter-intra tx_type row fix applies here unchanged.

## Gate

`stream.rs:4438-4700`:
`a_real_aomenc_inter_sequence_with_filter_intra_on_an_intra_block_decodes_pixel_exact` (8-bit)
and `..._10bit_...` (10-bit), driven by `filter_intra_in_inter_gate(bit_depth)` -- the
intrainter recipe (mandelbrot + hard overlay cut at frame 4, cq30, `--kf-min-dist=1000`,
`--enable-tx-size-search=1`, `--max/min-partition-size=32/16`) with `--enable-filter-intra=1`.
The flag is spelled ONCE (the base `=0` deleted) so no aomenc precedence question arises;
arrival is proved decoder-side by the counter, per COMMON. Every decoded frame's Y/U/V is
compared against ffmpeg; a decode error is only accepted when its message contains
"unsupported"; running out of attempts panics (no SKIP).

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-fiinter EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 \
  cargo test -p ec-av1 --lib filter_intra_on_an_intra_block -- --nocapture
```

EVIDENCE: $HOME/.cache/fiinter-suite.log | cargo test -p ec-av1 --lib filter_intra_on_an_intra_block | 8-bit seed 42: 11 filter-intra intra-in-inter blocks, 8 frames Y/U/V exact; 10-bit seed 47: 8 blocks, 8 frames exact; buckets counted-exact=1 uncounted-exact=0

EVIDENCE: negative control (`&& false` in front of both new reads, decode.rs restored after) | same gate command | 8-bit: 40/40 attempts, 0 firings, 33 named refusals + 7 decoded-uncounted -> PANIC. The gate is not vacuous: without the read the stream desyncs.

EVIDENCE: mutation sweep (`sed 's/--enable-filter-intra=0/=1/g' stream.rs`, restored after) | `cargo test -p ec-av1 --lib inter_sequence` | 15 passed / 0 failed / 2 ignored (both pre-existing `#[ignore]`s: the 10-bit 8x8-leaf-split twin and the tile-column 8x8 gate) -- every inter gate that passed with filter intra OFF still passes with it ON, including `..._split_transform_intra_block_...` at both depths.

## Refusals

None lifted -- there was no refusal here, only an unread symbol (which is the worse shape:
`refusal_inventory.rs` is unchanged and stays green). `gate_coverage.rs` needs no edit: its
coverage is derived from the `--enable-*` spellings in the gate source, and the two new tests
spell `--enable-filter-intra=1` at 8 and 10 bit.

## Residue

- deferred(a directional-intra-in-inter gate): the `"a nonzero angle delta (this encoder never
  writes one)"` refusal in both intra-in-inter arms stays. The symbol read itself is correct
  (`cdfs.angle_delta[mode - V_PRED]`, compared to `ANGLE_DELTA_ZERO`), so lifting it is
  mechanical -- but lifting a refusal without a gate that fires it is forbidden by COMMON, and
  this lane's recipe pins `--enable-angle-delta=0`/`--enable-directional-intra=0` (a nonzero
  delta would need its own source family). Unblocked by: an aomenc recipe that puts a
  directional intra block with a nonzero delta inside an inter frame.
- accepted: the 8x8-leaf arm's read is size-generic and compiles/decodes, but no gate proves it
  FIRED -- this gate pins `--min-partition-size=16`. The mutation sweep ran the 8x8-leaf-split
  gates with filter intra ON and they stayed pixel-exact, which bounds the risk but does not
  prove the symbol fired at 8x8.
- accepted: CfL/palette parity is unchanged from the key-frame arm -- a real palette-Y/UV block
  still refuses by name before the filter-intra read, and `UV_CFL_PRED` chroma alphas are read
  as before (filter intra is luma-only).

## Suite + film

`cargo test -p ec-av1 --lib` (systemd unit, log `$HOME/.cache/fiinter-suite.log`): **346 passed / 0 failed / 32 ignored**, 739 s.

EVIDENCE: $HOME/.cache/fiinter-suite.log | systemd-run --user --unit=fiinter-suite ... cargo test -p ec-av1 --lib -j3 | test result: ok. 346 passed; 0 failed; 32 ignored

EVIDENCE: scratchpad/hg-head.obu | cargo run -p ec-av1 --example decode_probe -- hg-head.obu | 18 frame headers parsed, now stops at `unsupported: AV1 tile (filter intra on a HORZ/VERT strip (this decoder predicts square-only))` -- i.e. his film really does carry filter intra, and the next blocker is the rect-strip filter-intra predictor (another lane), not the unread inter symbol this lane fixed.
