# Lane truehdatmos: decode the PCM presentations of Atmos TrueHD

Base: `main` at `e6274ff1`. Branch `lane/truehdatmos`.

## Goal

`ec-truehd` refused any TrueHD access unit with a 4th substream (Dolby Atmos
object audio) with `Error::Unsupported`, so most modern Blu-ray TrueHD tracks
produced no audio. An Atmos TrueHD stream is backward compatible: substreams
0..2 are the ordinary 2-ch, 6-ch and 8-ch PCM presentations, and legacy
decoders output the highest PCM presentation and ignore substream 3. ffmpeg's
`mlpdec.c` does the same (`limit to decoding 3 substreams, as the 4th is used
by Dolby Atmos for non-audio data`).

## What changed

- `crates/ec-truehd/src/sync.rs`
  - The directory walk accepts up to 4 entries (`MAX_DIRECTORY_ENTRIES`). A
    5th entry that never terminates is `Error::Corrupt`, not `Unsupported`.
  - New `pub(crate) const PCM_SUBSTREAMS: usize = 3`.
  - `channel_layout()` maps 3 or 4 entries to `Surround7_1`: the 4th entry
    does not change the highest PCM presentation.
  - Tests:
    - `a_fourth_substream_is_named_unsupported` is renamed to
      `a_directory_past_four_entries_is_corrupt` and now asserts `Corrupt`.
    - `a_well_formed_4_entry_directory_is_still_named_unsupported` is renamed to
      `a_well_formed_4_entry_directory_parses_with_7_1_on_top`. It asserts a
      4-entry parse, the 7.1 layout, and that substream 3's span is `(14, 20)`,
      the span the decoder skips.
    - Neither test is deleted or ignored.
- `crates/ec-truehd/src/decode.rs`
  - `decoded_substreams(header)` = `min(entries, PCM_SUBSTREAMS)`. `prepare`,
    `decode_substreams` and the `Core` substream vector use it, so substream 3 is
    never decoded. `top` and the restart-header CRC/lossless-check position
    (`is_top`) then refer to substream 2, the 8-ch presentation.
  - The 8-matrix-channel refusal message now says no PCM presentation carries
    more than 8 channels, instead of mentioning the object substream.
- `crates/ec-truehd/src/lib.rs`: the scope doc now says Atmos streams are
  decoded as their PCM presentations, and that the object audio itself is out
  of scope and skipped via its end pointer. Its parity is not checked, since its
  bytes are never read. A directory with more than 4 entries is `Corrupt`.
- `crates/ec-truehd/tests/atmos_synth.rs` (new): a synthetic Atmos stream
  helper and two tests (see below).

Not changed: the parity/checksum of substreams 0..2 (still checked when their
restart headers are present), the encoder, the fuzz targets (their API did
not change).

## Fixtures

- No real Atmos TrueHD stream is available. `real_library.toml` has no TrueHD
  keys, and the only TrueHD stream under `~/Downloads` (scanned with ffprobe)
  is the ReelR 7.1 remux, 3 substreams.
- Synthetic stream built from the remux: `with_object_substream` in
  `tests/atmos_synth.rs` takes the first 1200 TrueHD access units (1 s) and
  rebuilds each one with:
  - a 4th directory entry: flags copy substream 2's non-restart bit (`0x4`),
    end pointer = old last pointer + 4 words;
  - 8 bytes of filler after substream 2's data;
  - major sync `num_substreams` (top nibble of byte 16) set to 4, CRC-16 at
    bytes 26..28 recomputed;
  - header length word and parity nibble recomputed (the fold rule the encoder
    uses; the original AU is asserted to satisfy it first).
- The helper can also write the synthetic stream as raw TrueHD for an ffmpeg
  cross-check, via `EC_ATMOS_SYNTH_OUT=<path>`.

## Verify

Run with `scripts/remote-run.sh truehdatmos -- ...` (host c), unless noted.

| Command | Result |
|---|---|
| `cargo clippy -p ec-truehd --all-targets -- -D warnings` | pass (exit 0) |
| `cargo test -p ec-truehd --release` | pass: lib 17, atmos_synth 2 (skipped on the server, no `~/Downloads`), library 1, oracle 3, doctests 0 |
| `EC_REQUIRE_FIXTURES=1 cargo test -p ec-truehd --release --test atmos_synth` (local, `-j4`, `CARGO_TARGET_DIR=~/.cache/tgt-truehdatmos`, `TMPDIR=~/.cache/tmp-truehdatmos`) | pass: 2/2. 1200 frames, 8-channel, bit-exact vs decoding the original 7.1; restart CRC, parity, lossless check, length mismatches all 0 |
| ffmpeg cross-check (local) | `ffprobe` on the synthetic stream reports 8 channels, 7.1, 48000 Hz. `ffmpeg -f s32le` of the synthetic stream, 1536000 bytes (1 s), is byte-identical to ffmpeg's decode of the original remux's first second |
| `cargo fuzz build` + `cargo fuzz run decode_stream -- -max_total_time=60` (vps c, nightly) | pass: 4647 runs in 61 s, exit 0, no crashes |
| `cargo fuzz run parse_sync -- -max_total_time=60` | pass: 10.9M runs in 61 s, exit 0, no crashes |

The fuzz build predates a cosmetic-only `rustfmt` revert in `decode.rs`. The
final clippy and test runs are on the committed tree.

## What is left

- No real Atmos stream was checked. The synthetic stream follows the directory
  layout ffmpeg accepts (confirmed by its decode), but the object substream's
  real contents are untested. Object audio is not decoded, so this does not
  matter for the PCM output.
- The major sync's `num_substreams` (byte 16, top nibble) is not validated
  against the directory count. This matches the existing behavior for the
  count, which comes from the directory.
- The substream-2 CRC-8 (`br.skip_bits(8)`) is still unvalidated, as before.
