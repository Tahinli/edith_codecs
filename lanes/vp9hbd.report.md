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
`decode.rs:211`. `hbd_exact.rs` cannot COMPILE on the pre-fix tree
(`Picture.bit_depth` and the u16 plane accessors are new here), so the
refusal was proved with an adapted copy of the test: the new-field reads
stripped, so the body reaches `Decoder::decode` and panics with
`Unsupported("vp9 profile 2")` on the first frame
(`crates/ec-vp9/src/decode.rs` profile gate). The shipped test
`hbd_exact.rs::profile2_10bit_is_byte_exact` then passes on the fixed tree.

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
- `tests/scratch_pixdump10.rs`: u16-LE plane dumper for the 10-bit pixel
  comparator. KEPT as the next lane's instrument (paired with
  `drv_pix10`); it is not a throwaway.

## MERGE-SIDE FOLLOW-UPS (2026-09-19)

- **Review P3 fixed on the lane before the merge.** `lanes/vp9hbd.report.md`
  claimed `hbd_exact.rs` "fails at the first frame on the pre-fix tree"; it
  cannot compile there (`Picture.bit_depth` is new). Reworded to state the
  refusal was proved with an adapted test copy (new-field reads stripped)
  panicking with `Unsupported("vp9 profile 2")` at the profile gate. Lane
  commit `59fdf378`.
- **Merge**: `--ff-only` (base `79984549` == main HEAD), range
  `79984549..59fdf378`, 20 files, +819/−295.
- **Create-list audit**: exactly the expected three creates —
  `crates/ec-vp9/tests/hbd_exact.rs`, `crates/ec-vp9/tests/scratch_pixdump10.rs`
  (kept instrument, above), `lanes/vp9hbd.report.md`. No fixtures, junk or
  target artifacts. Nothing else in the merge.
- **Gates on the MERGED tree** (`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9hbdmerge`):
  `cargo test -p ec-vp9` → `rc=0`, 0 failed across every suite;
  `hbd_exact` **2/2** passed, 0 ignored (non-skipped); `odd_dimensions_exact`
  3/3; `inter_pixels_exact` 3/3; `keyframe_exact` 4/4.
  Warning parity: `cargo check -p ec-vp9 --all-targets` → exactly the 5
  pre-existing (TM_PRED, read_partition, partition_props, x_mis, y_mis).
  No dependent crate in-repo depends on `ec-vp9` (only ec-vp9 → ec-vp9-syntax,
  untouched), so no cross-crate suite to run.
- **Merged-tree corpus re-sweep** (u16 conversion touched every kernel; ours
  regenerated from the merged build via `scratch_pixdump10` / `scratch_pixdump`
  and `cmp`ed against the oracle `drv_pix10` / `drv_pix` dumps), all
  byte-IDENTICAL:
  - `vp9-1080p-23.976-10bit` 48 frames (u16), `vp9-1080p-60-10bit` 120 frames (u16),
  - `vp9-1080p-23.976-8bit` 48, `vp9-1080p-60-8bit` 120,
  - `vp9-superframe-altref` 60, `vp9-tiles-1280` 30,
  - odd witness `testsrc2 321x241` 12 frames (`odd_dimensions_exact`, ffmpeg oracle) 3/3.
- **Push**: `79984549..59fdf378 main -> main`; `git ls-remote origin refs/heads/main`
  == `59fdf378445701defd6b0e3446336ab20b1fa3fb` == local HEAD.
- **Cleanup**: `git worktree remove .../edith_codecs-vp9hbd` (branch
  `lane-vp9-hbd` kept); `rm -rf $HOME/.cache/cargo-target-vp9hbd{,-merge,-verify}`
  (lane + merge + reviewer dirs, ~977 MB). Worktree list went from 8 entries
  to 7 (the lane gone; `vp9ref` sibling lane untouched).
