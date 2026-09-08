# lane-txrd -- why the unconditional transform-type search collapsed

Branch `lane-txrd` off `01fa1fd5`. Predecessors: `lanes/txset.report.md`
(intra 5-type set), `lanes/txset2.report.md` (inter `DCT_IDTX`), both of
which shipped their search SCREEN-GATED because turning it on for all
content lost ~20 BD points on the two synthetic bars rows.

## The finding

The type search was never mispriced. **An inter block's chroma transform
type is not coded: the decoder derives it from the LUMA type at the block's
chroma-reference position** (spec 5.11.40, libaom `av1_get_tx_type`; our own
reader does it at `decode.rs:18234`, `reduce_inherited_chroma_tx_type`).
The encoder produced its chroma levels under a `DCT_DCT` basis whatever the
luma type search chose, so every non-`DCT_DCT` luma winner had its chroma
levels inverse-transformed on a basis they were never made for.

Per-plane PSNR, bars 1080p, q=60, control vs the unconditional arm
(`EC_AV1_FRAME_PSNR=1`, frames 0..11):

| plane | control frame 1..11 | unconditional arm |
|---|---|---|
| Y | 53.66 53.44 53.30 53.24 ... | **53.70 53.52 53.40 53.32 ...** (BETTER) |
| U | 51.97 51.89 51.78 51.55 ... | 49.15 48.19 47.74 47.57 ... |
| V | 52.52 52.35 52.25 52.14 ... | **45.53 44.19 44.16 42.82 ...** (-7 dB) |

Luma is better on every frame -- the search's own win is real -- and the
all-plane mean the gate ranks on (`psnr_all` concatenates Y, U and V) reads
53.13 -> 50.20. Class `metric blind to a plane`, on top of
`parsed then discarded` / `override slot on one arm`: a decoder-DERIVED
field the encoder never mirrored.

## The three chartered hypotheses, all refuted first

1. **RDOQ's transform-domain distortion / typed gain.** The forward-inverse
   gain was MEASURED per type and size (`alpha = <res, rec> / <res, res>` at
   q_idx 20): 0.977 .. 1.030 over all sixteen types at 4/8/16 and both types
   at 32. No type is mis-scaled, so the `(coeff - level*q)/8` model is as
   valid for `IDTX` and the ADST kernels as for `DCT_DCT`.
2. **The rate term.** `EC_AV1_TXRD_LAMBDA=0` (types compared on
   reconstruction error alone, rate weight zero) reproduces the collapse to
   the digit: bars 1080p +24.2/+4.2 against the arm's +23.1/+3.3 and the
   control's -1.0/-16.8. A rate-term calibration cannot be the cause of a
   loss that survives the rate term being switched off.
3. **The loop filters.** Pre-filter frame luma SSE (`EC_PREFILTER`) is LOWER
   in the arm on every frame at every quantizer (q=60 leaves: 26.02M vs the
   control's 26.21M), and so is the post-filter one (25.47M vs 25.60M). The
   filters were never the loss; the loss was in a plane nobody was reading.

The census also showed the chosen types dominating the `DCT_DCT` trial on
BOTH terms (sse -5.5%, priced bits -19.1%) -- which is what a defect that
lives outside luma looks like from inside the luma search.

## The fix

`encode::recode_inter_chroma`: once `commit_inter_luma` has chosen the
block's luma type, the two chroma planes are re-coded at
`reduce_inherited_chroma_tx_type(luma_type, chroma_w, chroma_h)` -- the
decoder's own rule -- before they are committed. Wired at all three inter
commit sites that can carry a non-`DCT_DCT` luma type (the `code_square_inter`
winner and its intra-vs-inter tail, and `search_inter_block`'s 32x32 path);
a split block hands over `luma_tx_types[0]`, the top-left unit, which is the
position `xd->tx_type_map` is indexed at. An OBMC/warp winner commits the
flat trial with no type search, so its `luma_tx_types` is empty and nothing
is re-coded.

SWEEP (same shape elsewhere): intra chroma derives its type from the UV MODE,
not from luma (`Intra_Mode_To_Tx_Type`), and the encoder already codes it that
way -- so the intra 5-type search of lane-txset is NOT affected. The only
other inheritance is intrabc's (`fctx.intrabc_chroma_tx`), whose luma type is
always `DCT_DCT` in this encoder.

## Gates

See the table appended below; the arms are running at the time of writing.

### The 12-frame native gate (`bd_rate_screen_native`, 12 frames, gop 12)

BD-rate vs libaom `cpu-used 6` / rav1e `speed 6`; lower is better.

| clip | control (pre-lane) | unconditional BEFORE the fix | unconditional AFTER the fix | shipped (screen-gated) AFTER |
|---|---|---|---|---|
| bars 1080p | -1.0% / -16.8% | +23.1% / +3.3% | **-0.6% / -16.2%** | -1.0% / -16.8% |
| bars 2160p | +9.4% / -12.7% | +31.2% / +3.7% | **+10.3% / -11.7%** | +9.4% / -12.7% |
| film A | +21.7% / -4.4% | +21.5% / -4.5% | +21.6% / -4.5% | +21.7% / -4.4% |
| film B | +26.9% / -0.6% | +27.2% / -0.3% | +27.2% / -0.3% | +26.9% / -0.6% |
| screen capture | +20.1% / -30.4% (shipped) | +20.1% / -30.4% | +19.8% / -30.5% | **+19.8% / -30.5%** |

The twenty-point collapse is GONE: the two bars rows come back to within
0.4/0.6 and 0.9/1.0 of the control instead of 24/20 points off it.

DECISION: the unconditional arm still does NOT pass the keep rule (bars
1080p 0.4/0.6 down, bars 2160p 0.9/1.0 down, film B 0.3/0.3 down -- the
charter's bound was 0.3), so the inter search STAYS screen-gated and the
default is unchanged on every non-screen row: the four non-screen rows print
the control's own byte counts to the digit (194497/299067/419042/567789,
205409/321883/435765/564573, 64392/102508/170169/413430,
27442/50374/105485/296813). What the fix does change is the row the search
already ships on: the screen capture goes +20.1/-30.4 -> **+19.8/-30.5**,
i.e. the shipped screen-gated search was carrying this defect and is 0.3
better against libaom with it repaired.

What is left on the bars rows is now the real `local-rd-on-references`
residue, not a correctness bug -- a whole order of magnitude smaller than
what was attributed to the price.

### Invariants

* `cargo test -p ec-av1 --release --lib` (detached): **569 passed, 0 failed,
  46 ignored** (45 + this lane's `txrd_gain_probe` measurement).
* `cargo check --workspace --all-targets -j4`: 0 errors, 0 ec-av1 warnings.
* The BD gate itself asserts, at all four quantizers of all five rows, that
  ffmpeg's decode AND our decoder's both equal the encoder's reconstruction
  sample for sample -- three-way exact under the fix, in both arms.

### Instruments this lane leaves

* `EC_AV1_TXRD_LAMBDA=<f>` -- rate weight of the transform-TYPE decision
  only (`0` = compare types on reconstruction error alone).
* `EC_AV1_TXRD_CENSUS=1` -- chosen vs `DCT_DCT` summed sse/bits over the flat
  inter luma decisions, printed by the gate.
* `EC_AV1_FRAME_PSNR=1` -- per-frame all-plane AND per-plane (Y/U/V) PSNR per
  ladder point, plus `EC_PREFILTER`/`EC_POSTFILTER` frame luma SSE around
  `pick_and_apply_filters`, which is what separates a bad trial from a bad
  filter choice.
* `transform::tests::txrd_gain_probe` (`--ignored`) -- the measured
  forward-inverse gain per type and size.
* `every_speed_preset_decodes_sample_exact_through_both_decoders --ignored`:
  PASS (presets 0..6, 27.7s).
* `an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly`
  under `EC_COMP_MISMATCH=1`: PASS, no mismatch line.

### Deferred

* `deferred: the unconditional inter type search -- still 0.4-1.0 BD points
  short of the control on the two synthetic bars rows once the chroma defect
  is out -- what unblocks it: the residual is `local-rd-on-references` (a
  block whose type wins locally is a worse reference), so the lever is a
  propagation-aware type cost (tpl weight per block), not another pricing fix.`
* `deferred: the intra 5-type search's own unconditional arm was not re-run
  under this fix -- intra chroma does not inherit from luma, so nothing this
  lane found applies to it; its screen gate stands on lane-txset's own table.`
