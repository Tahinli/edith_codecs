# lane-av1-lossless128 — the lossless 128-axis intra panic is closed (byte-exact)

Base: `main` **46f07214**. Worktree `../edith_codecs-av1ll`, branch
`lane-av1-lossless128`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1ll`,
`TMPDIR=$HOME/.cache/tmp-av1ll`. Nothing pushed.

## 0. WIP assessment (cancelled agent, kept)

The uncommitted WIP (`decode.rs`, `stream.rs`, `examples/decode_probe.rs`, plus
an untracked report and a gitignored fixture) was a **coherent** fix of
`decode_block_128rect`, not a half-applied edit. It already:

* skipped the `tx_size_cat3` read on a lossless frame,
* walked every plane as TX_4X4 (`TxbSet::Luma4` / `Chroma4`),
* made the chroma walk plane-major inside each 64x64 mu chunk (a no-op when
  `cn == 1`, i.e. every non-lossless block),
* bumped `INTRA128_LOSSLESS_HITS`, and
* added `a_lossless_sb128_rect_intra_block_decodes_sample_exact`.

What it had not done: prove the gate, force-add the gitignored fixture, run
the wave-1 probes, or run the suite. Those are this resume. The three files
were **not** reset.

## 1. Headline

A lossless key frame that codes an intra `BLOCK_64X128` no longer panics. The
committed fixture is sample-exact against ffmpeg, and the fixed body is
reached (`intra128_lossless` hits 2).

```
$PROBE crates/ec-av1/fixtures/lossless_sb128_rect_kf.obu out.raw
OK: 4 frames decoded, 320x256
intra128_lossless: 2
ffmpeg -loglevel error -i crates/ec-av1/fixtures/lossless_sb128_rect_kf.obu \
       -pix_fmt yuv420p -f rawvideo ref.raw -y
cmp out.raw ref.raw          # identical
sha256 of the planes: b30015ecfdd5091a02be1b1d92e467fc63d65e0fd6775fc17e01589b2f7edc44
sha256 of the .obu:   013b07979c8be999c70d47f32768c2bf959be086a893f60dde1b0f5e35d4f475
```

On the merged base (`$HOME/.cache/av1ll-base`, probe
`$HOME/.cache/cargo-target-av1ll-base`) the same fixture **panics** (rc 101):

```
thread 'main' panicked at crates/ec-av1/src/decode.rs:3596:16:
index out of bounds: the len is 16 but the index is 16
  decode::exec_intra <- decode::read_plane <- decode::decode_block_128rect
```

The txbands repro (`aomenc --lossless=1 --cpu-used=0 --sb-size=128` over
testsrc2 320x240, 24 frames) also panics on that base probe and decodes all
24 frames on this lane (`intra128_lossless: 13`). It is **not** sample-exact
against ffmpeg — the first wrong luma sample is frame 0 col 108, and that
desync is the pre-existing 16x4 chroma bug in §6, not this panic. Base and
this lane consume the same mode-symbol ladder up to the 128-axis block
(identical `EC_ISTEP` through `mi(0,64)`); they diverge only inside that
block, which is the fix.

## 2. Root cause

`decode_block_128rect` had no lossless carve-out. libaom forces TX_4X4 for a
`CodedLossless` frame before any tx-size syntax exists (read in
`~/.cache/aom-oracle/src`):

* `read_tx_mode` returns `ONLY_4X4` when `coded_lossless`
  (`av1/decoder/decodeframe.c:141`);
* `read_tx_size` returns `TX_4X4` before it looks at anything
  (`decodeframe.c:1183`);
* `av1_get_tx_size` returns `TX_4X4` for every plane of a lossless `xd`
  (`av1/common/blockd.h:1381`), so chroma is 4x4 too;
* `decode_token_recon_block` then walks each 64x64 mu chunk plane-major
  (`decodeframe.c:982-1006`): all Y, then all U, then all V, clipped to
  `max_block_wide/high`.

With `tx_select == false` the old body took `depth = 0`, hence
`logical_tx = 64`, and called `read_plane` with a 64-point geometry.
`TxParams::run` on a lossless frame always returns the 16-sample
Walsh-Hadamard residual (`dequant_and_inverse_wht4x4`), and
`PlaneBuf::reconstruct` indexes that slice with the 64-point side
(`residual[idx]`, `dense_residual` does not pad a non-empty slice). Index 16
on a len-16 slice. A panic on a conformant stream.

The committed fixture's 128 roots (aomdec `EC_PART_VAL`, `bsize=15`) are
`PARTITION_NONE` except `mi(0,64)` and `mi(32,64)`, which are
`PARTITION_VERT` (value 2) — two `BLOCK_64X128` blocks. That is the shape the
txbands report named (`fn=rect bw=64 bh=128`).

## 3. Fix

`decode_block_128rect` now mirrors `decode_block`'s lossless path:

* `depth` is read only when `tx_select && !lossless_frame` (no `tx_size_cat3`
  symbol on a lossless frame);
* `logical_tx = 4`, `luma_set = TxbSet::Luma4`, `chroma_tx = 4`,
  `chroma_set = TxbSet::Chroma4`;
* chroma units inside each mu chunk are `cn = 32 / chroma_tx` per axis and
  plane-major (all U, then all V), which is `decode_token_recon_block`'s
  order and is a no-op at `cn == 1`;
* a unit whose top-left is outside libaom's per-chunk
  `unit_width/height = min(mu_blocks + col, max_blocks) >> ss` is not coded;
* `INTRA128_LOSSLESS_HITS` is bumped at the head; the probe prints it.

Non-lossless behavior is unchanged: the 23 previously committed
`crates/ec-av1/fixtures/*.obu` decode byte-identical to the base probe
(§5).

## 4. The gate (non-vacuous, fails pre-fix)

`a_lossless_sb128_rect_intra_block_decodes_sample_exact` (`stream.rs`):
decodes the committed fixture, asserts `intra128_lossless_hits()` delta > 0,
then compares all four frames' Y/U/V against `ffmpeg_decode_sequence`.

Fail-pre-fix, measured on the base probe (rc 101, panic quoted in §1).

Pass post-fix:

```
test stream::tests::a_lossless_sb128_rect_intra_block_decodes_sample_exact ... ok
  "4 frames sample-exact, intra128_lossless hits 2"
```

Fixture: `crates/ec-av1/fixtures/lossless_sb128_rect_kf.obu`, 3471 bytes,
sha256 `013b07979c8be999c70d47f32768c2bf959be086a893f60dde1b0f5e35d4f475`.
The path is gitignored (`fixtures` in `.gitignore`); it is force-added, same
as the other 23 committed `.obu` files. 320x256, 4 frames, lossless,
`use_128x128_superblock=true`, single tile. The cancelled agent's generator
script is not in the tree; the bytes and the aomenc recipe in the test doc
are what this lane verified (base panics, this lane is sample-exact, sha256
matches).

The panic witness is the **vertical** axis (`BLOCK_64X128`). A bottom-edge
`PARTITION_HORZ` (`BLOCK_128X64`) stream generated here decoded sample-exact
on **both** the base probe and this lane, so it does not exercise the
panicking reconstruct; aomenc did not emit an interior non-skip `PARTITION_HORZ`
128 root on the seams tried. The function is shared (`bw, bh` swapped, same
unit walk). Not separately panic-witnessed.

## 5. Regression evidence

* **23 committed fixtures** (`crates/ec-av1/fixtures/*.obu` except the new
  one): lane probe raw bytes **identical** to the base probe. The only row
  that moves is the new fixture (base rc 101 → lane OK, sample-exact vs
  ffmpeg).
* Wave-1 probes, this lane's `decode_probe`:
  * gray 60-frame key (`libaom-av1`, 320x240, `mono_chrome=true`): `OK: 60
    frames`, luma `cmp` identical to `ffmpeg -pix_fmt gray`.
  * `fixtures/bitstreams/av1-monochrome.ivf` remuxed to OBU: `OK: 60 frames`,
    luma identical to ffmpeg gray.
  * `av1-profile1-444.ivf` remuxed to OBU: refused by name (`a chroma format
    other than 4:2:0`).
  * `aomenc --enable-qm=1` (160x96, 4 frames): refused by name (`a frame
    using quantisation matrices`). The `--enable-qm=0` control of the same
    source is sample-exact vs ffmpeg.
  * `~/.cache/av1txr/s/cdf0.obu` (`disable_cdf` stream, 3 frames, 320x240):
    sample-exact vs ffmpeg `yuv420p`.
  * `hg_kf900.obu` (3840x1608, 10-bit, 1 shown frame): `EC_PROBE_OUT16` cmp
    identical to `ffmpeg -pix_fmt yuv420p10le` (18524160 bytes). The other
    committed `hg_*` fixtures are in the 23-file base-vs-lane sweep (identical).
* `cargo check -p ec-av1 --all-targets`: **0 warnings**.
* A 640x256 lossless `--sb-size=128 --tile-columns=1` stream refuses on both
  base and this lane with the existing string `intra block copy on a
  HORZ/VERT/1:4 rect intra strip` (lane-av1-intrabc's shape). Not a regression.
  `intra128_lossless` was 0 on that stream — it never reached this body.

## 6. Found and DEFERRED: lossless 16x4/4x16 chroma is still one rect, not TX_4X4

Pre-existing. Independent of the panic: it reproduces on a stream with no
128-axis block, and the base probe's `EC_ISTEP` ladder matches this lane's
through the divergence (so this lane's edit did not move it).

**Measured on** `aomenc --lossless=1 --cpu-used=0 --sb-size=128` over
testsrc2 320x240, frame 0. aomdec `EC_PART`: the 16x16 at `mi(0,20)` is
`PARTITION_HORZ_4` (value 8) — four 16x4 strips. Mode symbols match through
`mi(1,20) uv_mode` (rng 53536 both sides). The next strip's `skip` does not
(aom rng 50651, ours 56265). That is the chroma-reference strip's coefficient
read: `decode_rect4_16_strip` still reads one `TxbSet::ChromaRect8x4` for the
pair (`decode.rs` around the `read_chroma_coeffs_rect` call at the chroma-pair
arm) where libaom codes two TX_4X4 units per plane. One symbol block too few,
and the tile desyncs from that strip on. First wrong luma sample on the
24-frame file is frame 0 col 108 (the strip at pixel x=104).

`deferred(lossless 16x4/4x16 chroma pair — decode_rect4_16_strip needs the
two-TX_4X4 walk; unblock: a lossless-shape lane).` Different shape, different
symptom (silent wrong pixels, never a panic), and not what the 128-axis body
reads. Not folded into the success summary.

## 7. Not changed

* No refusal string added or removed.
* No encoder change.
* Sibling `lane-av1-intrabc` (Nazli, worktree `edith_codecs-av1ibc`) owns
  intrabc / MV-grid. This lane did not touch those regions. Shared helpers
  (`read_plane`, `exec_intra`, txfm bands) were not modified.

## 8. Suite

`cargo test -p ec-av1 --release --lib -- --test-threads=1`, hub process
`av1ll-suite`, on the committed tree. Expected **601 passed; 0 failed; 60
ignored** (base 600 + this gate). Literal final line filled after the run.
