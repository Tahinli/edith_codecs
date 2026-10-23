# Lane ac3dep: E-AC-3 dependent substreams

Branch `lane/ac3dep`, base `a19477ea` (main, "lanes: edith swapflacenc report").
Decision: DONE.md D5. Revision 2, after review of `00fbc62c` (Table E2.5 and placement were wrong).

## Goal

A 7.1 E-AC-3 stream decodes as 8 channels instead of folding to 5.1. Independent
substream 0 is grouped with the dependent substreams that follow it in the same
packet. Each dependent substream is decoded by the same pipeline, and placed by
its custom channel map the way ffmpeg places it.

## Placement (ffmpeg is the reference)

Source: ffmpeg 8.1 `libavcodec/ac3tab.c` (`ff_eac3_custom_channel_map_locations`)
and `libavcodec/ac3dec.c` (the dependent-substream layout code).

- Table E2.5 as a 16-bit value, bit 15 first: `0x8000` L, `0x4000` C, `0x2000` R,
  `0x1000` Ls, `0x0800` Rs, `0x0400` Lc/Rc pair, `0x0200` Lrs/Rrs pair, `0x0100` Cs,
  `0x0080` Ts, `0x0040` Lsd/Rsd pair, `0x0020` Lw/Rw pair, `0x0010` Vhl/Vhr pair,
  `0x0008` Vhc, `0x0004` Lts/Rts pair, `0x0002` LFE2, `0x0001` LFE.
- Lrs/Rrs map to BackLeft/BackRight (ffmpeg `AV_CH_BACK_LEFT/RIGHT`).
- A dependent position the independent substream already has REPLACES that channel.
  Any other position is ADDED.
- Output channels are in the layout's canonical order (the bit order of the channel
  layout mask): FL FR FC LFE BL BR FLC FRC BC SL SR.
- The independent substream's 3/2 surround pair is SideLeft/SideRight. ffmpeg
  labels AC-3 3/2 as `5.1(side)`; see "Behaviour changes" below.
- Unsupported positions (Ts, Lsd/Rsd, Lw/Rw, Vhl/Vhr, Vhc, Lts/Rts, LFE2) and a
  missing map are refused with `Error::Unsupported`. A map whose channel count
  disagrees with the substream is `Error::Corrupt`.
- Programs 2+ (a second independent substream in a packet) stay refused with
  `Error::Unsupported`. This decision stands.

## Changes

- `crates/ec-ac3/src/dependent.rs`: `positions` (Table E2.5 as above), `merge`
  (replace-or-add, canonical order), `split` and `interleave`, `peek`. Unit tests
  cover the table, refusals, corrupt counts, replacement and ordering.
- `crates/ec-ac3/src/decoder.rs`: the frame is split into channels, each dependent
  is merged in, and the result is interleaved. `core_positions` gives the core's
  positions, with 3/2 at side. `layout_of` names the layouts. `fold` takes the
  positions of the merged frame. The `Extension` struct and the `join` helper are
  gone.
- `crates/ec-ac3/src/lib.rs`: crate doc describes the placement rule.
- `crates/ec-ac3/tests/dependent.rs`: see the test table below.
- `scripts/gen-ac3dep-fixtures.sh`: chanmaps corrected (see fixtures). The 5.1 source
  now uses distinct side sines (1100 and 1500 Hz) so placement errors show in the
  comparison.
- `scripts/splice-eac3-dependent.py`: writes the map bit 15 first. Its logic was
  already right; the table in the review was the problem. Docstring updated.
- `crates/ec-ac3/fuzz/seed.sh`: seeds every `ac3dep-*.eac3` fixture.
- `scripts/ac3dep-src-2.0.convsnr.txt`: NOT deleted (deviation, see below).

## Fixtures

ffmpeg's E-AC-3 encoder cannot encode 7.1, so the fixtures are spliced from ffmpeg
encodes (`scripts/gen-ac3dep-fixtures.sh`). Each packet is an independent 5.1 frame
plus a dependent frame copied from a stereo encode (`strmtyp=1`, map inserted,
`frmsiz` recomputed, independent-only fields removed).

| Fixture | Map | ffprobe `channel_layout` | Result |
| --- | --- | --- | --- |
| `ac3dep-7.1-48000.eac3` | 0x0200 (Lrs/Rrs appended) | `7.1` | 8 channels |
| `ac3dep-7.1wide-48000.eac3` | 0x0400 (Lc/Rc appended) | `7.1(wide-side)` | 8 channels |
| `ac3dep-repl-48000.eac3` | 0x1800 (Ls/Rs replace core SL/SR) | `5.1(side)` | 6 channels |

ffmpeg decodes all three with zero errors.

Why the splice needs `scripts/ac3dep-src-2.0.convsnr.txt`: the stereo source is an
independent frame, so its `convexpstr` (audfrm) and `convsnroffste` (each audblk)
must be removed from the dependent copy. Their bit positions depend on the whole
audblk prefix (spectral extension, coupling, rematrixing, exponent groups), which
only the crate's parser knows. The file is that table, one row per block, recorded
from the parser for this one encode. Deleting it needs a full audblk walk in the
splicer, which I did not write. The file is a splicer input, not a debug leftover.
It must be regenerated if the stereo source changes.

## Behaviour changes

- A core 3/2 surround pair is now SideLeft/SideRight (matches ffmpeg). Any AC-3 or
  E-AC-3 5.1 stream now reports `ChannelLayout::Custom([FL FR FC LFE SL SR])`
  instead of `Surround5_1` (BL/BR). The sample order is unchanged. No existing
  test asserts the 5.1 layout name, but this is visible to callers.
  Options: accept the Custom label, or add a side-surround 5.1 layout to ec-core.
- A mono core with LFE reports `Custom([FC, LFE])`. It used to report `Stereo`,
  which was wrong.

## Commands and results

Run via `scripts/remote-run.sh ac3dep -- ...`, with `EC_REQUIRE_FIXTURES=1` for the tests.

| Command | Result |
| --- | --- |
| `cargo clippy -p ec-ac3 --all-targets -- -D warnings` | clean |
| `cargo test -p ec-ac3 --release` | lib 48, decode_matrix 8, dependent 7, encode_matrix 6 passed; all exit 0 |
| `./seed.sh && cargo +nightly fuzz build` (crates/ec-ac3/fuzz) | built |
| `cargo +nightly fuzz run decode_stream -- -max_total_time=60` | 11,920 runs in 61 s, exit 0 |
| `cargo +nightly fuzz run parse_header -- -max_total_time=60` | 3,836,507 runs in 61 s, exit 0 |

The existing `decode_matrix` and `encode_matrix` tests pass unchanged.

## Tests (`crates/ec-ac3/tests/dependent.rs`)

- `seven_one_matches_ffmpeg`: layout name and channel order checked with ffprobe.
  Channels 0 to 5 are compared with ffmpeg's decode of the spliced file. Channels 6 and 7
  (core Ls and Rs) are compared with ffmpeg's decode of the 5.1 source. See the ffmpeg
  defect below.
- `seven_one_wide_matches_ffmpeg`: same method, `7.1(wide-side)`.
- `side_pair_replacement_matches_ffmpeg`: all 6 channels against ffmpeg's decode of
  the spliced file.
- `side_pair_replacement_carries_the_dependent_signal`: the replaced side pair equals
  ffmpeg's decode of the dependent source.
- `dependent_without_an_independent_is_named_not_decoded`,
  `truncated_dependent_substream_is_an_error_not_a_panic`,
  `a_second_independent_substream_in_the_packet_is_still_refused`.

## Per-channel correlation (EC_REQUIRE_FIXTURES=1, dither off)

Format: correlation / RMS delta / max abs error.

7.1 (`ac3dep-7.1-48000.eac3`, identical numbers for the wide fixture):

| Channel | Position | Correlation | RMS | Max abs | Reference |
| --- | --- | --- | --- | --- | --- |
| ch0 | FL | 1.000000 | 4.546e-6 | 8.695e-5 | ffmpeg 7.1 decode |
| ch1 | FR | 1.000000 | 5.988e-6 | 1.080e-4 | ffmpeg 7.1 decode |
| ch2 | FC | 1.000000 | 5.090e-6 | 1.534e-4 | ffmpeg 7.1 decode |
| ch3 | LFE | 1.000000 | 3.406e-7 | 1.065e-6 | ffmpeg 7.1 decode |
| ch4 | BL | 1.000000 | 6.849e-6 | 2.551e-4 | ffmpeg 7.1 decode |
| ch5 | BR | 1.000000 | 8.141e-6 | 3.882e-4 | ffmpeg 7.1 decode |
| ch6 | SL | 1.000000 | 6.980e-6 | 2.710e-4 | ffmpeg 5.1 decode, Ls |
| ch7 | SR | 1.000000 | 8.069e-6 | 3.085e-4 | ffmpeg 5.1 decode, Rs |

Replacement (`ac3dep-repl-48000.eac3`, `5.1(side)`): ch0 to ch5 are 1.000000 with RMS
at most 8.1e-6 and max abs at most 3.9e-4, all against ffmpeg's decode of the same file.
The replaced SL/SR match the dependent source at 1.000000 (RMS 6.9e-6 and 8.3e-6).

Acceptance: correlation at least 0.999 (met); RMS at most 1e-3 (met). The matrix
tests do not report max abs error, so there is no direct 5.1 comparison.

## ffmpeg defect found (oracle limitation)

ffmpeg 8.1 places an appended dependent position wrongly when the core already has a
side pair. Evidence, from its decode of `ac3dep-7.1-48000.eac3`:

- channel 6 (SL) is silent (RMS 0.0000), though the core's Ls carries a sine;
- channel 7 (SR) correlates 1.000 with the dependent's left channel and 0.000 with
  the core's Rs.

Channels 0 to 5 match ffmpeg. The cause appears to be that ffmpeg's core mapping is
identity by output index, so the core's side pair is not moved to the new positions.
The replacement case has no such problem, and it matches ffmpeg on every channel.
So for SL/SR of the appended 7.1 fixtures the test uses ffmpeg's decode of the 5.1
source, which ffmpeg decodes correctly. This is a deviation from "ffmpeg is the
oracle for everything": ffmpeg's own output for that case is wrong.

## What is left

1. Table E2.5 matches ffmpeg's table. I have not checked it against the spec text,
   which is not available here.
2. A real Dolby 7.1 E-AC-3 stream would be the strongest check. None is available.
3. Decide whether the 5.1 layout should report side surrounds (`Custom`, as now) or
   gain a named layout in ec-core.
4. Multiple dependent substreams in one packet are merged in order. ffmpeg keeps only
   the last map, so behaviour for that case is untested.
5. `ac3dep-src-2.0.convsnr.txt` is a splicer input that must be regenerated whenever
   the stereo source changes. Removing it needs an audblk walk in the splicer.
6. Unsupported positions (Ts, Lsd/Rsd, Lw/Rw, Vhl/Vhr, Vhc, Lts/Rts, LFE2) are
   refused. Adding `ChannelPosition` variants would allow them.
