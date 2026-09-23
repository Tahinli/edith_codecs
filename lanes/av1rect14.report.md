# lane-av1-rect14 — 1:4 pair intrabc and the lossless 16x4 chroma walk

Base: `main` **7f8d6cf6** (checkout HEAD was 09c7ff39; this lane is pinned to the
charter base). Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-av1r14`,
branch `lane-av1-rect14`. `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1r14`,
`TMPDIR=$HOME/tmp-av1r14`. Nothing pushed.

Sibling `lane-av1-intrabc` (worktree `edith_codecs-av1ibc`) is in review and
was not touched. Its `decode_intrabc_rect` is the 2:1 helper. This lane's 1:4
body is a different function (`decode_rect4_16_intrabc`) so that merge is
additive. The 2:1 call sites this lane also lifted (`decode_intrabc_owned_rect`
in `decode_block_rect`, `decode_block_rect4`, `decode_leaf_rect`) collide with
that lane's wiring: keep theirs at those three sites if both land.

## 1. Headline

Two lifts.

1. An intrabc 16x4/4x16 strip is reconstructed as a pair. Chroma is coded only
   by the odd-mi member (`is_chroma_reference`, `av1_common_int.h:1454`): the
   bottom strip of `HORZ_4`, the right strip of `VERT_4`. The chroma plane
   block is `BLOCK_8X4` / `BLOCK_4X8` at the pair origin, not `bw/2 x bh/2`
   of the strip itself. The report recipe is byte-exact vs ffmpeg. allintra
   advances past the named refusal.
2. A lossless 16x4/4x16 chroma pair is two `TX_4X4`s per plane, plane-major,
   not one `ChromaRect8x4`. The measured site (luma col 108 of the 320x240
   sb128 lossless key frame) matches ffmpeg. Rows 0-127 of that frame match.
   The rest of the frame does not: a pre-existing 64x64 square lossless
   desync, not this class.

## 2. Byte-exactness

Probe: `$HOME/.cache/cargo-target-av1r14/release/examples/decode_probe`.
Oracle: `~/.cache/aom-oracle/build/{aomenc,aomdec}`. ffmpeg raw yuv420p.

| stream | recipe | base | this lane |
|---|---|---|---|
| ibc640 | testsrc2 640x480, cq60, palette=1, txs=0, rect+1to4, min4, sb64, screen, intrabc | REFUSED the named 1:4 strip string | **EXACT** 460800 bytes. `rect4_16_pair: intrabc=1`. aomdec `EC_PART_VAL mi_row=76 mi_col=108 bsize=6 value=8` |
| cq50 arm | testsrc2 256x192 cq50 palette=0 txs=0 min8 max32 sb64 screen intrabc (the old refusal arm) | refused by name | **EXACT** 73728 bytes. `rect_intrabc_reads=80` |
| allintra | the lane-av1-intrabc allintra stop | stopped on the 1:4/rect strip refusal | advances. `rect4_16_pair: intrabc=22`. New stop: `an intrabc block whose var-tx tree resolved to mixed leaf transform sizes` |
| loss320 | testsrc2 320x240, `--lossless=1 --sb-size=128 --enable-1to4-partitions=1 --min-partition-size=4`, 1 frame | first luma diff **byte 108** (col 108) — the §6 measurement | luma rows 0-127 **EXACT**. First remaining luma diff byte 41120 = row 128 col 160. `rect4_16_pair: lossless_chroma=6` |
| lnone | same source, 1:4 and rect off | first ffmpeg diff byte 41120 (pre-existing) | same first byte 41120 on both probes. lane vs base diverges at byte 41152, inside the region both probes already misdecode (pre-existing partition-reader misclassification: aomdec censuses zero `value=8/9` blocks on this stream while both probes read fn=rect blocks). Recipe under-specified: encode with and without `--min-partition-size=4` gave identical 5678-byte streams |

Repro, ibc640:

```
ffmpeg -y -v error -f lavfi -i 'testsrc2=s=640x480:r=25' -frames:v 1 \
  -pix_fmt yuv420p -strict -1 -f yuv4mpegpipe /tmp/ibc640.y4m
~/.cache/aom-oracle/build/aomenc --codec=av1 --bit-depth=8 --input-bit-depth=8 \
  --passes=1 --end-usage=q --cpu-used=0 --lag-in-frames=0 --kf-max-dist=1 \
  --limit=1 --threads=1 --tile-columns=0 --enable-rect-partitions=1 \
  --enable-1to4-partitions=1 --min-partition-size=4 --max-partition-size=64 \
  --sb-size=64 --tune-content=screen --enable-intrabc=1 --cq-level=60 \
  --enable-palette=1 --enable-tx-size-search=0 --obu -o /tmp/ibc640.obu /tmp/ibc640.y4m
$PROBE /tmp/ibc640.obu /tmp/ibc640.raw
ffmpeg -v error -i /tmp/ibc640.obu -pix_fmt yuv420p -f rawvideo /tmp/ibc640.ff.raw
cmp /tmp/ibc640.raw /tmp/ibc640.ff.raw
```

## 3. What changed

* `decode_rect4_16_intrabc`: pair reconstruction. Luma is the strip footprint
  from this strip's DV. Chroma, only when `has_chroma`, is the pair block at
  the pair origin. Skip publishes a square-stride zero residual (`side * bh`,
  not `bw * bh` — a VERT strip's stride is 16 and `bw * bh` underruns
  `reconstruct_mc_rect` at index 136).
* `decode_intrabc_owned_rect`: 2:1 rect intrabc (16x8 and up, including the
  32x8/8x32 arms of `decode_block_rect4` and the 16x8/8x16 leaf). Same
  square-stride skip fix. This is the piece that collides with
  `lane-av1-intrabc`'s `decode_intrabc_rect`.
* `decode_rect4_16_strip` chroma arm: `lossless(fctx)` walks two `TX_4X4`s
  per plane (`TxbSet::Chroma4`, `SCAN_4X4`), plane-major, and stamps each
  unit with `record_mi_chroma`. The non-lossless arm is still one
  `ChromaRect8x4`.
* Key-frame `reduced_tx_set` is published on `fctx.reduced_tx_set_inter` so
  the inter coefficient reader an intrabc block calls sees the frame's flag.
  Restored on drop (`ReducedTxRestore`).
* `decode_block_128rect`'s intrabc arms stay on the named refusal. A 256x192
  sb128 cq50 screen encode decoded without entering them.
* Counters: `rect4_16_intrabc_hits`, `rect4_16_lossless_chroma_hits`,
  `reset_rect4_16_pair_hits`. Printed by `decode_probe`.

## 4. Class sweep

The single-read assumption lived only in `decode_rect4_16_strip`. The other
1:4 readers already take the lossless per-TU walk:

* `decode_block_rect4` (`if depth != 0 || lossless(fctx)`). loss320 codes a
  32x32 `VERT_4` at mi(0, 24), inside the now-exact rows 0-127.
* `decode_block_128rect` already tiles a lossless 128-axis block as `TX_4X4`.
  Its 1:4 intrabc arms still refuse by name; no reaching stream in this lane.

`lnone` (aomdec census: zero `value=8/9`) still enters `decode_rect4_16_strip`
six times. Base and lane read the same symbols until mi(26, 20); the split is
the coefficient walk inside a block both sides already classify as a 16x4.
That classification disagrees with aomdec and is pre-existing (partition
reader untouched). First ffmpeg mismatch stays at byte 41120 on both probes.

## 5. Gates

`cargo test -p ec-av1 --offline --lib -- --test-threads=1`, dev profile:

```
a_16x4_intrabc_pair_strip_decodes_pixel_exact ... ok
a_lossless_16x4_chroma_pair_repairs_the_measured_site ... ok
a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips ... ok
every_proven_refusal_names_a_test_that_exists ... ok
the_decode_path_refuses_exactly_the_listed_cases ... ok
```

The old rect-strip gate's cq50 arm now expects a decode and compares pixels.
`refused == 0`. The inventory string remains, only in `decode_block_128rect`.
`cargo build -p ec-av1 --offline`: 0 warnings.

## 6. Deferred

* **128-level rect intrabc.** `deferred(a reaching sb128 screen stream that codes a 128-level HORZ/VERT/1:4 intrabc strip)`. Named refusal kept. Does not need `lane-av1-intrabc`'s `decode_intrabc_rect`: that helper's chroma is `bw/2 x bh/2` of the strip, wrong size and origin for a 1:4 pair, and it does not cover a 64x16/16x64 strip inside a 128 SB.
* **Full loss320 frame byte-exact.** `deferred(square 64x64 lossless desync, pre-existing)`. The first wrong pixel (row 128 col 160) sits in a 64x64 `NONE` at mi(32, 32) (`bsize=12 value=0`), not a 1:4 strip. Shared `EC_ISTEP` matches at mi(32, 0) including rng; the first real rng split is mi(48, 16) `skip` (37192 vs 60308). Base already fails this stream at byte 108. The gate pins the repaired region.
* **VERT_4 pixel witness.** `deferred(a txs0 recipe that codes PARTITION_VERT_4 intrabc and completes)`. ibc640 and smptebars 640x480 cq45 coded zero `bsize=6 value=9`. allintra codes four `VERT_4` and then refuses on the unrelated var-tx mixed-leaf string, after `rect4_16_pair: intrabc=22`. The VERT arm is the same function with the axes swapped.

## 7. Merge note

Keep `decode_rect4_16_intrabc` and the lossless pair walk. At the three 2:1
call sites, prefer `lane-av1-intrabc`'s `decode_intrabc_rect` if that branch
lands first; drop `decode_intrabc_owned_rect` in that case. Do not point the
1:4 pair at `decode_intrabc_rect`.

Two riders on "prefer theirs":

1. The sibling's `decode_intrabc_rect` at `bfb06b9b` passes
   `ZERO_RESIDUAL[..bw*bh]` at stride `side` in its skip arm while
   `reconstruct_mc_rect` reads `side*bh` rows — "prefer theirs" is only safe
   after the sibling's skip-stride fix lands.
2. This lane wires a FOURTH `owned_rect` call site (`decode_block_rect64`,
   the 64-level 4:1 strips) the sibling does not cover, so dropping
   `decode_intrabc_owned_rect` wholesale requires rewiring that site.

## 8. Suite

Run hub-supervised on the committed tree (`4b73d07f`, `git status` clean)
after the builder's model seat died post-commit:

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1r14 TMPDIR=$HOME/tmp-av1r14 \
  cargo test -p ec-av1 --release --lib -- --test-threads=1

test result: ok. 603 passed; 0 failed; 60 ignored; 0 measured; 0 filtered out; finished in 1505.38s
```

No aomenc pipe deadlock fired this run.
