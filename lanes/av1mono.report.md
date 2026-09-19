# lane-av1-mono (WIP) — monochrome AV1 key-frame decompiles; luma not yet byte-exact

Base: `main` **f0727a87** (2026-09-19). Worktree `edith_codecs-av1mono`, branch
`lane-av1-mono`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1mono` (private).
No push. Status: **INCOMPLETE** — entropy now syncs through a real monochrome
key frame (the pilot's first-wall refusal is gone), but the reconstructed luma is
NOT yet byte-exact against ffmpeg; a small block region diverges.

## TL;DR

The pilot (`lanes/av1decode.report.md`, branch `lane-av1-decode`) root-caused the
first monochrome stop to libaom `decodemv.c:933`'s
`!monochrome && is_chroma_ref` guard on the chroma mode symbols. This lane ports
the monochrome plane model into ec-av1:

- new `FrameCtx::mono_chrome` (from the sequence header's `color_config.mono_chrome`),
- every chroma **symbol** read gated (uv_mode/cfl/angle_uv/palette_uv),
- every chroma **coefficient** read gated (`read_plane`/`read_inter_plane`/
  `read_inter_plane_rect`/`decode_rect_split`'s un-split `read_coeffs_rect`),
- the probe writes a single luma plane for a mono sequence.

**Result: the gray/key-frame refusal `a Golomb tail longer than this decoder
reads` is GONE — `decode_probe` now returns `OK: 60 frames decoded` for both the
pilot's gray recipe and `fixtures/bitstreams/av1-monochrome.ivf`.** The 4:2:0
twin still decodes 60/60.

**Remaining defect:** the decoded luma is NOT byte-exact. First divergence on the
pilot's gray stream is frame 0, row 0, column 37 (off-by-one), and a small
~6×2-pixel block region at row 1, cols 36-42 decodes wrong (probe 20/19 vs ref
76). Because entropy stays in sync (all 60 frames decode, no refusal) this is a
**luma reconstruction/prediction** defect, not an entropy desync — most likely a
block whose intra prediction reads a neighbour state that the mono path no longer
updates, or a residual/palette interaction on a chroma-adjacent block. This needs
one more round.

## Changes (uncommitted→committed on `lane-av1-mono`)

`crates/ec-av1/src/decode.rs`
- `FrameCtx::mono_chrome: Cell<bool>` + `mono(fctx)` / `set_mono(...)` helpers;
  copied in `filter_ctx_copy`.
- `read_intra_mode_rect`: `has_chroma &= !mono` (gates uv_mode/cfl/angle_uv/palette_uv).
- `read_intra_mode` (square), `decode_intra_rect_in_inter`, `decode_inter_block`
  intra-in-inter, `decode_inter_block8` intra-in-inter: explicit mono branch on
  the chroma mode read + palette_uv gate.
- `read_intra_mode_sub8`, `decode_intra_sub8_leaf`: `has_chroma &= !mono`.
- `read_plane`, `read_inter_plane`, `read_inter_plane_rect`: early-return
  `Grid::Zero` for `plane_idx > 0 && mono` (no coefficient symbol consumed).
- `decode_rect_split`: whole chroma section bypassed for mono (its un-split path
  read chroma through `read_coeffs_rect` directly, NOT through `read_plane`, so
  the reader gate did not cover it — this was a real desync source).
- `decode_leaf_rect8` / `decode_leaf_split4`: chroma reconstruction tail
  bypassed for mono (the `last_uv.expect("…has_chroma…")` panics otherwise).

`crates/ec-av1/src/stream.rs`
- `SeqFlags::mono_chrome` from `sequence_header().color_config.mono_chrome`;
  `set_mono(...)` called per frame in `decode_frame` (like `set_bit_depth`).

`crates/ec-av1/examples/decode_probe.rs`
- prints `mono_chrome` in the `SEQ:` line; writes only the luma plane when the
  sequence is monochrome (so the raw dump is what `ffmpeg -pix_fmt gray -f
  rawvideo` writes).

## Repro

```
# generate (pilot recipe)
ffmpeg -v error -f lavfi -i "testsrc2=size=320x240:rate=30:duration=2" \
  -pix_fmt gray -c:v libaom-av1 -cpu-used 8 -b:v 300k -f obu -y /tmp/gray.obu
ffmpeg -v error -i fixtures/bitstreams/av1-monochrome.ivf -c:v copy -f obu -y /tmp/mono.obu

P=$HOME/.cache/cargo-target-av1mono/release/examples/decode_probe
EC_PROBE_OUT16=$HOME/m.raw16 $P /tmp/gray.obu        # OK: 60 frames (was REFUSED)
ffmpeg -v error -i /tmp/gray.obu -pix_fmt gray -f rawvideo $HOME/ref.gray
python3 -c "d=open('$HOME/m.raw16','rb').read();print(bytes(d[0::2])==open('$HOME/ref.gray','rb').read())"
# -> False today; first differing byte at index 37.
```

## Verified

- `decode_probe` gray recipe: `OK: 60 frames decoded, 320x240` (was REFUSED).
- `decode_probe` `av1-monochrome.ivf` (remuxed to OBU): `OK: 60 frames` (was REFUSED).
- 4:2:0 twin of the same recipe: `OK: 60 frames` (unchanged).
- Byte-exactness vs ffmpeg gray: **FAILS** (see above).

## NOT done (blocking acceptance)

- Byte-exact luma on the three required witnesses (gray recipe, av1-monochrome.ivf,
  a 60+ frame inter gray encode). The key-frame luma mismatch must be found first.
- Inter-frame mono streams not yet exercised past the key frame (an earlier run
  panicked in `mc.rs:1141` "a reference plane has samples" when the mono Picture
  carried empty u/v; that was fixed by keeping the chroma scratch planes in the
  Picture and making the probe — not the Picture — luma-only. Inter mono still
  needs a byte-exactness gate.)
- The pilot's gate `a_real_libaom_monochrome_key_frame_is_refused_by_name`
  (branch `lane-av1-decode`, not on main) must be INVERTED to a pixel-exact
  witness once the luma defect is fixed.
- `cargo test -p ec-av1 --lib` suite + `cargo check -p ec-av1 --all-targets`
  0-warning gate not run this session.
- Refusal inventory count not updated.
- Film-window probes not re-run on this build.

## Deferred / not chased

- The other 34 refusals (pilot terrain map Wave 2/3) — untouched, per charter.
