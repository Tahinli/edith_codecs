# Lane aacpstone: PS pure-tone mismatch

## Goal
Find and fix the cause of the HE-AACv2 Parametric Stereo mismatch against FFmpeg on the
pure-tone fixtures (`ps_44100_sine.aac`, `ps_48000_sine.aac`), which sat at corr ~0.70 at zero
lag and were listed in `KNOWN_INEXACT`. Acceptance: all four PS fixtures reach corr >= 0.999 per
channel, `KNOWN_INEXACT` is empty, the existing ec-aac tests still pass.

Base sha: `82dd83f6` (main). Branch: `lane/aacpstone`.

## Cause
The PS Huffman codewords were assigned wrongly. `ps_data.rs::build_codes` assigned canonical
codes (sorted by length, then table index). FFmpeg's `ff_vlc_init_from_lengths` (libavcodec/vlc.c)
does not sort: each table entry takes the next free slice of the code space in table order, so the
codes depend on the table order, not only on the lengths.

The canonical assignment decodes the fine-IID (iid_mode 3..5) and time-differential IID/ICC
frames into out-of-range values (for example ICC = -2). `read_data` then drops PS for that frame
and, because `start` is only set again by a header frame, the following frames fall back to mono
(L = R = SBR mono). On the sine fixtures this was 91 of ~190 frames, which produced the ~0.70
correlation with matching RMS: the sine is a 440 Hz tone panned left and a 660 Hz tone panned
right, and the mono fallback gives both channels both tones at 1/sqrt(2) gain.

Checking the flags first (as the brief suggested) showed the sine fixtures are the only ones that
use `iid_quant` (iid_mode 4, fine IID, 20 bands) with two envelopes; IPD/OPD and mixing type B were
not used by any fixture. The decode path is the one that failed.

## Fix
`crates/ec-aac/src/ps_data.rs`: `build_codes` now assigns codes the FFmpeg way (cumulative, in
table order, in units of 2^-32). Added unit test `codes_follow_table_order_as_ffmpeg_assigns_them`
that pins the first codewords of `IID_DF1`.

`crates/ec-aac/tests/ps_oracle.rs`: `KNOWN_INEXACT` is now empty. The mechanism (per-fixture
floor, list, and doc) is kept.

## Stages checked against FFmpeg (all matched, no change needed)
- ps_data: header/iid_mode/icc_mode tables, border_position, fake-envelope fix-up, `is34bands`,
  IPD/OPD parse and mask, extension handling, error path. Huffman table data (all 10 tables)
  compared programmatically against FFmpeg's `aacpsdata.c`: identical.
- Stereo processing: `H11/H12/H21/H22`, `H_LUT` (HA for icc_mode < 3, HB otherwise), IPD/OPD
  smoothing (`pd_re/pd_im_smooth`), envelope interpolation (`MSUB31_V3`, `h += step` before use),
  the k=9..13 / k<=1 sign flip, `k_to_i` for 20 and 34 bands, remap20/remap34 and the 10/34
  mapping functions.
- Decorrelation: `phi_fract`, `Q_fract_allpass`, the three all-pass links, delay offsets (2, 14, 1),
  `DECAY_CUTOFF`/`NR_ALLPASS_BANDS`/`SHORT_DELAY_BAND`, transient constants (0.765928..., 1.5, 0.25).
- Hybrid analysis and synthesis: `ps_hybrid_analysis`, `hybrid2_re`, `hybrid6_cx`, the 34-band
  path, and the synthesis folds.
- Filter tables `G0_Q8/G0_Q12/G1_Q8/G1_Q2/G2_Q4`, `IID_DEQUANT` (46 entries), `ICC_INVQ`.

Diagnostic that isolated it: a per-block correlation was constant (~0.70) over the whole file,
and the decoded spectra showed FFmpeg L = pure 440 Hz, R = pure 660 Hz, while ours had both tones
in both channels at 1/sqrt(2). That is the mono fallback, not a DSP phase error.

## Correlation (zero lag, full overlap, vs FFmpeg, `-ac 2` f32)
Frame counts identical on both sides in every row.

| fixture | ch | corr@0 before | corr@0 after |
|---|---|---|---|
| ps_44100_sine | 0 | 0.708783 | 1.000000 |
| ps_44100_sine | 1 | 0.703701 | 1.000000 |
| ps_44100_music | 0 | 0.999922 | 0.999922 |
| ps_44100_music | 1 | 0.999993 | 0.999993 |
| ps_48000_sine | 0 | 0.699835 | 1.000000 |
| ps_48000_sine | 1 | 0.716376 | 1.000000 |
| ps_48000_music | 0 | 0.999702 | 0.999937 |
| ps_48000_music | 1 | 0.999928 | 0.999994 |

## Verify (run through `scripts/remote-run.sh aacpstone -- ...`, host a)
- `cargo clippy -p ec-aac --all-targets -- -D warnings`: clean, 0 warnings.
- `cargo test -p ec-aac --release --no-fail-fast`: all binaries pass. 48 + 3 + 1 + 2 + 1 + 13 + 1 + 16
  passed; 1 ignored (pre-existing, in the 13-test binary). `ps_fixtures_decode_to_stereo_matching_ffmpeg`
  passes with an empty `KNOWN_INEXACT`.

## Left
- Nothing known for the four PS fixtures. Music 48 kHz ch0 is at 0.999937, above the bar.
- The PS parse-error path (`read_data`) still sets `start = false` until the next header frame.
  That is FFmpeg's behavior and is kept.
- No PS-specific fuzz target yet (unchanged from `lanes/aacps.report.md`).
