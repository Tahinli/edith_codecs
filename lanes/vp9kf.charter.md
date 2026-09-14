# lane-vp9-kf — first VP9 software-decode lane: crate `ec-vp9`

## Charter (keep-rule)

A shown profile-0 8-bit 4:2:0 **KEYFRAME**'s Y/U/V planes are **byte-exact** against
`ffmpeg -v error -i <ivf> -frames:v 1 -f rawvideo -pix_fmt yuv420p -`.
Close PSNR is a fail. libvpx quirks ported verbatim, including dead guards.

## Scope

- New crate `crates/ec-vp9` in worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-vp9`,
  branch `lane-vp9-kf`, base `29f35b77`. Workspace `Cargo.toml` lists crates via the
  `members = ["crates/*"]` glob exactly like `ec-vp8` — nothing to add there.
- Deps: `ec-core` + `ec-vp9-syntax` only (uncompressed header, superframe split, dc_q/ac_q).
  libvpx/ffmpeg are test oracles, never runtime code. `deny(unsafe_code)`.

## Modules

`bool` (range coder, spec 8.3), `header` (thin wrap of `Vp9Parser` + compressed header),
`decode` (Decoder::new / decode -> Option<Picture>, superframe split first), `stream` (IVF
DKIF/VP90), `modes` (partition + intra mode info), `intra` (predictors), `transform`
(DCT/ADST/WHT), `tokens` (coefficient decoding), `loopfilter`, `tables`. **No `mc.rs`
this lane.**

## Refusals (named, via `Error::unsupported`)

- VP9 inter frames — `vp9 inter`
- profile != 0 — `vp9 profile N` (pinned: `fixtures/bitstreams/vp9-profile1-444.ivf`)
- subsampling != 4:2:0 — `vp9 4:2:2` / `vp9 4:4:4`
- `show_existing_frame`: return the buffered reference plane, never pretend to decode.

## Out of scope

inter/MC, profile 1/3 pixels, 10-bit, edith seat, HEVC, any edit to `ec-vp8`, `ec-hw`,
`edith`, or `ec-vp9-syntax`.

## Verify (scoped only)

```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9 cargo check -p ec-vp9
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9 cargo test -p ec-vp9 --lib
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9 cargo test -p ec-vp9 --test keyframe_exact -- --nocapture
```
