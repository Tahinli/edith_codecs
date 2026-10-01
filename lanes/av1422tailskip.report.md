# lane-av1422tailskip — `s422_384x240` is FIXED: the reserved arm was not the writer

**Outcome in one line: the cell is byte-exact on all 16 displayed frames (and all
17 decode-order frames) against ffmpeg 8.1.3; the cause was NOT the group-tail
inter chroma SKIP arm at `decode.rs:43948` that three lanes reserved and this one
was chartered to fix, but the **4:2:2 intra-in-inter PALETTE chroma per-unit
window** in `decode_inter_block`'s `side == 64` TX_32X32 chroma walk — the second
chroma transform unit found no palette prediction pending and silently fell back
to the edge DC prediction.**

Branch `lane/av1422tailskip`, worktree `/home/tahinli/.cache/wt/av1422tailskip`,
base `main` = `0c77bad5`. The 4:2:2 sequence-header refusal
(`stream.rs:1803`, `if seq.subsampling_x != seq.subsampling_y`) is **untouched
and in place**; every 4:2:2 measurement below was taken on a local
patch-run-restore bypass and the patch was reverted before the commit. No push.

---

## 0. Why the reserved arm was the wrong owner

The chartered site, `decode.rs:43948` (`decode_inter_block`'s single-reference
`push_mc_rect(1, cpx, cpy, chroma_stride, write_chroma_w, write_chroma_h, su,
ZERO_RESIDUAL)`), **does** fire at the 64x64 mi(32,0) geometry — from decode
frame **2** onward. It never fires for decode frame 1, the first divergent frame.

The oracle settles it. `EC_DP` (per-block rung, private build) on the pinned
cell:

```
line  236  pict 0  EC_DP mi_row=32 mi_col=0 bsize=12 inter=0 chroma_ref=1 skip_txfm=0
line 2298  pict 1  EC_DP mi_row=32 mi_col=0 bsize=12 inter=0 chroma_ref=1 skip_txfm=0
line 7074  pict 2  EC_DP mi_row=32 mi_col=0 bsize=12 inter=1 chroma_ref=1 skip_txfm=1
line 12200 pict 3  EC_DP mi_row=32 mi_col=0 bsize=12 inter=1 chroma_ref=1 skip_txfm=1
```

bsize 12 is `BLOCK_64X64` (`common_data.h`'s `block_size_wide[12] = 64`,
`block_size_high[12] = 64`). **Decode frame 1's block at mi(32,0) is an INTRA
64x64 block** (`inter=0`, `skip_txfm=0`, `chroma_ref=1`) in the oracle exactly as
in our decoder. Only pictures 2+ code it as an inter SKIP block. So the first
divergence is not an inter prediction at all, and no edit at `decode.rs:43948`
can reach it.

`EC_PIB` (the intra prediction call identity, printed before the palette early
return) on the same block, picture 1:

```
EC_PIB mi_row=32 mi_col=0 plane=1 row_off=0 col_off=0 txw=32 txh=32 mode=0
         use_palette=1 filter_intra=5 bsize=12 px=0 py=0 hbd=0
EC_PIB mi_row=32 mi_col=0 plane=1 row_off=8 col_off=0 txw=32 txh=32 mode=0
         use_palette=1 filter_intra=5 bsize=12 px=0 py=32 hbd=0
```

`use_palette=1` — and that is why the oracle's prediction rungs print nothing
for this block: `av1_predict_intra_block` (`reconintra.c:1735`) returns at the
palette branch, before `EC_PREDOUT8`'s `build_directional_and_filter_...` site.
Picture 0's twin prints `use_palette=0` and does emit `EC_PREDOUT8`
(`sum=92160`, flat 90 — a DC prediction), which is what made the earlier lanes
read this as a DC intra block.

## 1. The cause

At 4:2:2 the 64x64 block's chroma plane block is **32x64**, but
`av1_get_max_uv_txsize` adjusts `max_tx_size_rect_lookup[BLOCK_32X64] = TX_32X64`
down to **TX_32X32**, so it codes **two stacked square units per plane**.
`decode_inter_block`'s `side == 64` walk armed the palette prediction **once,
before the loop** (`decode.rs:45640` for U, `:45776` for V), handing the whole
2048-entry block-sized buffer to the first unit. `take_palette_pred` is a
`.take()`: the second unit found nothing pending and reconstructed from the
block edge (DC) instead. Measured with `EC_DEBUG_PAL=1`:

```
PALSET len=2048 stale=false at crates/ec-av1/src/decode.rs:45641:21   (U, whole 32x64)
PALTAKE len=Some(2048)  at crates/ec-av1/src/decode.rs:4072:16        (unit 0 takes it all)
PALTAKE len=None        at crates/ec-av1/src/decode.rs:4072:16        (unit 1: nothing)
PALSET len=2048 stale=false at crates/ec-av1/src/decode.rs:45777:21   (V)
PALTAKE len=Some(2048)
PALTAKE len=None
```

Exactly one 4:2:2 intra-in-inter palette chroma block of that shape exists in
the cell, and its `PALSET len=2048` count over the whole decode is **2** (U and
V). The pixel consequence is precisely the measured box: unit 0 (chroma rows
128..159) matches the oracle, unit 1 (rows 160..191) is flat 90/240 wherever the
palette differs.

## 2. The fix

`decode.rs`, `decode_inter_block`, the `side == 64` arm's per-unit loop, both
planes: arm **this unit's window** on the block-sized buffer instead of the
whole buffer once — the helper the split-transform chroma paths already used
(`palette_window`, whose own doc names this exact failure mode).

```rust
if let Some((ub, _)) = &palette_uv_bufs {
    set_palette_pred(
        palette_window(ub, chroma_w, 0, cu_row * cu_h, cu_w, cu_h),
        fctx,
    );
}
hit!(PALETTE_422_UNIT_WINDOW_HITS);
```

Nothing else in the tree changed: the pre-loop arm stays for the `chroma_w ==
chroma_h` (4:2:0 / 4:4:4) branch, whose single unit IS the whole plane block,
and for the `skip` arm, which pushes one 32x64 chroma unit.

## 3. The cell, before and after (per plane, per frame, ffmpeg 8.1.3)

Comparator: `ffmpeg -v error -i s422_384x240.obu -f rawvideo -pix_fmt yuv422p -`
in DISPLAY order against `decode_probe`'s `EC_PROBE_OUT`; the decode-order
table below is `EC_AV1_PREFILT_WIDE_DUMP` (mi-aligned true extents) against
aomdec's `EC_AV1_PREFILT_DUMP`, both decode order. Geometry is read from the
sequence header by ffprobe, never from a file size; 17 decode frames, 16
displayed (`decode 1` is the hidden altref), map
`0 HID 7 3 1 2 5 4 6 11 9 8 10 13 12 14 15`.

### 3.1 Display order vs ffmpeg — wrong samples per plane

| display | Y | U | V | | display | Y | U | V |
|---|---|---|---|---|---|---|---|---|
| 0 | 0 | 0 | 0 | | 8 | 0 | **96** | **104** |
| 1 | 0 | 0 | 0 | | 9 | 0 | **138** | **150** |
| 2 | 0 | 0 | 0 | | 10 | 0 | **210** | **210** |
| 3 | 0 | 0 | 0 | | 11 | 0 | **250** | **258** |
| 4 | 0 | 0 | 0 | | 12 | 0 | **292** | **303** |
| 5 | 0 | 0 | 0 | | 13 | 0 | **290** | **303** |
| 6 | 0 | **4** | **12** | | 14 | 0 | **248** | **261** |
| 7 | 0 | **50** | **60** | | 15 | 0 | **202** | **210** |
| | | | | | **total** | **0** | **1780** | **1871** |

**After the fix: 0 / 0 / 0 on every one of the 16 displayed frames** —
`cmp mine/fin4.yuv ff.yuv` reports the two files byte-identical (2 949 120 B).

### 3.2 Decode order, pre-filter reconstruction vs aomdec

| decode | 0 | 1 | 2 | 3–7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| before Y/U | 0/0 | 0/**180** | 0/**48** | 0 | 0/**4** | 0/**246** | 0/**136** | 0/**92** | 0/**202** | 0/**290** | 0/**290** | 0/**248** | 0/**202** |
| after Y/U | 0/0 | 0/0 | 0/0 | 0 | 0/0 | 0/0 | 0/0 | 0/0 | 0/0 | 0/0 | 0/0 | 0/0 | 0/0 |

17/17 decode frames byte-exact per plane after the fix (V included). The first
divergence box, before: U and V rows **170..189**, cols **23..31**, 180 wrong
samples per plane — ours flat 90 (U) / 240 (V), the oracle's palette. Luma was
already exact in all 17 and stays exact.

### 3.3 Comparator liveness (the flip control)

One oracle sample's low byte flipped, per plane, at display frame 4, baseline
0/0/0: **+1 in exactly that plane of exactly that frame, 0 in all 47 other
plane/frame slots.** A 0/0/0 here is therefore a measurement, not a
non-comparison. (The same control is what
`scripts/cmpff.py`'s `flip()` performs, and the sweep below runs every cell
through that instrument.)

## 4. Regression sweeps (both binaries, same machine, same oracle)

Two independent instruments, base = `0c77bad5`'s `decode.rs`, fixed = this
branch; the only difference is the two window lines and the counter.

1. **Committed corpus, 115 fixtures** (`crates/ec-av1/fixtures/*.obu`), display
   order, per plane per frame vs ffmpeg: **0 of 115 cells changed** — 78
   byte-exact under both, and every other cell's verdict and totals identical
   between base and fixed.
2. **The 4:2:0 / 4:2:2 / 4:4:4 census sweep cells**
   (`~/.cache/census422b/sweep/*.obu`, 54 cells) through the repo's own
   comparator (`EC_AV1_FINAL_DUMP`, decode order, `scripts/cmpff.py`'s derived
   decode→display map): **48 byte-exact, 5 still diverging, 1 instrument failure
   (`s422_416x250_10b` — "luma identity never covers the display side", the same
   failure under both binaries) — and exactly ONE cell changed:**

   ```
   s422_384x240.obu  DIVERGES [0, 1780, 1871]  ->  BYTE-EXACT [0, 0, 0]
   ```

   The five still-diverging census cells, identical under both binaries (another
   lane's): `s422_320x246` 0/7810/5531, `s422_322x240` 0/22003/21625,
   `s422_322x246` 0/3437/2222, `s422_352x242_10b` 0/4653/3777, `s422_416x242_10b`
   0/5740/4348.
3. **The committed 115 through the repo's own comparator** (`EC_AV1_FINAL_DUMP`,
   decode order, `cmpff.py`) for the 56 cells that instrument finished inside its
   run budget: every verdict identical between base and fixed. That run stopped
   at its own 3000 s cap, not at a failure — `cmpff.py`'s decode→display
   assignment search is exponential in the display-frame count and the long
   `ii-flake` / `all_side` cells did not finish. Those cells are still covered
   by instrument 1 above, which compared all 115 and found no change.
4. **All 169 cells (115 committed + 54 census), base vs fixed, BYTE IDENTITY**
   with no oracle at all: both binaries run with `EC_AV1_FINAL_DUMP` (decode
   order, hidden altref included) plus the display-order `EC_PROBE_OUT`, and
   every dumped byte of every frame is compared. Two cells differ and only one
   of them is real:

   * `census/s422_384x240.obu` -- **the target**, as intended.
   * `corpus/440_request_is_422.obu` -- **not a change: that cell decodes
     NON-DETERMINISTICALLY.** Six runs of the two binaries over it produced six
     distinct md5s, three of them from the *same* binary:

     ```
     e5db1cf5… rep_base_1    147dbacf… rep_base_2    23361e34… rep_base_3
     40d83d5b… rep_fixed_1  975a71a5… rep_fixed_2  a8acc479… rep_fixed_3
     ```

     It is the hand-built 2014-byte 4:2:2 witness (a profile-2 4:4:0 request;
     ffprobe and ffmpeg both refuse to read it, so it has no oracle), it reads
     `intra_in_inter_palette: y=0 uv=0` -- so this lane's palette arm does not
     even run on it -- and its differing bytes are 255<->0 swaps. That is the
     unwritten-plane-sample class (`decode.rs`'s `fresh_plane` deliberately hands
     out uninitialised storage, and `EC_AV1_PLANE_SENTINEL` exists to catch it).
     Pre-existing, independent of this lane, named here rather than fixed:
     `the_440_cell_is_not_a_codable_chroma_shape` -- the only gate pinning those
     bytes -- asserts the (1,0) header and the refusal by name, never pixels,
     and passes.

Named cells from the charter:

| cell | before | after |
|---|---|---|
| `s422_384x240` (the target) | DIVERGES 0/1780/1871 | **BYTE-EXACT 0/0/0** |
| `s422_384x242` | BYTE-EXACT | BYTE-EXACT |
| `s422_384x246` | BYTE-EXACT | BYTE-EXACT |
| `s422_320x240` | BYTE-EXACT | BYTE-EXACT |
| the frame-0 4:2:2 cells (`s422_416x242_10b`, …) | DIVERGES 0/5740/4348 | DIVERGES 0/5740/4348 (unchanged — another lane's) |

## 5. The gate

`stream.rs::the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact`
plus the pinned fixture `crates/ec-av1/fixtures/422_palette_intra_in_inter_384x240_17f.obu`
(19 344 B, sha256 `bf1c658e9bfdc13ec1ed51e59049d2b81201941b6ef4571f8382f356ea4b815b`,
fnv1a64 `0x923ccabfcb933e90`; the sweep artifact
`/home/tahinli/.cache/census422b/sweep/s422_384x240.obu`, unchanged).

The counter is `decode::palette_422_unit_window_hits()` — the number of 4:2:2
intra-in-inter chroma transform units that took their own window. It reads 0 on
a 4:2:0 or 4:4:4 frame (their chroma unit is the whole plane block) and 0 on a
4:2:2 block with no palette, so a witness gate reading 0 proves the arm never
ran.

**Two branches, and why.** `decode_stream` still refuses 4:2:2 at the sequence
header — the standing family rule, untouched here — so the committed tree's
run takes the refusal branch: it asserts the byte pin and that the refusal is
BY NAME, prints that the exactness claim could not be exercised, and asserts the
counter reads 0 so the pin alone can never fake a green. On the local
patch-run-restore build the same test takes its other branch and reports:

```
the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact:
  17 decode-order frame(s) byte-exact against aomdec (1 hidden),
  16 chroma palette unit window(s)
```

**Red-before-green, measured.** With the two `palette_window(...)` calls
replaced by the pre-fix `ub.clone()` / `vb.clone()` and everything else
unchanged, the same command fails:

```
422_palette_intra_in_inter_384x240_17f.obu: decode-order frame 1 of 17
(16 shown, 1 hidden) differs from the oracle at byte 124631 (ours 90 vs 89),
412 bytes differ
```

## 6. Not claimed / open

* **The armed-counter half of the gate is deferred to the 4:2:2 header lift.**
  The arm is reachable only at ss (1,0), and `decode_stream` refuses that at the
  sequence header, so a committed fired-counter assertion is impossible without
  re-committing the bypass the family rule forbids. The counter, the fixture, the
  pin and both branches of the gate are in place; the moment the lift lands, the
  test already asserts 17 decode-order frames byte-exact **and**
  `palette_422_unit_window_hits() > 0`, with no further edit to this lane.
* **The rest of the palette class was not audited arm by arm.** ~50
  `set_palette_pred` sites exist; the ones reachable on the committed corpora are
  covered by the sweeps above (all 115 fixtures and all 54 census cells, byte
  for byte unchanged). The untested remainder is "a block-sized palette buffer
  armed once in front of a MULTI-unit walk" — the class this fix is one instance
  of — and it is reachable only through shapes no committed fixture codes today.
  Named here rather than claimed closed.
* **`corpus/440_request_is_422.obu` decodes nondeterministically** — six runs,
  six md5s, three of them from one binary (§4.4). Unwritten plane samples reach
  that hand-built 4:2:2 witness's output. Found by this lane's byte-identity
  sweep, not caused by it; `EC_AV1_PLANE_SENTINEL` is the instrument that would
  localise it. It has no oracle (ffprobe and ffmpeg both refuse the stream) and
  no pixel gate. `deferred(the 4:2:2 lift owns every 4:2:2 pixel claim, and this
  cell is reachable only through that bypass)` — named, not fixed.
* The oracle binary in `~/.cache/aom-oracle/build/aomdec` is **stale** relative
  to its own source: `EC_DP` does not fire in it (the rung is at
  `decodeframe.c:983`), and its `AOM_CPRED` prints block chroma extents the
  current source does not print for the plane. Every structural reading in §0/§1
  was therefore taken from a private rebuild
  (`~/.cache/aom-oracle-tailskip`, built from the current source), whose pixels
  agree with ffmpeg 0/0/0 on every shown frame of this cell under the display
  map.
* Measurements taken on a local patch-run-restore bypass (never committed, the
  refusal restored in the commit's tree): the §3.2 table, the fired-counter
  branch of §5, and the red-before mutation's oracle comparison.