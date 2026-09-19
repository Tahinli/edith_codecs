# lane-vp9-hbd — profile 2 (10-bit 4:2:0) decode, byte-exact vs libvpx 1.15

Base: `79984549` (main). Branch: `lane-vp9-hbd`. Worktree:
`~/Documents/Code/Rust/edith_codecs-vp9hbd`. Target dir:
`$HOME/.cache/cargo-target-vp9hbd`. Not pushed.

## What changed

One representation for both depths: every plane is `u16` (`ec_vp9::Sample`,
8-bit values in the low byte) and every pixel kernel takes the frame's `bd`
and clamps with `clip_pixel_bd(v, bd)`. The reconstruction walk is single;
8-bit and high-bit-depth kernels are the same transcribed butterflies with
`bd`-scaled clips/thresholds (exactly how libvpx splits its C kernels). No
forked decode paths.

- `decode.rs`: `Planes`/`Picture` are u16 + `bd`; `Planes::new` fills the
  neutral grey `1 << (bd - 1)`; profile gate accepts 0/2, refuses 1/3;
  new bit-depth gate refuses 12-bit by name; subsampling gate (4:2:0 only)
  unchanged and STAYS. Loop filter + intra + transform calls thread `bd`.
- `intra.rs`: u16 staging; `base = 128 << (bd - 8)` replaces the 127/129
  fills; DC/`TM` use `base`/`clip_pixel_bd`.
- `transform.rs`: `clip_add(d, v, bd)`; idct/iadst take `bd` and port
  libvpx's `detect_invalid_highbd_input` guard (zero output on a
  magnitude ≥ 2^25 when bd ≠ 8); butterflies unchanged.
- `mc.rs`: u16 planes + u16 convolve intermediate; `RefPlane.bd`; the
  intermediate clamp is `clip_pixel_bd(v, bd)` (== `clip_pixel` at bd 8).
- `loopfilter.rs`: u16; `sc(x, bd)` == libvpx `signed_char_clamp_high`;
  `limit`/`blimit`/flat/hev thresholds scaled by `1 << (bd - 8)`.
- `tokens.rs`: **the one depth-dependent bitstream element** — the CAT6
  coefficient token. 8-bit: 14 extra bits at `vp9_cat6_prob`; 10-bit (and
  12-bit): 16 extra bits at `vp9_cat6_prob_high12 + 2`. Ported.
- Tests updated for u16 planes; new `hbd_exact.rs`; `scratch_pixdump10.rs`
  (u16-LE dumper harness).

## Fail-pre-fix witness

Pre-fix, `Decoder::decode` on the 10-bit stream returned
`unsupported: vp9 profile 2 — this lane decodes profile 0 only` at
`decode.rs:211`. `hbd_exact.rs::profile2_10bit_is_byte_exact` fails at the
first frame on the pre-fix tree for that reason.

## Oracle

`$HOME/.cache/vp9hbd/` = a fresh libvpx 1.15.0 built with
`--enable-vp9-highbitdepth` (`CONFIG_VP9_HIGHBITDEPTH 1`), plus
`drv_pix10.c` (cropped planes as little-endian u16, frame-indexed,
`PIXDUMP=`). Verified against ffmpeg `rawvideo yuv420p10le`.

## Byte-exactness (cmp of frame-aligned dumps)

| stream | frames | ours vs libvpx 1.15 |
|---|---|---|
| vp9-1080p-23.976-10bit (profile 2) | 48 | **BYTE-IDENTICAL** |
| vp9-1080p-60-10bit (profile 2) | 120 | **BYTE-IDENTICAL** |
| vp9-1080p-23.976-8bit | 48 | BYTE-IDENTICAL |
| vp9-1080p-60-8bit | 120 | BYTE-IDENTICAL |
| vp9-2160p-23.976-8bit | — | BYTE-IDENTICAL |
| vp9-superframe-altref | — | BYTE-IDENTICAL |
| vp9-tiles-1280 | — | BYTE-IDENTICAL |

Inter/odd fixtures are covered by the media-gated `inter_pixels_exact.rs`
and `odd_dimensions_exact.rs` (green). 8-bit corpus re-swept on this build;
all identical.

Repro:
```
F=fixtures/bitstreams/vp9-1080p-23.976-10bit.ivf
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9hbd INTER_IVF=$F \
  EC_VP9_PIXDUMP=$HOME/.cache/vp9hbd/ours10.pix \
  cargo test -q -p ec-vp9 --test scratch_pixdump10 -- --nocapture
PIXDUMP=$HOME/.cache/vp9hbd/or10.pix $HOME/.cache/vp9hbd/drv_pix10 $F
cmp $HOME/.cache/vp9hbd/ours10.pix $HOME/.cache/vp9hbd/or10.pix   # identical
```

## Gates

- `cargo test -p ec-vp9` green (all suites; `hbd_exact` 2/2).
- Warning parity: exactly the 5 pre-existing (TM_PRED, read_partition,
  partition_props, x_mis, y_mis).
- Scoped rustfmt on touched files; main checkout untouched (only the
  pre-existing untracked `EDITH_FINDINGS.md`).

## Debug technique that found the CAT6 bug (reusable)

A 10-bit stream desynced in the tile bool decoder at 8-bit-correct syntax.
The depth-dependent element is invisible to code reading. Method: built an
HBD oracle, patched `vpx_read` to print `BR c=<count> p=<prob> b=<bit>`,
matched our `EC_VP9_TRACE` `B <pos> <bc> <prob> <bit>` lines, found the
first differing prob (ours 195 = un-updated default, oracle 85), and read
libvpx's `decode_coefs` CAT6 branch — 16 bits/`vp9_cat6_prob_high12 + 2` at
bd 10. `OMODE` (patched `read_intra_frame_mode_info`) proved the divergence
was AFTER mode info, not in it.

## Deferred / out of scope (unchanged)

- 12-bit decode (profile 2 12-bit): refused by name. No corpus stream.
- Profiles 1/3, non-4:2:0 subsampling: refused by name.
- Sibling lane owns intra-only + reference scaling; size-change refusal
  (`decode.rs` reference-size check) untouched.

## Instruments left in tree

- `decode.rs`: the tile desync error now names the tile and overread count
  (was a bare `ensure`); behaviour otherwise unchanged.
