# lane-troykf r1 — Troy key frames: two chroma-only reconstruction defects

Base: `lane-sb128b` e8e64a6 (= main 18bf7dc + the 128 NONE block fix).

## Premise re-measured (not taken from the charter)
census4's `troy4.tsv` was recorded on main 18bf7dc, where every Troy segment
REFUSED. On this base all ten non-`sb128c` key frames decode with **no refusal**
and **luma bit-exact**, chroma wrong — which is what made the entropy stream an
exonerated suspect from the first measurement.

## Root causes (both chroma, both cited to libaom)

1. **A skipped intra block still predicts, and CfL is prediction.**
   libaom `predict_and_reconstruct_intra_block` (decodeframe.c) calls
   `av1_predict_intra_block_facade` unconditionally and guards only the
   residual on `mbmi->skip_txfm`. Three of our skip arms dropped the CfL term
   and predicted plain DC:
   - `crates/ec-av1/src/decode.rs:7719` — the 16x16-level 1:4 chroma pair
     (`decode_rect4_16`).
   - `crates/ec-av1/src/decode.rs:10528`, `:10891` — the two sub-8x8 4x4-group
     chroma arms.
   Troy ss=2700: chroma-only bounded box at chroma (48..79, 464..492), the
   `VERT_4` pair at mi(24,234)/(24,235) — aomdec `EC_TRACE_MODE` prints
   `uv_mode=13 skip=1` there. 499 wrong samples -> **0**.

2. **The 1:4 pair's chroma intra-edge FILTER TYPE is read at the chroma
   reference, not at the closing strip.**
   libaom `get_intra_edge_filter_type` (reconintra.c) reads
   `chroma_above_mbmi`/`chroma_left_mbmi`, i.e. the neighbours of the
   chroma-reference mi. We passed the closing strip's own mi, whose above cell
   is the pair's FIRST strip: the mi-exact UV map missed, the coarse 16px
   fallback answered, and the filter type flipped
   `av1_use_intra_edge_upsample` (`blk_wh = 8 + 4 = 12 <= 16` upsamples for
   type 0, never for type 1). `crates/ec-av1/src/decode.rs:7714`.
   Troy ss=5400, the `HORZ_4` pair at mi(82,83)/76: aomdec
   `EC_PRED ... mode=5 p_angle=107 ft=0`, `EC_PREDOUT row0=415,431,453,471,...`
   vs our `OUR_PRED ... row0=[415,431,452,470,...]`. 46 wrong samples -> **0**.

## Files changed
- `crates/ec-av1/src/decode.rs` — the three skip arms now pass the CfL term;
  the 1:4 pair's `smooth_uv_neighbour` is read at `pair_mi`; two new
  counters + `pub fn troy_chroma_counters()`.
- `crates/ec-av1/src/stream.rs` — new gate
  `a_real_aomenc_sb128_stream_whose_skipped_cfl_and_1to4_chroma_pairs_decode_pixel_exact`.
- `crates/ec-av1/examples/decode_probe.rs` — prints `troy_chroma:` counters.

## Sweep for the same shape
`grep -n "u\.reconstruct\|v\.reconstruct\|buf\.reconstruct"` over decode.rs, then
every `if skip {` arm (21 sites) inspected: the square path (`:9416`), the rect
strip paths (`:6490`, `:6930`, `:7248`, `:8034`) and the inter paths already
computed `ac` inside their skip arm. The three fixed above were the only
chroma-CfL-dropping arms. All 11 `smooth_uv_neighbour` call sites checked: the
other ten pass a block/group ORIGIN mi (`leaf_mi`, `r*(SUB/MI)`), which already
is the chroma reference; only `decode_rect4_16` passed a non-origin mi.

## Gate
`cargo test -p ec-av1 --lib a_real_aomenc_sb128_stream_whose_skipped_cfl -- --nocapture`
(real aomenc `--sb-size=128 --cpu-used=0 --enable-1to4-partitions=1
--enable-cfl-intra=1 --min-partition-size=4 --max-partition-size=32
--cq-level=52`, 256x256 `mandelbrot+noise(all_seed=7)`, 8- and 10-bit arms,
aomenc output hashed twice for reproducibility, decode error tolerated only if
the message contains "unsupported", `compared > 0` asserted).

EVIDENCE: $HOME/.cache/troykf-suite-r1.log (gate stdout quoted below) | cargo test ... --nocapture | pixel-exact at 8-bit and 10-bit, directional_1to4_chroma_pairs=11 per arm, skipped_cfl=0

## Troy key-frame table (1 frame per seek point, `EC_PROBE_OUT16` vs ffmpeg yuv420p10le)

| ss | before (this base) | after |
|----|--------------------|-------|
| 0    | 0 | 0 |
| 900  | 4173 (U 3107 / V 1066, max 7) | **0** |
| 1800 | 14402 (max 10) | **0** |
| 2700 | 499 (max 3) | **0** |
| 3600 | 119652 (max 37) | **0** |
| 4500 | 0 | 0 |
| 5400 | 46 (max 4) | **0** |
| 6300 | 33742 (max 7) | **0** |
| 7200 | 52891 (max 11) | **0** |
| 9900 | 4131 (max 7) | **0** |
| 8100 | frame 0 refuses (128-root non-SPLIT) — lane-sb128c | unchanged |
| 9000 | frame 0 refuses (128-root non-SPLIT) — lane-sb128c | unchanged |

Luma was bit-exact in every row, before and after.

EVIDENCE: $HOME/.cache/troykf-work/xo_*.raw vs xr_*.raw | ffmpeg -ss <t> -t 2 -c:v copy -f obu, trunc.py to 1 frame, decode_probe EC_PROBE_OUT16, ffmpeg -pix_fmt yuv420p10le | samples_diff 0/2280960 on all ten decodable key frames

## Residue
- deferred(a lavfi/aomenc recipe or a committed stream fixture that fires a
  SKIPPED 1:4 or sub-8x8 CfL block) — defect 1 has no synthetic gate. 20 aomenc
  recipes were swept (cq 40/44/46/50/52/55/58/62/63 x mandelbrot, blurred
  mandelbrot, noised mandelbrot, testsrc2, gradients, smptebars, cellauto, life,
  grey+noise; min-partition 4, max-partition 16/32/64, sb-size 128) and
  `skip_cfl` stayed 0 in all of them. Its evidence is Troy ss=2700 (499 -> 0)
  and the counter is printed by the gate and by `decode_probe`, so the first
  recipe that reaches it is visible.
- accepted — ss=8100 / ss=9000 still refuse inside frame 0 on the 128-root
  non-SPLIT partition; that refusal belongs to lane-sb128c.
- No refusal was lifted this round, so `refusal_inventory.rs` /
  `gate_coverage.rs` are unchanged.
