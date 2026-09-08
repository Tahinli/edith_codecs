# lane-refs — the reference set (LAST2) and the frame interpolation filter

Worktree `edith_codecs-refs`, branch `lane-refs` off main 68e96c56. Every gate
arm is the prebuilt release lib-test binary running `bd_rate_screen_native`
(12 pictures, `gop 12`, four quantizers); each row is read off that log's own
header line, one selector env var per arm (the 5-row gate does not fit a 900 s
timeout).

## 0. The controls reproduce, to the digit

| clip | control vs libaom / vs rav1e | charter |
|---|---|---|
| film A, 12 frames | +21.7 / -4.4 | +21.7 / -4.4 |
| film B, 12 frames | +26.9 / -0.6 | +26.9 / -0.6 |
| screen capture, 12 frames | +20.1 / -30.4 | +20.1 / -30.4 |

## 1. LAST2 — the census, taken BEFORE the slot bookkeeping

`encoder.rs`'s `ref_frame_idx` maps LAST2/LAST3 onto LAST's own DPB slot, so
the picture before LAST is not merely unoffered to the search, it is not
retained. Building the real thing is not a small diff (a second retained
slot at every level, `ref_frame_idx`/`refresh_frame_flags`, sign bias, order
hints, `record_mi`, the motion field, the search offering LAST2 through the
per-reference mv stack, and a second MOTION SEARCH per block -- today even
GOLDEN and ALTREF are offered search-FREE, `search_inter_block`'s `extra`
prices only their NEARESTMV/GLOBALMV, precisely because a second search
doubles the stage that already owns most of the encode wall).

So `last2_census` (`encode.rs`, `--ignored`) prices the lever first, on the
gate's OWN window and loader (`probe::gate_crop`, 12 pictures): every 16x16
luma block of every picture gets one full-pel diamond search against the NEAR
reference and the same search against the FAR one, at the two lags the pyramid
offers -- (1, 2) for the leaf chain, (4, 8) for the ARF level (rav1e's own
pair: its ARFs read LAST@-4 + LAST2@-8).

    cargo test -p ec-av1 --release --lib -- --ignored last2_census --nocapture

| clip | level (near/far lag) | blocks | far wins | far wins >10% | SAD near | SAD best-of | prediction energy removed |
|---|---|---|---|---|---|---|---|
| film A | leaf (1/2) | 57600 | 38.7% | 19.0% | 20974762 | 19509622 | **6.99%** |
| film A | ARF (4/8) | 23040 | 37.8% | 21.0% | 12015492 | 10776319 | **10.31%** |
| film B | leaf (1/2) | 76800 | 38.1% | 21.2% | 18946199 | 17572121 | **7.25%** |
| film B | ARF (4/8) | 30720 | 44.3% | 22.5% | 8539904 | 7824079 | **8.38%** |

READ IT HONESTLY: the last column is prediction ENERGY (SAD), not bytes. A
second reference wins about 4 blocks in 10 and wins by more than 10% on 1 in 5,
and best-of-{LAST, LAST2} removes 7-10% of the frame's total absolute
prediction error -- before any of it is paid back in the `single_ref` bits
that name the second reference, the mv bits its own stack costs, and the
second motion search's wall. 7-10% of SAD is the CEILING of the lever, and the
ARF level (10.3% / 8.4%) is where it sits, not the leaf chain.

## 2. The frame interpolation filter

Wired (this lane): `mc::predict` hardwired `InterpFilterKind::Regular`, so the
encoder could not have coded a non-REGULAR frame whatever its header said.
The kernel now lives on `FrameCtx::interp_filter` (copied into every
tile-search worker by `filter_ctx_copy`), `encode_inter_frame` writes the SAME
kernel into the frame header, and the search, the block trials, the compound
predictors and the trial decode all read it. `EC_AV1_INTERP=smooth|sharp` or
the process-global `set_frame_interp` select it; the default stays REGULAR and
byte-identical.

Witness (not ignored):
`each_frame_interpolation_filter_codes_its_own_stream_ffmpeg_decodes_exactly`
-- the three kernels code three DIFFERENT streams (a filter that never reached
the prediction would code REGULAR's bytes, class `symbol consumption gap`) and
ffmpeg reconstructs every frame of each of them exactly.

### SMOOTH on the 12-frame gate

| clip | control | EC_AV1_INTERP=smooth |
|---|---|---|
| film A | +21.7 / -4.4 | **+40.9 / +10.2** |
| film B | +26.9 / -0.6 | **+26.3 / -0.4** |
| bars 1080p | -1.0 / -16.8 | -1.4 / -17.2 |
| bars 2160p | +9.4 / -12.7 | +9.3 / -12.9 |

Film B and both bars rows improve; film A loses NINETEEN BD points (0.2 dB at
every quantizer). The two real films disagree in sign, so SMOOTH is not a
default -- the keep rule ("both film rows improve") is unmet by a wide margin.

(SHARP arms pending.)
