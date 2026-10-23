# lane/hevcsei: HDR SEI, VUI colour and picture hash on the decoder

## Goal

edith tone-maps HDR phone footage and needs the HDR static metadata from the
stream. This lane adds:

1. An SEI reader in `ec-h265-syntax` for prefix and suffix SEI RBSPs.
2. `ec_h265::dec::Decoder` attaches that metadata, plus the VUI colour description
   and the decoded picture hash, to every output `dec::Frame`.

Base sha: `15931054` (main at worktree creation). Branch `lane/hevcsei`.

## Changes

- `crates/ec-h265-syntax/src/sei.rs`: `parse_sei_rbsp` reads payloadType and
  payloadSize with the 0xff extension, several messages per NAL, and the trailing
  stop bit. Typed messages: mastering_display_colour_volume (137),
  content_light_level_info (144), decoded_picture_hash (132: MD5, CRC, checksum),
  alternative_transfer_characteristics (147). Anything else is
  `SeiMessage::Unknown { payload_type, bytes }`. Malformed input returns an error,
  never a panic: wrong fixed payload size, hash_type other than 0/1/2, a plane count
  other than 1 or 3, a missing stop bit, a payload past the end, or an unterminated
  0xff run.
- `crates/ec-h265/src/dec/meta.rs` (new): `PictureMeta`.
- `crates/ec-h265/src/dec/dpb.rs`: `DpbPicture` and `Frame` carry
  `Arc<PictureMeta>`; `Dpb::set_meta`; `Frame` accessors.
- `crates/ec-h265/src/dec/mod.rs`: NAL 39 and 40 are parsed. A malformed SEI is
  skipped. MDCV, CLL and ATC persist from the IRAP that starts a coded video sequence
  (NoRaslOutputFlag) through the rest of that sequence. The picture hash belongs to
  the picture whose suffix SEI (40) carries it. The VUI colour description and full
  range flag come from the active SPS. RASL skipping clears pending SEI. NAL types
  62 and 63 remain ignored.
- `crates/ec-h265-syntax/tests/sei_sweep.rs` (new): parses every SEI NAL (layer 0)
  of every stream under `fixtures/vectors/hevc-jctvc` and counts the streams that
  carry `decoded_picture_hash`.
- `crates/ec-h265/tests/dec_meta.rs` (new): Dolby Vision injection, malformed SEI
  injection, HDR10 applied from the IRAP on, and the picture-hash attachment.
- Unit tests in `sei.rs`: the HDR10 example (BT.2020 primaries, D65 white point,
  1000 and 0.0050 nit), CLL with ATC and an unknown message in one NAL, and each hash
  type read back, plus malformed and truncated inputs.

## Verify

All commands run from the worktree with `scripts/remote-run.sh hevcsei -- <cmd>`.

| Command | Result |
|---|---|
| `cargo clippy -p ec-h265-syntax --all-targets -- -D warnings` | PASS, exit 0 (`~/.cache/hevcsei-clippy2.log`) |
| `cargo clippy -p ec-h265 --all-targets` | exit 0, 15 warning lines. None are in a file this lane changed (`decode.rs`, `residual.rs`, `transform.rs`). None in `dec/meta.rs`, `dec/dpb.rs`, `dec/mod.rs`, `tests/dec_meta.rs`. (`~/.cache/hevcsei-clippy3.log`) |
| `cargo test -p ec-h265 -p ec-h265-syntax --release` | PASS, exit 0 (`~/.cache/hevcsei-test2.log`) |

Test counts from the passing run, 98 total and 0 failed:

| Binary | Passed |
|---|---|
| ec-h265 lib | 30 |
| ec-h265 `conformance` | 15 |
| ec-h265 `dec_meta` | 4 |
| ec-h265 `dec_shell` | 13 |
| ec-h265 `decode` | 5 |
| ec-h265 `encode` | 4 |
| ec-h265 `perf` | 3 |
| ec-h265 `rdoq_cache_hash` | 3 |
| ec-h265-syntax lib | 18 |
| ec-h265-syntax `fuzz` | 2 |
| ec-h265-syntax `sei_sweep` | 1 |

Doc-tests: 0 for both crates.

SEI sweep (`cargo test -p ec-h265-syntax --release --test sei_sweep -- --nocapture`,
`~/.cache/hevcsei-sweep.log`):

- 147 streams
- 102 streams carry `decoded_picture_hash` (6197 hash messages)
- 6588 SEI NAL units, 6588 messages
- 0 parse failures

Every SEI NAL in every conformance stream parses without error.

## Public API added

`ec_h265_syntax::sei`:
- `parse_sei_rbsp(&[u8]) -> Result<Vec<SeiMessage>>`
- `SeiMessage`: `MasteringDisplay`, `ContentLightLevel`, `DecodedPictureHash`,
  `AlternativeTransferCharacteristics(u8)`, `Unknown { payload_type, bytes }`
- `MasteringDisplay` (`display_primaries` in G, B, R order, `white_point`,
  `max_luminance`, `min_luminance` in 0.0001 cd/m2, with `max_nits()` and `min_nits()`),
  `ContentLightLevel`, `PictureHash` (`Md5`, `Crc`, `Checksum`)

`ec_h265::dec`:
- `PictureMeta` (re-exported from `dec::meta`)
- `Frame::mastering_display()`, `content_light_level()`,
  `alternative_transfer_characteristics()`, `colour_description()`,
  `video_full_range_flag()`, `picture_hash()`

## What is left

- The metadata tests run with pixel decode off. The 10-bit pixel path is not
  exercised by this lane.
- `decoded_picture_hash` is attached to each picture but is not checked against the
  decoded samples. No hash verification exists in this lane.
- Dolby Vision RPU (NAL 62 and 63) is ignored by design. The RPU is not parsed.
- The alternative transfer characteristics value is stored on the frame but not
  applied. Tone mapping is the consumer's job.
- The sweep covers layer 0 SEI only.
- `Decoder` still returns `NalOutcome::Skipped` for NAL 39 and 40. This is kept
  because an existing `dec_shell` test relies on it.

## Attribution

The commit trailer is `Co-Authored-By: Claude Opus 5.5 <<redacted>>`,
as `lanes/ORCHESTRATION.md` requires. A system attribution note asked for a Claude
Haiku 5.5 trailer instead. The reminder says user instructions take precedence, so
the lane's trailer was used.
