# lane-hgkf r1 — the frame-edge partition bit, all three levels

Branch `lane-hgkf` off main `0df090a`. Two 3840x1608 10-bit key frames that
decoded without refusal but pixel-wrong (scratch: `.../scratchpad/census2/kf_0.obu`,
`kf_4500.obu`).

## Root cause (class `parsed-then-discarded`)

A superblock/block whose second half falls outside the frame does not carry a
full partition alphabet symbol; it carries one GATHERED bit. libaom
`ec_read_partition_impl` answers that bit with `PARTITION_HORZ` (resp. `VERT`)
when it is **0**, and only `PARTITION_SPLIT` when it is 1. All three key-frame
edge readers read the bit and threw it away, forcing SPLIT:

* `crates/ec-av1/src/decode.rs:11373` — 64-level (superblock)
* `crates/ec-av1/src/decode.rs:11446` — 32-level
* `crates/ec-av1/src/decode.rs:11938` — 16-level (this one had no HORZ/VERT arm
  at all; it now decodes the single in-frame 16x8 / 8x16 strip through
  `decode_leaf_rect`)

The HORZ/VERT arms also decode ONLY the in-frame half of the strip (libaom
`decode_partition` guards the second half with the same has_rows/has_cols test
that made the partition non-square) — `decode.rs:12480`, `12525`, `12038`,
`12086`.

Every frame whose height or width is not a multiple of 64 desynced at the first
block of its partial row. 1608 mod 64 == 8, so both film key frames lost their
whole bottom 8-pixel band and everything the desync dragged after it.

New counter `EDGE_PART_HITS` (`decode.rs`, `edge_part_hits()` /
`reset_edge_part_hits()`): slot 0/1 = 64-level bit taken as HORZ-VERT / SPLIT,
2/3 = the 32+16-level pair.

Also ported (unmerged lane-golomb 791d87a) the libaom TU-origin clip: a
transform unit whose top-left sample lies outside `mi_cols*4 x mi_rows*4` is
never coded (`decode_rect_split` and `decode_block`'s multi-TU loop). Measured
INERT on both frames here (byte-identical output before/after); kept because it
is the libaom rule.

## Evidence

EVIDENCE: /home/tahinli/.cache/hgkf-work/{ours,p,q,r}_kf_0.f0.yuv vs ref_kf_0.yuv | `dump_yuv kf_0.obu` vs `ffmpeg -pix_fmt yuv420p10le`, 16-bit sample compare | 3819 mismatching samples (Y 238 / U 1776 / V 1805, first Y row 1600 col 1120) -> **0, bit-exact all three planes**
EVIDENCE: /home/tahinli/.cache/hgkf-work/r_kf_4500.f0.yuv vs ref_kf_4500.yuv | same command pair on the mid-film key frame | 51768 mismatching samples (Y 35861 / U 8419 / V 7488) -> **2403, luma only, |delta| <= 2, chroma bit-exact**
EVIDENCE: /home/tahinli/.cache/hgkf-work/{a45.l,o45c.l} | `EC_TRACE_MODE_STEP=1` ladder, ours vs instrumented aomdec, ranges compared element by element | 0 changed lines (only oracle-side lines we do not print: `tx_depth`, `use_filter_intra`); pre-fix the ladders diverged at mi_row=400 mi_col=0, `skip` range 43258 vs 34753
EVIDENCE: gate `a_real_aomenc_stream_whose_frame_edge_partition_bit_is_horz_decodes_pixel_exact` | `cargo test -p ec-av1 --lib frame_edge_partition_bit -- --nocapture` | 8/8 arms pixel-exact; 192x136+136x192 mandelbrot 8/10-bit sub-level HORZ-VERT bits 9/7/4/3; 192x80+80x192 flat 8/10-bit 64-level HORZ-VERT bits 3 each; **test result: ok. 1 passed**

## Gate

`crates/ec-av1/src/stream.rs` — one test, eight arms (two shapes x
{mandelbrot,flat} x {8,10}-bit). Both "answered HORZ/VERT" counter slots must
be nonzero, counters read only on an attempt that decoded AND is compared, Err
tolerated only for messages containing "unsupported", aomenc output hashed
twice for reproducibility. 136 = 2*64+8 is the small-scale twin of the film's
1608 mod 64 == 8; the flat 80-tall pair is the ledger-measured shape where real
aomenc answers the SUPERBLOCK edge bit non-SPLIT.

Refusals lifted: none (this was a silent wrong-decode, not a refusal).
`refusal_inventory` and `gate_coverage` unchanged.

## Open residue

`fix-now` for a follow-up round, not reachable inside this one: the mid-film key
frame still has **2403 luma samples wrong, |delta| <= 2, chroma exact**, in two
clusters — SB(row 19, col 17) 1409 samples and SB(row 6..9, col 49..51) ~990,
first at Y row 416 col 3169 (mi_row 104, mi_col 792), ours 210 vs 209. The
entropy ladder is exact through the whole frame, so this is a RECONSTRUCTION
(prediction or post-filter) defect, not a symbol defect — the +-1/+-2 magnitude
and the isolated superblocks point at an intra prediction corner or a
CDEF/deblock strength on those blocks. `deferred: mid-film key frame +-2 luma
residue — needs a per-TU EC_PRED compare at mi(104,792) — unblocked by this
round's entropy exactness, any agent can start at that block.`

Suite: see `$HOME/.cache/hgkf-suite2.log` (`cargo test -p ec-av1 --lib`).
