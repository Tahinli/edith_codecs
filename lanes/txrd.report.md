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
