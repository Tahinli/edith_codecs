# lane-av1-txbands — warped byte-exact; the key-frame TXFM bands are published by every block

Base: `main` **3ecd4a10** (wave 1 merged). Worktree `../edith_codecs-av1txb`, branch
`lane-av1-txbands`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1txb`,
`TMPDIR=$HOME/.cache/tmp-av1txb`. Nothing pushed.

Three items, all landed: (1) the key-frame tx-depth/var-tx context now reads the
real `TXFM_CONTEXT` bands, which an intra-only frame now maintains like libaom
does — **`warped.obu` is byte-exact end to end**; (2) the sub-8x8 group's chroma
keying (`decode_leaf_split4`'s `last_intrabc`) — found by that same stream, whose
luma was already exact while only chroma moved; (3) the `intrabc_chroma_tx` slot
leak — two reachable routes, both fixed, both witnessed by a hand-written tile.
One **pre-existing** defect found and deferred (a panic, §7), stated as such.

---

## 1. Headline: `warped` is byte-exact, and it moved 4.5x on `allintra`

`aomenc --enable-warped-motion=1 --cpu-used=0` over `testsrc2 320x240 rate 8 dur 3`:

```
$PROBE ~/.cache/av1txr/s/warped.obu /tmp/w.raw          # OK: 24 frames decoded, 320x240
ffmpeg -loglevel error -i ~/.cache/av1txr/s/warped.obu -pix_fmt yuv420p -f rawvideo /tmp/w.ref.raw -y
cmp /tmp/w.raw /tmp/w.ref.raw                            # identical
sha256 196fdd8be1c5c04a6e3f031892b55deb9606e7ba8c89dad196b11c27e8395162   (both files, 2764800 B)
```

On the merged base that same command **REFUSES** in frame 0
(`intra block copy on a HORZ/VERT/1:4 rect intra strip`), which is a desync
symptom: with the context bands wrong the tile's symbol stream walks off into a
shape it cannot reconstruct.

`allintra.obu` (aomenc `--allintra --cpu-used=6`) still stops, but far later.
Measured with the `intrabc` symbol ladder against the instrumented `aomdec`
(`EC_TRACE_MODE_STEP=1`, one `name=intrabc` token per block, both decoders):

| build | intrabc tokens decoded | first divergence from aomdec |
|---|---|---|
| merged base | 650 | token **570**, `aom=mi(28,64)` vs `base=mi(26,68)` |
| **this lane** | 2658 | token **2593**, `aom=mi(32,24)` vs `lane=mi(36,22)` |
| aomdec (reference) | 10315 | — |

So the first divergence moved from 570 to 2593 (4.55x further), and what stops
`allintra`/`allintra0` now is a **named refusal of its own**
(`intra block copy on a HORZ/VERT/1:4 rect intra strip …`, `decode.rs`'s rect
mode reader) at mi(32,68) in frame 0 — the rect-strip intrabc reconstruction
gap, which is **lane-av1-intrabc's** (Kubra owns that reader; coordinated, she
keeps the write side off her plate — see §9).

## 2. What changed

### 2.1 The bands are now maintained on intra-only frames too

libaom runs `set_txfm_ctxs(mbmi->tx_size, xd->width, xd->height,
mbmi->skip_txfm && is_inter_block(mbmi), xd)` at the end of **every**
`parse_decode_block` (`decodeframe.c:1229`), an intra-only frame's blocks
included, and `get_tx_size_context` (`pred_common.h:342`) plus
`txfm_partition_context` read those two arrays. This decoder published them on
inter frames only, and read them on inter frames only: an intra-only frame's
`tx_depth`/`txfm_split` context came off the **deblock grid**
(`tx_px_at`/`tx_h_px_at`), an approximation that coincides with the bands only
while every neighbour codes one whole-block transform.

Now:

* `FrameCtx::intra_only` (new `Cell<bool>`) is set in
  `decode_key_frame_tile_with_cdfs` and cleared in `set_inter_tx_mode` (the one
  place the inter tile decoder arms its own frame state).
* `Neighbours::fill_lf_grid_rect` — the deblocker's own grid, called by **every**
  block body with the resolved transform and the block's mi span — publishes the
  same `(tx_w, tx_h)` over the same span into `above_txfm`/`left_txfm` when
  `intra_only` is set, and stamps `above_inter`/`left_inter` false over that
  span. That is what makes the coverage complete without threading a new call
  through ten block bodies; the two bodies that can code an **intrabc** block
  keep their explicit `set_txfm_ctxs` (still `allow_intrabc`-gated) because
  libaom's `skip && is_inter` term makes a *skipped intrabc* block publish its
  BLOCK size, which the deblock grid cannot express.
* `read_tx_size` always reads `tx_size_context_txfm`; the seven
  `if intra_in_inter_mode.is_some() { …txfm… } else { …deblock… }` sites now
  read `tx_size_context_txfm_rect` unconditionally; `tx_size_context` and
  `tx_size_context_rect` are **deleted** (the deblock approximation survives only
  in the divergence witness counter, §6).
* `record_intrabc_mi_rect` now also stamps `above_inter`/`left_inter` **true**
  over an intrabc block's own span: libaom's `is_inter_block` is
  `is_intrabc_block(mbmi) || ref_frame[0] > INTRA_FRAME` (`blockd.h:373`), so
  `get_tx_size_context` reads an intrabc neighbour's **block size** instead of
  its transform band. This is the whole of the `(46,66)` divergence: the block
  above it is an intrabc `BLOCK_8X4` at mi(45,66) whose band was 4 while its
  block width is 8.

### 2.2 `decode_leaf_split4`: chroma is keyed on the CHROMA-REFERENCE leaf

`last_intrabc` was set by **every** intrabc leaf of the 2x2 group and only
cleared by the coefficient branches, so a group whose intrabc leaf was a
non-chroma-reference one (i=2) reconstructed its single chroma unit as a frame
copy at **that** leaf's block vector — where libaom codes one chroma unit under
the last sub-block only (`is_chroma_reference`), so the prediction is leaf 3's
own `uv_mode` (its `cfl_alphas` included). Moved inside `if has_chroma`,
mirroring `decode_leaf_rect8`. `warped.obu`'s 8x8 group at mi(40,62) is exactly
that shape (leaf 2 intrabc, leaf 3 `uv_mode = UV_CFL_PRED`), and it was the
**only** remaining divergence once the bands were fixed: luma byte-exact,
chroma wrong from its first sample (luma byte 89724, chroma row 80).

### 2.3 `intrabc_chroma_tx`: armed routes are now disarmed routes

See §5.

## 3. Regression evidence

**Corpus sweep, base build vs this lane's build, byte-for-byte** (same probe,
private target dirs; `$PROBE <stream> <out.raw>` then `cmp`): all 23 committed
`crates/ec-av1/fixtures/*.obu` produce **identical** plane bytes, and so do 40
generated streams (4:4:4 controls, `plain`, `cdf0`, 16-lossless/blur/box/flat
aomenc recipes, the s2/s3 screen-content attempts). Every 4:4:4 stream still
**refuses by name** (`a chroma format other than 4:2:0`), and the three
generated streams that the base refused with a *desync-induced* string
(`an intrabc block whose var-tx tree resolved to mixed leaf transform sizes`,
`blur-screen-cpu0`) now refuse with the genuine rect-strip-intrabc string —
both still zero bytes.

The one row that differs in that table is the intended one: `warped.obu` base
`REFUSED` → this lane `OK: 24 frames`.

**Refusal/witness probes** (per-lane context): the committed hg_*/troy_*/
palette_*/superres_* 10-bit and screen fixtures all decode byte-exact against
ffmpeg inside the suite (identical to base in the sweep above); the non-4:2:0 and
qmatrix refusals are unchanged. The 10-bit film-window hash probe
(`hbd-r5/hunger.obu`, sha256 `d7214bb7…`) could **not** be re-run: that stream is
no longer on disk anywhere under `$HOME` (`find` for `hunger*.obu`: none). Its
regression role is covered by the committed `hg_*` 10-bit witnesses, which are
byte-identical to the base build and ffmpeg-exact (the suite prints them).

## 4. Item 2 witness

The stream is its own witness: pre-fix it is luma-exact and chroma-wrong
(`cmp` first difference at byte 89724, i.e. chroma row 80 of frame 0), post-fix
the whole 24-frame file is identical to ffmpeg's decode (sha256 above). Nothing
else changed for that stream's symbols (the luma plane was already byte-exact
before the one-line move), which is the point: the slot only ever decided the
chroma prediction's *source*, never a symbol.

## 5. Item 3: the `intrabc_chroma_tx` slot leak — two routes, both fixed

`INTRABC_CHROMA_TX` is the `Cell<Option<TxType>>` that stands in for libaom's
`xd->tx_type_map` at the co-located luma position: it is armed immediately
before an intrabc block's two chroma `read_plane` calls and cleared immediately
after, and the chroma reader picks its scan and inverse transform off it.

**The leak.** An intrabc sub-8x8 group leaves one arm at the *group's*
chroma-reference leaf, but the group's chroma section dispatches
`leaf_skips[chroma_ref]` **first**: a SKIPPED chroma-reference leaf reads no
chroma coefficient at all, so it never reaches the coefficient branch that
would have disarmed the slot — which then stayed armed into the **next** block's
chroma read, changing that block's scan and inverse transform.

Three routes carry the arm-then-skip shape, and the census (every `intrabc_chroma_tx` write on the
production path: four arms, six clears, one reader) is:

| route | arms? | clears? |
|---|---|---|
| `decode_leaf_split4` (4x4 group) | yes, on `has_chroma` | **no** → fixed (disarm + `SKIPPED_INTRABC_CHROMA_ARM_HITS`) |
| `decode_leaf_rect8` (4x8/8x4 group) | yes, on `has_chroma` | **no** → fixed (same) |
| `decode_block` (square) | no arm on the skip route (arm and clear are straight-line in the same branch, `14772`→`14830`) | n/a |
| `decode_leaf8` (8x8) | no arm on the skip route (same straight-line pairing) | n/a |

(The `decode_intra_sub8_leaf` chroma-skip branch, `decode.rs`'s inter-frame
sub-8x8 twin, has no intrabc arm either — intrabc exists on intra-only frames
only.) So the reachable instances are exactly the two fixed above; the
"square-path instance" this lane was sent to sweep does not exist as an
arm-without-clear (it can only *suffer* a leak, which the source-side fix now
removes), and that is stated here rather than papered over with a dead clear.

**Census: no stream in hand reaches the route.** `aomenc` never picked it on any
recipe tried (screen-content / lossless / cpu-used 0..6 / flat, blur, box,
repeated-rectangle and drawn-text sources), this crate's own encoder cannot
(`crate::tile::write_intra_mode` hardcodes `skip = 0`), and the counter reads 0
on every committed fixture and generated stream
(`$PROBE <stream>` prints `skipped_intrabc_chroma_arm_hits:`).

**Witness tests** (two, one per route), driving the kernel with hand-written
symbols exactly as `a_skipped_block_decodes_to_pure_prediction` does — an 8x8
frame descends the superblock cascade to one 8x8 group with no partition symbol
of its own, so the tile is `partition_w8` + four/nTwo leaf symbol sets:

* `a_skipped_intrabc_chroma_reference_leaf_disarms_the_inherited_chroma_tx`
  (`partition_w8 = SPLIT` → `decode_leaf_split4`),
* `a_skipped_intrabc_rect_chroma_reference_leaf_disarms_the_inherited_chroma_tx`
  (`partition_w8 = VERT` → `decode_leaf_rect8`).

Each asserts (a) the route was reached (`skipped_intrabc_chroma_arm_hits() >= 1`)
and (b) the slot is disarmed at the end (`intrabc_chroma_tx` is `None`).

**Fail-pre-fix, measured** (the disarm removed on that route, counter kept):

```
a_skipped_intrabc_chroma_reference_leaf_disarms_the_inherited_chroma_tx ... FAILED
  panicked: the inherited chroma transform slot must be disarmed when its block's chroma
  route ends, or the NEXT block's chroma read picks its scan and inverse transform
```
and identically for the rect8 twin (its own disarm removed → only that test
fails). Restored, both pass.

## 6. Gates

* `cargo check -p ec-av1 --all-targets` — **0 warnings**
  (`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1txb`, `cargo check` after a
  touch-sweep).
* `cargo test -p ec-av1 --lib` (to completion, hub-supervised) —
  **still running at yield time** (hub process `av1txb-suite`, started before the final edits-freeze; the tree it runs on is the tree in the commit). Its scope: the full `ec-av1` lib suite on this branch, expected `600 passed; 0 failed; 60 ignored` (base 598 + this lane's 2 new gates). The scoped runs that DID complete on this tree: `cargo check -p ec-av1 --all-targets` = 0 warnings (forced re-check), both new gates and both fail-pre-fix runs, and the base-vs-lane corpus byte sweep in §3.
* Two new gates (`a_skipped_intrabc_{,}rect_…`) plus their fail-pre-fix runs
  above; the previous baseline was 598 passed / 0 failed / 60 ignored, so this
  lane's target is **600 passed / 60 ignored**.
* Regression probes: §3 (corpus base-vs-lane byte sweep, refusal preservation).
* Divergence witness counter `TXCTX_DEBLOCK_DIVERGENCE_HITS`
  (`decode::txctx_deblock_divergence_hits`) counts blocks where the band read
  resolves to a **different** CDF row than the deleted deblock approximation —
  i.e. exactly the blocks this change fixes. It replaced
  `INTRA_RECT_IN_INTER_TXCTX_OVERRIDE_HITS`, which was inter-frame-only and had
  no reader.

## 7. Found and DEFERRED (not this lane's shape): a lossless 128-axis intra block panics

`decode_block_128rect` has no `lossless` carve-out. libaom's `read_block_tx_size`
returns TX_4X4 for a lossless frame before reading anything, but this body still
reads a 64/32-point transform and then reconstructs with a residual slice sized
for a 4x4 unit:

```
$ ~/.cache/aomenc --codec=av1 --passes=1 --threads=1 --obu --lossless=1 --cpu-used=0 \
      -o /tmp/loss.obu <320x240 y4m>
$ $PROBE /tmp/loss.obu /dev/null
thread 'main' panicked at crates/ec-av1/src/decode.rs:3596:16:
index out of bounds: the len is 16 but the index is 16
  decode::exec_intra <- decode::read_plane <- decode::decode_block_128rect
```

**Pre-existing**: the merged base panics at the same place (its line 3587, the
same site before this lane's edits), and it is reachable via any `--lossless=1`
stream that codes a `BLOCK_128X64`/`BLOCK_64X128` intra block (the panicking
block is `fn=rect bw=64 bh=128` at mi(32,64)). Disposition: **deferred(lossless
128-axis intra tiling)** — the fix is a TX_4X4 per-unit arm (or a named refusal
at this body's head) and belongs with a lossless-shape lane; it is *not* a
regression and is stated here rather than folded silently into a success
summary.

## 8. Repro recipes

```
# build (absolute fixture env; private target dir)
cd /home/tahinli/Documents/Code/Rust/edith_codecs-av1txb
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1txb TMPDIR=$HOME/.cache/tmp-av1txb \
  cargo build --release -p ec-av1 --example decode_probe

# streams (re-derivable)
cd ~/.cache/av1txr
aomenc --codec=av1 --passes=1 --threads=1 --obu --enable-warped-motion=1 --cpu-used=0 -o s/warped.obu s/s420.y4m
aomenc --codec=av1 --passes=1 --threads=1 --obu --allintra --cpu-used=6 -o s/allintra.obu s/s420.y4m
# (s420.y4m = ffmpeg -f lavfi -i testsrc2=size=320x240:rate=8:duration=3 -pix_fmt yuv420p)

# byte-exactness (8-bit: EC_PROBE_OUT16 is yuv420p10le and must NOT be compared
# to ffmpeg's yuv420p10le for an 8-bit source -- ffmpeg left-shifts, the probe does not)
$PROBE s/warped.obu out8.raw
ffmpeg -i s/warped.obu -pix_fmt yuv420p -f rawvideo ref8.raw -y && cmp out8.raw ref8.raw

# base-vs-lane regression sweep
$PROBE <stream> a.raw && cargo-target-base/release/examples/decode_probe <stream> b.raw && cmp a.raw b.raw

# the two witness gates + their fail-pre-fix runs
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1txb cargo test -p ec-av1 --lib -- a_skipped_intrabc

# the divergence ladder (per-name intrabc tokens; rng values COLLIDE, do not align on them)
EC_TRACE_MODE_STEP=1 ~/.cache/aom-oracle/build/aomdec --rawvideo -o /dev/null s/allintra.obu 2>a.step
EC_TRACE_MODE_STEP=1 $PROBE s/allintra.obu /dev/null 2>o.step
grep -oP 'mi_row=\d+ mi_col=\d+ name=intrabc val=-?\d+' a.step > a.ib   # then diff a.ib o.ib
```

## 9. Notes for the next lane

* **`lane-av1-intrabc` (Kubra)**: the band WRITE side is this lane's and is
  landed — rebase on `lane-av1-txbands` rather than adding publishes in the
  rect-strip bodies. `allintra`/`allintra0` now stop at the genuine
  `intra block copy on a HORZ/VERT/1:4 rect intra strip` refusal at mi(32,68)
  frame 0 (was a desync at mi(28,64)); the mixed-leaf var-tx refusal the base hit
  on `blur-screen-cpu0` is no longer reached (that was a desync symptom too).
* `EC_TXCTX` still prints both bands, `above_inter`/`left_inter`, the neighbour
  sides and the new `intra_only` flag; `EC_TXCTXDBG` was removed with the
  temporary probe print it served.
* The deblock approximation is gone from the read path but still computed (as
  `tx_px_at`/`tx_h_px_at`) for the divergence counter and for the deblocker
  itself; deleting it wholesale means deleting that counter.
