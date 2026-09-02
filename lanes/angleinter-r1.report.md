# lane-angleinter r1 — a nonzero angle delta on an intra block inside an inter frame

Branch `lane-angleinter` off `lane-fiinter` 6b82994. Class:
[[refusal-strings-are-claims]] + [[tool-disabled-in-every-gate]].

## The claim, tested

Both intra-in-inter arms read `angle_delta_y` and then refused it as
"a nonzero angle delta (this encoder never writes one)". The claim is FALSE:
aomenc's `--enable-angle-delta` defaults to 1 and libaom's
`read_intra_angle_info` (inside `read_intra_block_mode_info`, decodemv.c)
writes the symbol for any directional `y_mode` at bsize >= 8x8 exactly as a
key frame does. A real default-settings inter stream desynced there.

Confirmed by the gate below on the FIRST attempt (seed 42, 8-bit): 4 intra
blocks inside inter frames carry a nonzero `angle_delta_y`.

## What changed

- `crates/ec-av1/src/decode.rs:15542` (`decode_inter_block`, >=16x16 arm):
  refusal replaced by `read_angle_delta`; the delta is threaded into every
  luma prediction of the block — the split-transform per-TU
  `reconstruct`/`read_plane` pair and the block-level pair — where a literal
  `0` used to go.
- `crates/ec-av1/src/decode.rs:15555`: the same arm derived
  `smooth_neighbor` (`get_intra_edge_filter_type`, spec 7.11.2) via the
  existing `Neighbours::modes_above_left` helper; it passed a hardcoded
  `false` before, which was unobservable while every directional
  intra-in-inter block was refused, and decides the intra edge filter's
  strength once a delta'd angle reaches it.
- `crates/ec-av1/src/decode.rs:82` (+ around): counters
  `intra_in_inter_angle_delta_y_hits` / `intra_in_inter_angle_delta_uv_hits`.
- `crates/ec-av1/src/decode.rs:17191` (`decode_inter_block8`, 8x8 leaf):
  the refusal STAYS, reworded to a claim about this decoder's gate coverage
  instead of about the encoder — see "Not lifted" below.
- `crates/ec-av1/src/stream.rs`: gate `angle_delta_in_inter_gate` + its 8-bit
  and 10-bit tests.
- `refusal_inventory.rs`: the capability claim is gone; the 8x8-leaf refusal
  is listed as a decoder-scoped refusal. `gate_coverage.rs`:
  `enable-angle-delta` deleted from `NEVER_EXERCISED_8BIT` and `_10BIT`.

## Gate

`a_real_aomenc_inter_sequence_with_a_directional_intra_block_with_angle_delta_decodes_pixel_exact`
(+ `..._10bit_...`). Recipe = the intrainter/fiinter one (64x64 mandelbrot
zoom with a hard overlay cut at frame 4, `--cq-level=30`, `--kf-min-dist=1000`
`--kf-max-dist=1000`, `--min-partition-size=16`) with
`--enable-directional-intra=1` and `--enable-angle-delta=1` — each flag
spelled ONCE in the arg list (the base recipe's `=0` entries were edited in
place, so flag precedence cannot silently invert the arm). Every frame's Y, U
and V are compared against ffmpeg; a decode error or mismatch is a FAILURE,
never a SKIP; the hit counter is a hard assert.

```
cargo test -p ec-av1 --lib with_a_directional_intra_block_with_angle_delta -- --nocapture
```

EVIDENCE: $HOME/.cache/angleinter-suite.log | aomenc --enable-angle-delta=1 --enable-directional-intra=1, 8 frames decoded and Y/U/V-compared vs ffmpeg | 8-bit seed 42: 4 nonzero-angle_delta_y intra-in-inter blocks, 0 sample mismatches; 10-bit seed 47: 4 blocks, 0 mismatches (buckets counted-exact=1 uncounted-exact=0)

EVIDENCE: negative control (scratchpad decode.rs.bak diff) | re-inserted `return Err(unsupported("NEGATIVE CONTROL: a nonzero angle delta"))` at decode.rs:15549, reran both gates | 2 passed -> 2 FAILED, all 40 attempts refused with that string; lift reverted afterwards

## Mutation sweep (task 3)

All 56 `--enable-angle-delta=0` spellings in `stream.rs` flipped to `=1`, then
the 17 `inter_sequence` gates rerun: **17 passed / 0 failed** — no hidden
defect behind the flag. stream.rs restored from the scratchpad copy
afterwards (only the new gate's own two spellings are `=1` on the branch).

EVIDENCE: mutation run | sed -i 's/--enable-angle-delta=0/=1/g' + `cargo test -p ec-av1 --lib inter_sequence` | test result: ok. 17 passed; 0 failed; 2 ignored

## Not lifted (disposition)

`deferred: the 8x8-leaf arm's nonzero-delta case (decode.rs:17205) — 80
attempts (64x64 and 128x128 sources, cq 20/30/40/50/60, seeds 42..81) never
produced an 8x8 intra leaf with a nonzero angle_delta_y, nor even one with a
directional luma mode: the leaf's three sibling refusals ("a non-DC chroma
mode on an 8x8 inter-frame leaf", "an 8x8 intra leaf ... whose tx_depth
splits it into 4x4 transform units", "an inter partition below 16x16 other
than SPLIT") fire first on 37/40 attempts. Lifting it would ship an ungated
capability claim (class refusal-lifted-without-a-gate), so the refusal stays,
reworded to be about this decoder. — unblocked by: lifting the non-DC-chroma
and TX_4X4-split refusals on that leaf, which then makes a directional 8x8
intra leaf reachable.`

`accepted: angle_delta_uv fired 0 times in every passing attempt — the
recipe's `hue=s=0` desaturation is what keeps uv_mode on DC and the transform
split luma-driven. The uv delta path was already read AND already threaded
into both chroma predictions before this round (decode.rs:15556 ff.), so the
lift did not touch it; the counter is in place for a future chroma gate.`

`accepted: decode_inter_block8 still passes a hardcoded `false` for its luma
edge-filter neighbour type. Unobservable while its directional-mode
population is empty (see above); reverted rather than shipped ungated.`

## Suite + film

`cargo test -p ec-av1 --lib` (systemd unit, log `$HOME/.cache/angleinter-suite.log`):
**348 passed / 0 failed / 32 ignored**, 384 s.

Film probe (`hg-head.obu`, 0.4 s of the Hunger Games 2160p10 film, 18 frame
headers): now stops at `filter intra on a HORZ/VERT strip (this decoder
predicts square-only)` — a different, rect-prediction refusal; the angle-delta
refusal is no longer on its path.

EVIDENCE: $HOME/.cache/angleinter-suite.log | `cargo test -p ec-av1 --lib` under a systemd unit + `cargo run --example decode_probe -- hg-head.obu` | 348 passed / 0 failed; film refusal string = "filter intra on a HORZ/VERT strip"
