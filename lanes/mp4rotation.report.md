# lane-mp4rotation report — ec-mp4 reads the `tkhd` display matrix (parse + surface)

VERDICT: PASS — the matrix is parsed from both `tkhd` versions and surfaced as a
typed `ec_core::registry::Rotation` on `StreamInfo`; quarter turns are typed,
non-quarter matrices are carried (`Rotation::Other`), every suite green, warning
parity with c102c27a. No pixel rotation (scope: parse + surface); the muxer still
writes a unity matrix (see Residue).

## What landed
- `ec-core` (`registry.rs`, `lib.rs`): new public `Rotation`
  (`None`/`Cw90`/`Cw180`/`Cw270`/`Other { a, b, c, d }`) plus
  `Rotation::from_matrix(a, b, c, d)`, `steps() -> Option<u8>`, `swaps_axes()`,
  `is_none()`, re-exported from `ec_core`. New additive field
  `StreamInfo.rotation: Rotation` (defaults `Rotation::None` in
  `StreamInfo::new`); the only `StreamInfo` struct literal in the workspace
  (`ec-mp4/src/demux.rs`) was migrated — every other construction goes through
  `new()` and is untouched.
- `ec-mp4` (`demux.rs`, `read_trak`): the `tkhd` arm now reads the 36-byte
  display matrix (nine `s32`) that sits immediately before the `u32` width and
  height at the end of the box — correct for both versions, since the far end is
  version-independent (v0 `rest` is 80 B, v1 is 92 B). Its leading 2x2
  (`a b c d`, offsets +0/+4/+12/+16) goes through `Rotation::from_matrix`.
  The reported rotation is not applied to the pixels.
- Fixtures committed under `crates/ec-mp4/tests/data/` with the generator
  (`gen.sh`), byte-for-byte reproducible on this machine:
  `rot0.mp4` (no display side data: the identity matrix, no turn),
  `rot90/180/270.mp4`, `rot45.mp4` (a turn that is not a quarter turn).
  sha256:
  - rot0   `41400f216615a4054f20600fc6c9696cf753b3626db5aad811c9b6f12bd3c894`
  - rot90  `ce5cbe9290bd7c69483ba87f1f9290dc41575d76f6281e580b23d73e61b1c759`
  - rot180 `375d912d891bc8197967b77e6a01d9ef366789fbc8b8cbaa7793e060ac0e1011`
  - rot270 `77c0ecef4592c1c0084a28e1f44829ee32f01ce8abc943298192fd15b4398ce4`
  - rot45  `5fc8df151b85a03040f5f48a1f0b49289153a64f2514f5cf899dd1ae4a6ea715`
  Generator: `ffmpeg -f lavfi -i testsrc2=size=320x240:rate=24:duration=1 -c:v
  libx264 -profile:v baseline -pix_fmt yuv420p` to a scratch source, then
  `ffmpeg -display_rotation <deg> -i src -c copy rot<deg>.mp4` (ffmpeg 8.1.2,
  libx264). `-display_rotation` is an **input** option (before `-i`); the
  `-metadata:s:v rotate=90` form writes no matrix at all.

## Reading convention (matches edith's engine exactly)
Quarter turns **clockwise**; `steps()` = CW count. ffmpeg's counter-clockwise
`-display_rotation 90` writes matrix `a=0,b=-1,c=1,d=0`, which is edith's
`Rotation::Cw270` (edith `crates/engine/src/demux.rs::rotation_of_matrix`,
asserted in its `tests/rotation.rs`). This crate reads the same matrices the same
way, so edith can delete its workaround and consume `StreamInfo.rotation` — the
mapping is the drop-in point.

Classification is exact integer arithmetic: a pure rotation satisfies `a == d`
and `b == -c`; a quarter turn then lands the turned x-axis `(a, b)` on an axis,
so `b == 0` (sign of `a`) or `a == 0` (sign of `b`) decides `None`/`Cw180` vs
`Cw90`/`Cw270`. Anything else — a mirror, a shear, an anisotropic scale, a
45-degree turn, a degenerate all-zero matrix — is `Rotation::Other { a, b, c, d }`
with the raw 16.16 terms, never rounded onto a turn this decoder would apply for
real. A scaled identity (`2*65536, 0, 0, 2*65536`) still reads `None`, matching
edith's scale normalisation.

## Verification
- `cargo test -p ec-core -p ec-mp4` (CARGO_TARGET_DIR private to the lane):
  ec-core 43/0, ec-mp4 lib 18/0, `tests/mp4.rs` 15/0 (1 ignored, the real-library
  gate), `tests/rotation.rs` 3/0.
- New tests: `registry::tests::quarter_turn_matrices_read_as_typed_rotations`,
  `registry::tests::a_matrix_that_is_not_a_quarter_turn_is_kept_not_rounded`
  (ec-core, unit: 90/180/270/identity/scaled-identity + mirror/shear/zero/45),
  and `tests/rotation.rs` (integration, media: rot90→Cw270, rot180→Cw180,
  rot270→Cw90, rot0→None, rot45→Other{46340,-46340,46340,46340}).
- **Fail-pre-fix (non-vacuity)**: neutralising the parse (forcing
  `rotation = Rotation::None`) fails 2 of 3 integration tests
  (`a_tkhd_matrix_surfaces_as_a_typed_quarter_turn`,
  `a_matrix_that_is_not_a_quarter_turn_is_representable`); the no-turn test
  passes either way, as its expectation is `None`. Restored, re-green.
- `cargo check --workspace --all-targets`: clean; warning set is byte-identical
  to c102c27a (ec-vorbis 1, ec-vp9 5, both pre-existing).
- `cargo clippy -p ec-core -p ec-mp4 --all-targets`: no new warnings (one
  pre-existing `needless_borrow` in `tests/mp4.rs:249`, untouched).
- `cargo doc -p ec-core -p ec-mp4 --no-deps`: no rustdoc warnings.
- rustfmt (edition 2024, scoped to the touched leaf files): clean.

## Residue / deferred
- **deferred(follow-up)**: `ec-mp4`'s muxer writes `UNITY_MATRIX` into every
  `tkhd` (`src/mux.rs::write_trak`), so a demux→mux round trip drops a rotation
  carried on `StreamInfo`. Out of this lane's parse+surface scope; the demuxer
  now has the value the muxer would need to write.
- Non-mp4 demuxers (matroska/ogg/riff) construct through `StreamInfo::new` and
  report `Rotation::None`; Matroska's `ProjectionPoseRoll` is not read (out of
  scope).
