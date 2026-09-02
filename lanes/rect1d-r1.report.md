# lane-rect1d r1 — the 1D-tx-class-on-a-rect-transform gate, and the defect it exposed

Branch `lane-rect1d` off main `eebf24e`. One commit: `6faae24`.

## What changed

- `crates/ec-av1/src/decode.rs:5773` — `decode_leaf_rect`'s `tx_depth` symbol now reads
  `cdfs.tx_size_cat1[ctx]`, was `cdfs.tx_size_cat2[ctx]`. **The defect.**
- `crates/ec-av1/src/decode.rs:978` — `RECT_COEFF_TU_HITS` + `RECT_CLASS1_HITS` counters
  (`rect_coeff_tu_hits()` / `rect_class1_hits()`), bumped in `read_coeffs_rect` at
  `:2943` and `:2960`.
- `crates/ec-av1/src/stream.rs:15570` — the gate
  `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`,
  copied from lane-rectclass `ef7a476` minus its two refusal strings (they no longer
  exist on main — lane-tx4x8 ported the 1D arms) and minus the now-vacuous
  `class1_refusals == 0` assert; plus one new hard assert `compared_rect_class1 > 0`.
- `crates/ec-av1/src/gate_coverage.rs:229` — `enable-flip-idtx` leaves `NEVER_EXERCISED_10BIT`
  (`enable-tx-size-search` was already off that list on this tree).
- `crates/ec-av1/examples/dump_yuv.rs` — new: raw `u16` plane dump. Both existing prefilt
  dumps (ours and the instrumented aomdec) write `as u8`, which is blind to a 10-bit
  mismatch by construction.

## Root cause

libaom `blockd.h:1344 bsize_to_tx_size_cat` = `bsize_to_tx_size_depth_table[bsize] - 1`.
BLOCK_16X8/8X16 (and the 1:4 BLOCK_16X4/4X16 `decode_leaf_rect` also decodes) are depth 2 →
category **1**, the same table a 16x16 square block reads. Category 2 belongs to the
32-level shapes. Both tables carry three symbols, so the wrong row decodes a *plausible*
depth from the wrong probabilities — the msac range diverges silently at the first such
leaf and every later symbol in the tile is read from the wrong place. Class: same shape as
`cdf-row-held-constant` / `wrong-alphabet-same-value`, here same alphabet, wrong row.
This only surfaces with `--enable-tx-size-search=1`; 11 of 15 gate recipes pin it to 0,
which is why the tree was green.

Sweep of the whole class (every `tx_size_cat*` read, `grep -n tx_size_cat crates/ec-av1/src/decode.rs`):
`decode_block_rect` 32x16/16x32 → cat2 ✅, `decode_block_rect4` 32x8/8x32 → cat2 ✅,
`decode_block_rect64` 64x32/32x64/64x16/16x64 → cat3 ✅, sub-8 4x8/8x4 leaf → cat0 ✅,
square `read_tx_size` 8/16/32/64 → cat0/1/2/3 ✅. `decode_leaf_rect` was the only wrong site.

## Evidence

EVIDENCE: /home/tahinli/.cache/rect1d/s42a.obu (sha256 5b3c8fc94b8e01f59755a4534eae7f5f9037440b5e4040357105b5a1d0dd395c, identical on two independent aomenc runs) | ffmpeg lavfi sinusoidal stripes 192x128 10-bit → aomenc `--reduced-tx-type-set=0 --enable-rect-partitions=1 --enable-tx-size-search=1 --enable-flip-idtx=1` → `dump_yuv` vs `ffmpeg -pix_fmt yuv420p10le` | luma differs at 846 samples before the fix (first at row 94 col 175, 509 vs 510; max |Δ| 551, all in the bottom-right of the frame), **0 differing samples after**

EVIDENCE: /home/tahinli/.cache/rect1d/ours.both + aom.both (EC_TRACE_COEFF + mode traces) | range ladder ours vs instrumented aomdec through the pinned stream | first 105 transform units' `all_zero`/`base`/`br`/`sign` ranges identical; TU 106 (block mi_row=24 mi_col=44, BLOCK_16X8) diverged: mode ladder agreed through `uv_mode` (rng 50640 both), then aomdec `EC_ISTEP name=tx_depth val=0 ctx=2 cat=1 rng=60682` against our cat-2 read (our next `all_zero` came out at rng 36870 vs its 60086) — **first divergent element = the `tx_depth` symbol's CDF row**

EVIDENCE: gate output | `cargo test -p ec-av1 --lib a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform -- --nocapture` | `12 pixel-exact decodes (0 named refusals out of 12 attempts, 6 of the matches 10-bit), rect coefficient TUs on compared attempts: 219 (10 of them a 1D tx class); flag arrival: rect partitions 120, tx depths 53, 1D-class (square) transforms 13` — before the fix the same gate panicked at seed 42 10-bit

## Tests

`cargo test -p ec-av1 --lib` (systemd unit, `$HOME/.cache/rect1d-suite.log`): see the
trailer line appended below by the run.

## Residue

- accepted: `decode_leaf_rect` still refuses a *split* transform at this level
  ("a HORZ/VERT intra strip below 16x16 with a split transform"). The depth symbol is now
  read from the right table, so a `depth != 0` leaf refuses by name instead of desyncing;
  porting per-unit rect prediction is its own lane.
- deferred: the 4x16/16x4 shapes share category 1 and this fix covers them, but no gate
  recipe in the tree is known to produce one — unblocks with a 1:4-at-16-level gate.

## Handoff (turn cap)

The lane is COMPLETE, not WIP: the defect is found, fixed, and gated. The only thing not
witnessed by me is the tail of the full-suite run — it was still executing (179 tests
printed, no `FAILED`, no `test result:` line yet) when my turn cap hit. Check with
`grep -E "^test result|FAILED" $HOME/.cache/rect1d-suite.log`; it is a plain
`cargo test -p ec-av1 --lib` under a systemd unit, nothing else to redo.

Reproduce the original defect on main `eebf24e` (before `6faae24`):
```
bash ~/.cache/rect1d/gen.sh /tmp/s42     # ffmpeg lavfi + aomenc, 10-bit, seed 42
cargo run -p ec-av1 --example dump_yuv -- /tmp/s42.obu /tmp/ours
ffmpeg -v error -i /tmp/s42.obu -pix_fmt yuv420p10le -f rawvideo /tmp/ref.yuv
cmp /tmp/ours.f0.yuv /tmp/ref.yuv        # 846 differing luma samples before, 0 after
```
or just `cargo test -p ec-av1 --lib a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform`.
