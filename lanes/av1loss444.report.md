# lane-av1loss444 — the 4:4:4 LOSSLESS chroma divergence: ALL FOUR WITNESSES ARE ALREADY EXACT ON MAIN

**Tree.** branch `lane-av1loss444` off main `fc264573`, worktree
`~/.cache/wt/av1loss444`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1loss444`
(lane-private). One file touched: this report. **No decoder change was made and
none was needed** — see §0. Nothing pushed (Main merges).

## 0. Verdict in one table

| recorded witness | bytes / sha256 (re-derived) | status on main `fc264573` |
|---|---|---|
| (i) 256x128 yuv444p lossless, `--tile-columns=1 --cpu-used=2` | 77847 / `87b0d1a5…` | **EXACT 6/6**, 0 wrong luma, 0 wrong U, 0 wrong V |
| (ii) 256x128 yuv444p lossless, `--tile-columns=1`, tools defaulted | 75446 / `65e94e91…` | **EXACT 6/6** |
| (iii) 4:4:4 10-bit lossless untiled 256x128 | 122730 / `ec7a6a0e…` | **EXACT 6/6** |
| (iv) 4:4:4 12-bit lossless untiled 256x128 | 171599 / `ef733813…` | **EXACT 6/6** |

Every recorded first-divergence sample — `U(110,26)`, `U(237,58)`, `Y(128,0)`
on f1, `U(109,0)` on f0 — now compares equal, in the same frame the record
named. **The cell is closed; the class has no open instance on main.** The
unit-addressed dump the charter asked for as the next measurement is not
reachable: there is no first wrong sample to address. §4 records what was run
instead, and §5 the one cell of the neighbourhood that is *not* reachable, for a
reason that has nothing to do with this defect.

## 1. Re-derivation: every witness reproduced byte-for-byte before any measurement

All encodes: `~/.cache/aom-oracle/build/aomenc`, source piped as y4m
(`ffmpeg -f lavfi -i "testsrc2=size=WxH:rate=25" -frames:v 6 -pix_fmt yuv444p*`
— `mandelbrot` + `gblur=sigma=6` for the 12-bit cell, the recipe
`lanes/av1formatsweep` §6.1 recorded), common flags
`--codec=av1 --passes=1 --end-usage=q --threads=1 --row-mt=0 --lag-in-frames=0
--kf-max-dist=100 --limit=6 --obu -o - -`.

| # | source | per-cell flags | bytes | sha256 | recorded as |
|---|---|---|---|---|---|
| i | testsrc2 256x128 444p 8-bit | `--lossless=1 --tile-columns=1 --cpu-used=2` | 77847 | `87b0d1a5b3044552fc1fae839e868c30fdc0bebe79f8b3a3104030ffea24ab50` | Selin2-2 witness A |
| ii | testsrc2 256x128 444p 8-bit | `--lossless=1 --tile-columns=1` (tools default) | 75446 | `65e94e91c56178d21f5c8441877d0dbfbee17bce0916e971fbe6c51677c9891e` | Selin2-2 witness B / H3 |
| — | testsrc2 256x128 444p 8-bit | `--lossless=1` (tools default) | 74076 | `6c49502811b1a5d8faf6dfbb76bb6acc39039697bc082f1102ce3cf58b6d212b` | tilemeasure H3 control |
| iii | testsrc2 256x128 444p10le | `--lossless=1 --input-bit-depth=10 --bit-depth=10` | 122730 | `ec7a6a0ed9d7c2bada8816bb692de3379620856644a462f287a6cc3e6d9c5103` | formatsweep `t444_10_notile` |
| iv | mandelbrot+gblur 256x128 444p12le | `--lossless=1 --input-bit-depth=12 --bit-depth=12` | 171599 | `ef7338136d560674677cb5fd903e60906ef4df7b2f4c93a990e65a5b5ed4402c` | formatsweep `t444_12_notile` |
| — | testsrc2 256x256 444p 8-bit | `--lossless=1 --enable-palette=0 --enable-intrabc=0` | 109215 | `4563a01f1778600020ffa9f15bc05f5559321cf5712b99a29ed7192675dee6a4` | tilemeasure §3 "DIVERGENT f0, 49 samples" |

Three of the five shas are the *full* 64-hex the record published (i, ii, iii)
and match it exactly; the 256x256 carrier matches `lanes/av1tilemeasure` §2.4/
§3's `4563a01f…`. **Nothing had drifted**, so every "already fixed" below is a
measurement on the recorded bytes, not on a look-alike.

Wire-level facts, read from each stream's OWN sequence header by
`decode_probe` (not from the recipe): i → `bit_depth=8`, tile `cols=2 rows=1`,
`use_128x128_superblock=false`; ii → `bit_depth=8`, `cols=2 rows=1`, sb128;
iii → `bit_depth=10`, `cols=1 rows=1`; iv → `bit_depth=12`, `cols=1 rows=1`;
256x256 → `bit_depth=8`, sb128, `cols=1 rows=1`. The 4:4:4 subsampling itself is
confirmed by file length: `dump_yuv` writes `W*H` samples per plane and the
oracle's per-frame file is the same length, which a 4:2:0-coded frame of the
same dimensions could not be (§3).

## 2. Per-frame, per-plane wrong-sample counts (ours vs oracle rung 12)

Ours = `ec-av1`'s `EC_AV1_FINAL_DUMP` (rung 12's twin, the post-store reference
picture). Oracle = instrumented `aomdec --codec=av1` with
`EC_AV1_FINAL_DUMP=<prefix>`, 6 `.f<N>` files a side. Frames are DECODE order;
the streams carry no altrefs (`--lag-in-frames=0`, `--limit=6`), so decode order
is display order. **The compare asserts equal per-frame byte lengths before
counting** — the prefix-compare trap that produced a false "byte-exact 6/6" for
this very cell class (`lanes/av1formatsweep` §6.0 trap 1) is structurally
excluded: a length mismatch prints as `LENGTH MISMATCH` and no count follows.

| stream | f0 | f1 | f2 | f3 | f4 | f5 |
|---|---|---|---|---|---|---|
| (i) `87b0d1a5` 8-bit tiled | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 |
| (ii) `65e94e91` 8-bit tiled, tools default | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 |
| untiled control `6c495028` | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 |
| (iii) `ec7a6a0e` 10-bit | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 |
| (iv) `ef733813` 12-bit | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 |
| 256x256 `4563a01f` 8-bit | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 | Y0 U0 V0 |

**Total wrong samples across all 36 frame-dumps of the six streams: 0** (5 × 6
frames × 3 planes; 8-bit cells 32768 samples/plane/frame, 10/12-bit 65536 bytes
per plane per frame = the full 12-bit range is compared, not a narrowed `u8`).
The recorded fingerprints — 29..103 chroma samples per frame with `x >= 108`,
12 U + 40 V, 290506 samples from f1, 264180 from f0 — are all zero now.

## 3. The class sweep around them (15 further 4:4:4-lossless cells, all 6/6 EXACT)

The four witnesses being exact is a fact about four streams. To say something
about the CLASS, thirteen adjacent cells of the same family were encoded and
compared the same way, spanning depth 8/10/12, sb64/sb128, untiled / 1 tile
column / 1 tile row / 2×2 tile grid, palette+intrabc on, and 4:2:0-untouched
`--tile-rows=1` no-op controls:

| cell | flags | bytes / sha256 | wrong (all 6 frames) |
|---|---|---|---|
| 256x128 untiled 8-bit | `--lossless=1` | 76468 / `c2cc3bd5…` | 0 |
| 128x256 sb128 8-bit | `--sb-size=128` | 67946 / `d47e87df…` | 0 |
| 320x256 8-bit | `--tile-rows=1 --tile-columns=1` | 123285 / `b06387f0…` | 0 |
| 256x320 8-bit | `--tile-rows=1` | 114368 / `f147a546…` | 0 |
| 512x256 8-bit | `--sb-size=128 --tile-rows=1` | 160585 / `aa113c42…` | 0 |
| 256x256 8-bit | `--sb-size=128 --enable-palette=1 --enable-intrabc=1` | 107320 / `31bfdb9b…` | 0 |
| 256x128 10-bit | `--sb-size=128` | 124855 / `a628e835…` | 0 |
| 256x128 12-bit mandelbrot | `--enable-palette=0 --enable-intrabc=0` | 295035 / `d2b25ee2…` | 0 |
| 256x128 12-bit mandelbrot | `--tile-rows=1 --tile-columns=1` | 293037 / `6e2258b3…` | 0 |
| 256x256 12-bit mandelbrot | `--sb-size=128` | 731936 / `6838684c…` | 0 |
| 512x256 12-bit mandelbrot | `--sb-size=128 --tile-rows=1` | 1172597 / `d6d23a72…` | 0 |
| 256x128 12-bit flat grey | `--enable-palette=0 --enable-intrabc=0` | 122 / `7d07a7e7…` | 0 |
| 256x128 12-bit black | `--enable-palette=0 --enable-intrabc=0` | 137 / `23197378…` | 0 |

Two facts in that table are worth keeping:

* **The 12-bit cell is reachable after all.** The record inherited the belief
  that 4:4:4 lossless at 12 bits is only observable through the
  `allow_screen_content_tools=1` refusal. It is not: aomenc decides that bit per
  frame from the screen-content detector (`av1_set_screen_content_options`,
  `~/.cache/aom-oracle/src/av1/encoder/encoder.c:2426`), and a `mandelbrot`
  source at 12 bits does not trip it. That gives a 295 KB six-frame 12-bit
  4:4:4-lossless cell, exact 6/6 — so the 12-bit arm of the class is covered by
  measurement, not by a refusal.
* **`--sb-size=128` is a no-op at 256x128** (byte-identical to sb64, 295035 /
  `d2b25ee2…` both ways), the same trap `lanes/av1formatsweep` §6.0 trap 2
  recorded for `--tile-rows=1`. The sb128 claims in this table are therefore
  only measured at 256x256 and 512x256, where the flag provably moved bytes
  (`t8_sb128_ibc` 107320 vs the sb64 4563a01f 109215).

## 4. What replaced the unit-addressed dump: a non-vacuity proof of the compare

The charter's next measurement ("our levels vs oracle `EC_COEFF_VAL`, then the
WHT, on the first affected lossless chroma unit") is **unreachable**: there is no
first affected unit on this tree. `EC_COEFF_VAL` / `EC_DQCOEFF` were therefore
not run — running them would produce a trace pair with no fork in it, and
quoting that as evidence would be the vacuous shape the contract forbids.

What replaces it is the proof that the *zero* is a measurement and not a broken
harness. Two negative controls, both on the real dumps:

1. **8-bit, 2 injected wrong samples.** Taking witness (i)'s own `f0` pair and
   flipping one bit at `Y(110,26)`, `U(110,26)`, `V(110,26)` — the very
   coordinates the recorded fingerprint named — the compare reports
   `Y 2 / U 2 / V 2, first (110,26)`. It sees exactly what was injected.
2. **12-bit, 1 injected LSB.** On witness (iv)'s `f0` pair, flipping a single
   low bit of `U(109,0)` (the recorded 12-bit first divergence) reports
   `U 1 wrong` with `Y 0, V 0`. The high-bit path compares the full 12-bit
   range, not an 8-bit narrowing, so a 1-LSB fork cannot hide in it.

Both controls use the identical code path that produced every 0 in §2 and §3.
The zeros are therefore zeros, not silence.

## 5. The one neighbouring cell that is NOT reachable, and why it is a different class

`testsrc2` 256x128 yuv444p12le (not mandelbrot) at `--lossless=1
--enable-palette=0 --enable-intrabc=0` encodes to 251777 B / `873f8a49…` and this
decoder **refuses it by name**:

```
AV1 decode_stream (a 12-bit frame with screen content tools
(allow_screen_content_tools=1: neither palette nor intrabc has a 12-bit witness))
```

That is the deliberate H6 refusal at `crates/ec-av1/src/stream.rs:1821`, gated
by `a_12bit_screen_content_stream_is_refused_by_name` (§6, green). It fires on
the header BIT, not on palette/intrabc actually being used, and aomenc sets the
bit from the screen detector for this source. It is **not** the 4:4:4-lossless
chroma defect: the 12-bit lossless cells that the detector does *not* trip
(§3, four of them) decode byte-exact. If the screen-content refusal is ever
lifted, that 251777 B cell becomes a live 12-bit witness and belongs to the
refusal lane, not to this one. **Recorded, not touched.**

## 6. Neighbouring gates, re-quoted unchanged on this tree

No source file changed, so these are re-quotes, not re-baselines. All green on
`fc264573` in this worktree:

| scope | result |
|---|---|
| `cargo test -p ec-av1 --lib lossless` | **25 passed, 0 failed** (all 20 stream gates + 5 `transform::lossless_tx_tests`) |
| `cargo test -p ec-av1 --lib 444` | **43 passed, 0 failed** |
| `cargo test -p ec-av1 --lib 12bit` | **14 passed, 0 failed** |
| `cargo test -p ec-av1 --lib screen_content` | **4 passed, 0 failed** |
| `cargo check -p ec-av1 --all-targets` | **clean, no warnings** |

Named individually, the 4:4:4-lossless chroma gates that own this class:
`a_lossless_444_sub8_rect_leaf_chroma_reach_decodes_the_key_frame_pixel_exact`
(the `lane-av1loss444kf` sub8 4x4 chroma reach — 49 → 0),
`a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units`,
`a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint`
(`lane-av1lm444loss-corr`),
`a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units`,
`a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major`,
`a_lossless_444_rect16x4_chroma_reach_is_ss_aware`,
`a_lossless_sb128_square_frame_clips_overhanging_chroma_tus`,
`a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`,
`a_lossless_444_8bit_untiled_control_and_its_two_tile_column_sibling_decode_pixel_exact`
(witness (i)'s own control+sibling), `a_lossless_444_10bit_inter_stream_decodes_pixel_exact`,
`a_lossless_444_min_partition8_inter_stream_decodes_sample_exact`,
`a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`,
`a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact`. The 4:2:0
neighbours `a_lossless_libaom_key_frame_decodes_sample_exact` and
`a_lossless_libaom_inter_frame_decodes_sample_exact` are in the same 25/25 set.

**A correction to the charter's gate list: there are no `ishm` gates.** The
string does not occur in `crates/ec-av1/` (any file, any case) nor in any
`lanes/*.report.md`. The nearest neighbours to whatever was meant are the
intra-in-inter `ibc128` / `intra128_lossless_counters` unit tests, which live in
`crates/ec-av1/src/decode.rs` and are exercised by the 43-gate `444` set above.

**No committed floor was raised, and no counter was added**: nothing moved, so
there is no measured value to re-pin and no new arm to witness.

## 7. Provenance notes for whoever reads this next

* **The 256x256 inter-frame walk fix is NOT in main and main is still exact.**
  `lane-av1loss444mm` (`71ca43b1`, 11 rounds) is unmerged
  (`git merge-base --is-ancestor` says no) and its r2 finding — f1's transform
  units 8652 vs the oracle's 10368, a chroma walk fork at TU 4488 — cannot
  still be true: that exact stream (`4563a01f…`) measures 0 wrong on f1. The
  same arithmetic was therefore fixed under another lane's name
  (`lane-av1ibc128arm`'s per-mu-chunk chroma walk, which that branch's own
  report already anticipated). Do not re-charter the mm walk bug.
* `lane-av1loss444mm` is still worth keeping for its instrumentation work
  (`EC_DQCOEFF`'s lossless, plane-tagged twin) — that is an analysis tool
  question, not a defect question, and its lossless twin is what the next
  genuinely-divergent unit in this class should be compared against.
* The recorded "the error is already in the stored reference picture" fact is
  consistent with what is measured now: with the reconstruction itself exact,
  there is nothing left in the store for the later stages to corrupt.

## 8. What could not be separated

* **Whether the class is closed because of `lane-av1loss444kf` +
  `lane-av1lm444loss-corr` + `lane-av1ibc128arm` or because of something else
  entirely.** This lane measured the current state; it did not bisect which
  commit closed it, and the three branches are tangled (kf unmerged, corr
  unmerged, ibc128arm unmerged, all exact on main). Naming a single culprit
  commit here would be a guess, so none is named.
* **A full-suite run.** By contract no local full suite was executed; only the
  four scoped filters and `cargo check --all-targets`. Suite-wide validation is
  Main's, once, after the branches land.
* **The 251777 B screen-tools cell (§5)** stays unmeasured against the oracle
  because this decoder refuses it by name. Whether it would be exact if decoded
  is unknown and is not claimed.

## 9. Reproduce

```bash
D=$HOME/.cache/loss444; mkdir -p $D; cd $D
ffmpeg -v error -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 6 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe -y s6.y4m
~/.cache/aom-oracle/build/aomenc --limit=6 --kf-max-dist=100 --threads=1 \
       --row-mt=0 --lag-in-frames=0 --passes=1 --end-usage=q --lossless=1 \
       --tile-columns=1 --cpu-used=2 --obu -o w1.obu - < s6.y4m   # 77847, 87b0d1a5…
python3 cmp_final.py w1.obu 256 128 8 ./wk_w1                   # 6× "Y=0 U=0 V=0"
```

`cmp_final.py` is outside the repo (a read-only analysis script, as the mm
lane's were): length-asserting per-frame compare of `EC_AV1_FINAL_DUMP` dumps
from `dump_yuv` and from `aomdec`, per plane, with first-sample coordinates and
delta range. The two negative controls of §4 are the same script with a bit
flipped in a dump.
