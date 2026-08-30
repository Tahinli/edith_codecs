# lane-rectwire r1 report

VERDICT: NO CODE LANDED — design fully worked out and verified against real
libaom source, but not implemented, because the remaining turn budget did not
leave room to implement AND test-iterate a coefficient-decode surface where
"a desync is not acceptable" is the hard rule. Landing an unverified multi-site
edit at the end of a budget is exactly how a desync gets shipped; refusing by
inaction here is the charter's own method applied to the meta-level. Tree is
untouched (`git status --short` empty on HEAD), lib suite is unchanged at
226 passed / 0 failed, refusal counts are unchanged from before this round.

## What actually narrowed: the scope, not the code

`decode_block_rect` (`decode.rs:1713`) is called from exactly **four** call
sites, all at exactly **two** fixed size pairs: luma 32x16/16x32
(`decode.rs:4341-4396`, only from a 32x32 quadrant's `PARTITION_HORZ`/`VERT`)
and its chroma half 16x8/8x16. This is much narrower than the charter's "every
INTER partition below 16x16" framing suggested for the intra strip -- the
non-skip refusal at `decode.rs:1772-1776` only ever needs to cover these two
size pairs, not a general rect-tx-size machine. The INTER-partition-below-16x16
refusal (`decode.rs:8697`) was not investigated this round (charter: land one
cleanly, not both).

## The three "missing surfaces" the charter named, resolved

1. **Rectangular scan order.** libaom's `default_scan_WxH` tables are
   precomputed data, not a general algorithm in source (no generator shipped
   in `~/.cache/aom-oracle/src`). Extracted+transposed via a one-off script
   (libaom's own buffer is column-major, `col = pos >> bhl; row = pos -
   (col << bhl)` where `bhl = log2(height)`; our grid is row-major, so
   `our_pos = row * w + col`). Verified as a genuine bijection of `0..w*h` for
   all four needed tables (32x16, 16x32, 16x8, 8x16). Script + generated
   consts are in `/tmp/claude-1000/gen_scan.py` and
   `/tmp/claude-1000/scan_tables.rs` in this worktree's scratchpad (not
   copied into the repo since nothing consumes them yet).

2. **Rectangular eob context.** `eob_coeff_ctx(scan_idx, area)` already takes
   `area`, not `side`, and libaom's own `get_lower_levels_ctx_eob` formula
   (`(width << bhl)`, i.e. `width * height`) reduces to the exact same `area`
   — **no change needed here**, only a caller passing `w * h` instead of
   `side * side`. The genuine gap is `base_ctx`'s 2D-class position-offset
   table (`NZ_MAP_CTX_OFFSET_32`, `cdf.rs:569`), which is square-only.
   libaom's own comment in `txb_common.h:189-224`
   (`get_nz_map_ctx_from_stats`) gives the exact rect generalization
   verbatim: `if width < height && row < 2: offset 11; else if width > height
   && col < 2: offset 16; else` fall through to the existing `row+col<2→1,
   <4→6, else 21` table square case already implements. Confirmed this
   reduces to the identical square table when `width == height` (the two new
   branches never fire). `br_ctx`'s neighbour-offset math is shape-independent
   for `TxClass::TwoD` (only the boundary clamp needs real `w`/`h` instead of
   one `side`).

3. **`max_txsize_rect_lookup` threading.** Turned out to be unnecessary for
   this specific lift: `decode_block_rect` already resolves `tx_w = bw, tx_h
   = bh` directly (no split, since `depth != 0` is refused above the skip
   check) — the real tx size *is* the block size here, spec's rect-lookup
   table is a non-issue at this call site.

## The one genuinely new piece: two CDF table sets

Coefficient CDFs are selected by `get_txsize_entropy_ctx(tx_size) =
txsize_sqr_up_map[tx_size]` (`entropy.h:172`, confirmed algebraically: for any
2:1 rect size this reduces to exactly `sqr_up`, never the average). For our
two size pairs that means:
- 32x16/16x32 luma → same square-equivalent (`TX_32X32`) as the already-wired
  `TxbSet::Luma32` for **every** field except `eob_pt` — `txb_skip`, `base`,
  `base_eob`, `br`, `dc_sign` are all directly reusable as-is (confirmed field
  presence: `txb_skip_luma_32`, `base_luma_32`, `base_eob_luma_32`,
  `br_luma_32`, `dc_sign_luma` all already exist in `cdf_state.rs`).
- 16x8/8x16 chroma → same square-equivalent (`TX_16X16`) as `TxbSet::Chroma16`,
  same story (`txb_skip_chroma_16`, `base_chroma_16`, `base_eob_chroma_16`,
  `br_chroma_16`, `dc_sign_chroma` all exist).

Only `eob_pt` differs: the real-size eob group count is `log2(w*h) - 4`
(`txsize_log2_minus4`, confirmed exact match against the table in
`common_data.h:346`), which for 32x16/16x32 is 5 (the **512**-group, 10
symbols) — not 6 (**1024**-group, `eob_pt_1024_luma`, what `Luma32` reads for
a true 32x32) — and for 16x8/8x16 is 3 (the **128**-group, 8 symbols) — not 4
(**256**-group, `eob_pt_256_chroma`, what `Chroma16` reads for a true 16x16).
Neither `eob_pt_512_luma` nor `eob_pt_128_chroma` exist in `cdf_state.rs`
today. Their default values (4 q-context variants each, luma plane_type 0
class-2D and chroma plane_type 1 class-2D) were extracted and transcribed
from `~/.cache/aom-oracle/src/av1/common/token_cdfs.h:829-906`
(`av1_default_eob_multi128_cdfs`, `av1_default_eob_multi512_cdfs`), format
confirmed against this codebase's own conversion convention (`AOM_CDFn`'s raw
arguments direct, no `AOM_ICDF` inversion — cross-checked `EOB_PT_1024_LUMA`
byte-for-byte against `av1_default_eob_multi1024_cdfs[2][0][0]`, an exact
match, q-index 2 = the unsuffixed default, 0/1/3 = `_Q0`/`_Q1`/`_Q3`):

```
EOB_PT_512_LUMA (q2, plane0, class2D):   2624,3936,6480,9686,13979,17726,23267,28410,31078,32768,0
EOB_PT_512_LUMA_Q0:                       641,983,3707,5430,10234,14958,18788,23412,26061,32768,0
EOB_PT_512_LUMA_Q1:                      1230,2278,5035,7776,11871,15346,19590,24584,28749,32768,0
EOB_PT_512_LUMA_Q3:                      5927,7809,10923,14597,19439,24135,28456,31142,32060,32768,0

EOB_PT_128_CHROMA (q2, plane1, class2D): 13627,16246,20173,24429,27948,30415,31863,32768,0
EOB_PT_128_CHROMA_Q0:                     5245,7456,12880,15852,20033,23932,27608,32768,0
EOB_PT_128_CHROMA_Q1:                     8045,11200,15497,19595,23948,27408,30938,32768,0
EOB_PT_128_CHROMA_Q3:                    24313,26062,28385,30107,31217,31898,32345,32768,0
```

## The wiring plan (unexecuted, for the next round)

1. `cdf.rs`: add the 8 arrays above.
2. `cdf_state.rs`: add `eob_pt_512_luma: [u16; 11]` / `eob_pt_128_chroma:
   [u16; 9]` fields, `pick()`-initialize in `Cdfs::new`, add two `TxbSet`
   variants (`LumaRect32x16`, `ChromaRect16x8`) whose `txb()` match arms are
   byte-identical to `Luma32`/`Chroma16` except `eob_pt` points at the new
   fields. One variant each covers both orientations (32x16 and 16x32 share
   one square-equivalent context; likewise 16x8/8x16) — no per-orientation
   duplication needed.
3. `decode.rs`: four new small functions mirroring the existing square ones'
   `TxClass::TwoD` branch only (refuse-by-name if a symbol somehow produces a
   different class — should be geometrically impossible here since neither
   size pair ever reads a `tx_type` symbol: luma's is forced `DctDct` by
   `EXT_TX_SET_DCTONLY` at `tx_size_sqr_up == TX_32X32`, chroma is always
   mode-derived via the existing `default_intra_tx_type`, spec
   `av1_get_ext_tx_set_type`, `blockd.h:1097`):
   - `nz_map_offset_2d(row, col, w, h)`, `base_ctx_rect`, `br_ctx_rect`,
     `neighbour_rect` (the rect generalization above).
   - `read_coeffs_rect(dec, coding, scan, w, h, skip_ctx, sign_ctx,
     default_tx_type)`, a `(w, h)`-aware copy of `read_coeffs` restricted to
     `TxClass::TwoD` (refuses by name otherwise).
   - A rect `Neighbours::around_rect` (independent `w`/`h` extents, mirroring
     the already-landed `record_rect`/`record_mi_rect` asymmetric-extent
     pattern at `decode.rs:1378-1457` — `record_mi_rect` already handles real
     coefficient grids correctly with no change needed, since a lone
     non-split TU broadcasts one state uniformly across its own mi span
     either way).
   - `decode_block_rect` gains a `base_q_idx: u8` parameter (threaded from
     the four already-in-scope call sites at `decode.rs:4341-4396`, where
     `base_q_idx` is already a local), replaces the
     `if !skip { return Err(...) }` block with real reads through
     `read_coeffs_rect` + `transform::dequant_and_inverse_typed_wh` (already
     landed by lane-recttx) for luma then chroma, and passes the real
     residual (not `vec![0i32; ...]`) into the existing `reconstruct_rect`
     calls and the real coefficient grid (not the zero placeholder) into
     `record_rect`.
   - A new `RECT_COEFF_HITS` atomic counter incremented once per real
     non-skip decode, hard-asserted `> 0` in the existing free-partition gate
     (`stream.rs:4060-4137`, which already tracks `rect_partition_hits` and
     currently only ever sees the skip case reach decode) — no new gate file
     needed, this lift makes the *existing* gate exercise more of the stream
     it already decodes.

## Why this stopped here instead of landing partial code

Every individual piece above was cross-checked against real libaom source
(not memory, not the spec's prose) with byte-level verification where a
mistake would desync silently: the scan-table transpose was checked as a
bijection, the `EOB_PT_512_LUMA`/`_128_CHROMA` q2 rows follow the exact
`AOM_CDFn`-argument-direct convention this codebase already uses (verified
against `EOB_PT_1024_LUMA`'s known-good transcription), and the
`nz_map_offset_2d` rect branches are libaom's own comment, not a derivation.
But none of it has been compiled or run against a real aomenc stream, and
this decode path has no tolerance for "probably right" — the method this
lane was chartered under is explicit that a desync is never acceptable, only
a clean refusal is. Implementing all of the above plus threading
`base_q_idx` through four call sites plus writing/running the gate assertion
was more edit-and-verify surface than the remaining turn budget could safely
absorb, so nothing was committed rather than risk landing something wrong
this round left no room to catch.

## Before/after refusal counts, firing count

Unchanged this round: both named refusals (`decode.rs:1772-1776` intra
HORZ/VERT non-skip, `decode.rs:8697` inter-below-16x16) still fire exactly as
before. No gate firing count to report — nothing new decodes yet.
