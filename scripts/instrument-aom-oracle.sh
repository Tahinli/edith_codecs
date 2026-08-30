#!/usr/bin/env bash
# Add the decode-localization instrumentation to the libaom oracle's aomdec.
#
# These patches used to live only in the hand-built /tmp tree; the 2026-08-30
# tmpfs reclaim destroyed them along with the tree (see memory
# oracle-in-reaped-dir). They are re-derived here so `build-aom-oracle.sh` +
# this script reproduce the full ladder from scratch.
#
# Rungs provided (both env-gated, so an uninstrumented run is unchanged):
#   EC_AV1_PREFILT_DUMP=<prefix>  -> <prefix>.f<N> per frame, Y then U then V,
#       crop-sized rows, written after tile decode and BEFORE any loop filter,
#       CDEF or LR. Diff against our own EC_AV1_PREFILT_DUMP to separate a
#       reconstruction mismatch from a filtering one.
#   EC_TRACE=1 -> "EC_PART mi_row=.. mi_col=.. bsize=.. ctx=.. tell=.. rng=.."
#       before every partition symbol, "EC_PART_VAL .. value=.." after. The
#       range ladder: compare rng element-by-element against our own
#       `TRACE part32_pre` (class compare-range-not-tell -- ranges, never tell).
#   EC_TRACE_COEFF=1 -> "EC_COEFF plane=.. row=.. col=.. tx_size=.. rng=.."
#       before every coefficient block, "EC_COEFF_VAL .. rng=.." after.
#       Partition granularity is too coarse to localize a coefficient desync;
#       two lanes stalled on 2026-08-30 because this rung did not exist.
#   EC_TRACE_MODE=1 -> "EC_MODE mi_row=.. mi_col=.. rng=.." before every inter
#       block's mode info, "EC_MODE_VAL .. mode=.. ref0/1=.. mv0=.. rng=.."
#       after -- the mv-stack/DRL/mv reads EC_PART cannot see. The same flag
#       also emits "EC_IMODE .. rng=.." / "EC_IMODE_VAL .. mode=.. uv_mode=..
#       skip=.. tx=.. rng=.." around every INTRA key-frame block: without it
#       the key-frame path has no traced symbol at all between the partition
#       read and the first coefficient block, which is precisely the gap a
#       rect-strip desync hides in.
#   EC_AV1_POSTDEBLOCK_DUMP=<prefix> -> <prefix>.f<N> per frame, Y then U then
#       V, full ALIGNED-buffer rows (y_width/y_height, not y_crop_width --
#       cm->cur_frame->buf at this point is still the pre-superres buffer,
#       but its aligned width already extends past FrameWidth out to the
#       mi-aligned true width, which is exactly the margin content we need).
#       Written right after av1_loop_filter_frame_mt returns and before
#       CDEF/superres run -- ground truth for the post-deblock, pre-superres
#       row content over frame_width..true_width that decode.rs stashes as
#       its superres margin (lane-superres r5: hand-tracing the arithmetic
#       could only prove our own self-consistency, not correctness against
#       libaom).
#   Rung 9 (unconditional, no env gate) -- SGR per-tap ground truth
#       (lane-lr r7): `calculate_intermediate_result` in
#       av1/common/restoration.c is `static`, so a standalone harness cannot
#       call it to get real A[]/B[] intermediate arrays, only the final
#       flt0/flt1 via the public av1_selfguided_restoration_c. This rung
#       drops `static` so an external harness can call it directly with a
#       `dgd32`-style buffer (see scripts/lr-sgr-pin-harness.c) and diff
#       every A[k]/B[k] tap against a from-scratch recompute -- the missing
#       half of r6's 9-tap cross-check. No behaviour change (only linkage).
#
# Idempotent: re-running is a no-op. Rebuild afterwards with
#   ninja -C ~/.cache/aom-oracle/build aomdec
set -euo pipefail
SRC="${AOM_ORACLE_SRC:-$HOME/.cache/aom-oracle/src}"
F="$SRC/av1/decoder/decodeframe.c"
[ -f "$F" ] || { echo "no oracle source at $F -- run scripts/build-aom-oracle.sh first" >&2; exit 1; }

python3 - "$F" <<'PY'
import sys, re
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED" in s:
    print("already instrumented (no-op)")
    sys.exit(0)

# --- rung 2: partition range ladder -------------------------------------
old_sig = """static PARTITION_TYPE read_partition(MACROBLOCKD *xd, int mi_row, int mi_col,
                                     aom_reader *r, int has_rows, int has_cols,
                                     BLOCK_SIZE bsize) {"""
new_sig = """/* EC_INSTRUMENTED */
static PARTITION_TYPE ec_read_partition_impl(MACROBLOCKD *xd, int mi_row,
                                             int mi_col, aom_reader *r,
                                             int has_rows, int has_cols,
                                             BLOCK_SIZE bsize);

static PARTITION_TYPE read_partition(MACROBLOCKD *xd, int mi_row, int mi_col,
                                     aom_reader *r, int has_rows, int has_cols,
                                     BLOCK_SIZE bsize) {
  const int ec_trace = getenv("EC_TRACE") != NULL;
  if (ec_trace) {
    fprintf(stderr,
            "EC_PART mi_row=%d mi_col=%d bsize=%d ctx=%d tell=%d rng=%u\\n",
            mi_row, mi_col, (int)bsize,
            partition_plane_context(xd, mi_row, mi_col, bsize),
            (int)aom_reader_tell(r), (unsigned)r->ec.rng);
  }
  PARTITION_TYPE ec_p =
      ec_read_partition_impl(xd, mi_row, mi_col, r, has_rows, has_cols, bsize);
  if (ec_trace) {
    fprintf(stderr, "EC_PART_VAL mi_row=%d mi_col=%d bsize=%d value=%d\\n",
            mi_row, mi_col, (int)bsize, (int)ec_p);
  }
  return ec_p;
}

static PARTITION_TYPE ec_read_partition_impl(MACROBLOCKD *xd, int mi_row,
                                             int mi_col, aom_reader *r,
                                             int has_rows, int has_cols,
                                             BLOCK_SIZE bsize) {"""
assert old_sig in s, "read_partition signature moved"
s = s.replace(old_sig, new_sig, 1)

# --- rung 1: pre-filter recon dump --------------------------------------
anchor = """  av1_alloc_cdef_buffers(cm, &pbi->cdef_worker, &pbi->cdef_sync,
                         pbi->num_workers, 1);"""
dump = """  {
    const char *ec_dump = getenv("EC_AV1_PREFILT_DUMP");
    if (ec_dump) {
      static int ec_prefilt_idx = 0;
      char ec_path[1024];
      snprintf(ec_path, sizeof(ec_path), "%s.f%d", ec_dump, ec_prefilt_idx++);
      FILE *ec_f = fopen(ec_path, "wb");
      if (ec_f) {
        const YV12_BUFFER_CONFIG *ec_b = &cm->cur_frame->buf;
        for (int ec_r = 0; ec_r < ec_b->y_crop_height; ++ec_r)
          fwrite(ec_b->y_buffer + ec_r * ec_b->y_stride, 1, ec_b->y_crop_width,
                 ec_f);
        if (num_planes > 1) {
          for (int ec_r = 0; ec_r < ec_b->uv_crop_height; ++ec_r)
            fwrite(ec_b->u_buffer + ec_r * ec_b->uv_stride, 1,
                   ec_b->uv_crop_width, ec_f);
          for (int ec_r = 0; ec_r < ec_b->uv_crop_height; ++ec_r)
            fwrite(ec_b->v_buffer + ec_r * ec_b->uv_stride, 1,
                   ec_b->uv_crop_width, ec_f);
        }
        fclose(ec_f);
      }
    }
  }

"""
assert anchor in s, "cdef alloc anchor moved"
s = s.replace(anchor, dump + anchor, 1)
open(path, "w").write(s)
print("instrumented")
PY
echo "now: ninja -C ${AOM_ORACLE_BUILD:-$HOME/.cache/aom-oracle/build} aomdec"

# --- rung 3: coefficient range ladder (decodetxb.c) ---------------------
G="$SRC/av1/decoder/decodetxb.c"
[ -f "$G" ] || { echo "no oracle source at $G" >&2; exit 1; }

python3 - "$G" <<'PYC'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED" in s:
    print("decodetxb already instrumented (no-op)")
    sys.exit(0)

old = """void av1_read_coeffs_txb(const AV1_COMMON *const cm, DecoderCodingBlock *dcb,
                         aom_reader *const r, const int plane, const int row,
                         const int col, const TX_SIZE tx_size) {"""
new = """/* EC_INSTRUMENTED */
static void ec_read_coeffs_txb_impl(const AV1_COMMON *const cm,
                                    DecoderCodingBlock *dcb,
                                    aom_reader *const r, const int plane,
                                    const int row, const int col,
                                    const TX_SIZE tx_size);

void av1_read_coeffs_txb(const AV1_COMMON *const cm, DecoderCodingBlock *dcb,
                         aom_reader *const r, const int plane, const int row,
                         const int col, const TX_SIZE tx_size) {
  const int ec_trace = getenv("EC_TRACE_COEFF") != NULL;
  if (ec_trace) {
    fprintf(stderr, "EC_COEFF plane=%d row=%d col=%d tx_size=%d rng=%u\\n",
            plane, row, col, (int)tx_size, (unsigned)r->ec.rng);
  }
  ec_read_coeffs_txb_impl(cm, dcb, r, plane, row, col, tx_size);
  if (ec_trace) {
    fprintf(stderr, "EC_COEFF_VAL plane=%d row=%d col=%d rng=%u\\n", plane, row,
            col, (unsigned)r->ec.rng);
  }
}

static void ec_read_coeffs_txb_impl(const AV1_COMMON *const cm,
                                    DecoderCodingBlock *dcb,
                                    aom_reader *const r, const int plane,
                                    const int row, const int col,
                                    const TX_SIZE tx_size) {"""
assert old in s, "av1_read_coeffs_txb signature moved"
s = s.replace(old, new, 1)
open(path, "w").write(s)
print("decodetxb instrumented")
PYC

# --- rung 4: mode-info range ladder (decodemv.c) ------------------------
H="$SRC/av1/decoder/decodemv.c"
[ -f "$H" ] || { echo "no oracle source at $H" >&2; exit 1; }

python3 - "$H" <<'PYM'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED" in s:
    print("decodemv already instrumented (no-op)")
    sys.exit(0)

old = """static void read_inter_block_mode_info(AV1Decoder *const pbi,
                                       DecoderCodingBlock *dcb,
                                       MB_MODE_INFO *const mbmi,
                                       aom_reader *r) {"""
new = """/* EC_INSTRUMENTED */
static void ec_read_inter_block_mode_info_impl(AV1Decoder *const pbi,
                                               DecoderCodingBlock *dcb,
                                               MB_MODE_INFO *const mbmi,
                                               aom_reader *r);

static void read_inter_block_mode_info(AV1Decoder *const pbi,
                                       DecoderCodingBlock *dcb,
                                       MB_MODE_INFO *const mbmi,
                                       aom_reader *r) {
  const int ec_trace = getenv("EC_TRACE_MODE") != NULL;
  const MACROBLOCKD *const ec_xd = &dcb->xd;
  if (ec_trace) {
    fprintf(stderr, "EC_MODE mi_row=%d mi_col=%d rng=%u\\n", ec_xd->mi_row,
            ec_xd->mi_col, (unsigned)r->ec.rng);
  }
  ec_read_inter_block_mode_info_impl(pbi, dcb, mbmi, r);
  if (ec_trace) {
    fprintf(stderr,
            "EC_MODE_VAL mi_row=%d mi_col=%d mode=%d ref0=%d ref1=%d "
            "mv0=(%d,%d) rng=%u\\n",
            ec_xd->mi_row, ec_xd->mi_col, (int)mbmi->mode,
            (int)mbmi->ref_frame[0], (int)mbmi->ref_frame[1],
            mbmi->mv[0].as_mv.row, mbmi->mv[0].as_mv.col,
            (unsigned)r->ec.rng);
  }
}

static void ec_read_inter_block_mode_info_impl(AV1Decoder *const pbi,
                                               DecoderCodingBlock *dcb,
                                               MB_MODE_INFO *const mbmi,
                                               aom_reader *r) {"""
assert old in s, "read_inter_block_mode_info signature moved"
s = s.replace(old, new, 1)
open(path, "w").write(s)
print("decodemv instrumented")
PYM

# --- rung 5: intra key-frame mode-info ladder (decodemv.c) --------------
python3 - "$SRC/av1/decoder/decodemv.c" <<'PYI'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_INTRA" in s:
    print("intra mode-info already instrumented (no-op)")
    sys.exit(0)

old = """static void read_intra_frame_mode_info(AV1_COMMON *const cm,
                                       DecoderCodingBlock *dcb, aom_reader *r) {"""
new = """/* EC_INSTRUMENTED_INTRA */
static void ec_read_intra_frame_mode_info_impl(AV1_COMMON *const cm,
                                               DecoderCodingBlock *dcb,
                                               aom_reader *r);

static void read_intra_frame_mode_info(AV1_COMMON *const cm,
                                       DecoderCodingBlock *dcb, aom_reader *r) {
  const int ec_trace = getenv("EC_TRACE_MODE") != NULL;
  const MACROBLOCKD *const ec_xd = &dcb->xd;
  if (ec_trace) {
    fprintf(stderr, "EC_IMODE mi_row=%d mi_col=%d bsize=%d rng=%u\\n",
            ec_xd->mi_row, ec_xd->mi_col, (int)ec_xd->mi[0]->bsize,
            (unsigned)r->ec.rng);
  }
  ec_read_intra_frame_mode_info_impl(cm, dcb, r);
  if (ec_trace) {
    const MB_MODE_INFO *const ec_mbmi = ec_xd->mi[0];
    fprintf(stderr,
            "EC_IMODE_VAL mi_row=%d mi_col=%d mode=%d uv_mode=%d skip=%d "
            "tx=%d rng=%u\\n",
            ec_xd->mi_row, ec_xd->mi_col, (int)ec_mbmi->mode,
            (int)ec_mbmi->uv_mode, (int)ec_mbmi->skip_txfm,
            (int)ec_mbmi->tx_size, (unsigned)r->ec.rng);
  }
}

static void ec_read_intra_frame_mode_info_impl(AV1_COMMON *const cm,
                                               DecoderCodingBlock *dcb,
                                               aom_reader *r) {"""
assert old in s, "read_intra_frame_mode_info signature moved"
s = s.replace(old, new, 1)
open(path, "w").write(s)
print("intra mode-info instrumented")
PYI

# --- rung 6: post-deblock, pre-superres row dump ------------------------
python3 - "$F" <<'PYD'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_POSTDEBLOCK" in s:
    print("postdeblock dump already instrumented (no-op)")
    sys.exit(0)

anchor = """    if (cm->lf.filter_level[0] || cm->lf.filter_level[1]) {
      av1_loop_filter_frame_mt(&cm->cur_frame->buf, cm, &pbi->dcb.xd, 0,
                               num_planes, 0, pbi->tile_workers,
                               pbi->num_workers, &pbi->lf_row_sync, 0);
    }
"""
dump = """
    /* EC_INSTRUMENTED_POSTDEBLOCK */
    {
      const char *ec_dump = getenv("EC_AV1_POSTDEBLOCK_DUMP");
      if (ec_dump) {
        static int ec_postdeblock_idx = 0;
        char ec_path[1024];
        snprintf(ec_path, sizeof(ec_path), "%s.f%d", ec_dump,
                 ec_postdeblock_idx++);
        FILE *ec_f = fopen(ec_path, "wb");
        if (ec_f) {
          const YV12_BUFFER_CONFIG *ec_b = &cm->cur_frame->buf;
          /* Dump the FULL aligned buffer (y_width/y_height), not the
           * crop_width/crop_height (== FrameWidth/FrameHeight): the
           * superres margin (columns [FrameWidth, true mi-aligned width))
           * only exists in the aligned buffer -- lane-superres r5. */
          for (int ec_r = 0; ec_r < ec_b->y_height; ++ec_r)
            fwrite(ec_b->y_buffer + ec_r * ec_b->y_stride, 1, ec_b->y_width,
                   ec_f);
          if (num_planes > 1) {
            for (int ec_r = 0; ec_r < ec_b->uv_height; ++ec_r)
              fwrite(ec_b->u_buffer + ec_r * ec_b->uv_stride, 1,
                     ec_b->uv_width, ec_f);
            for (int ec_r = 0; ec_r < ec_b->uv_height; ++ec_r)
              fwrite(ec_b->v_buffer + ec_r * ec_b->uv_stride, 1,
                     ec_b->uv_width, ec_f);
          }
          fclose(ec_f);
        }
      }
    }
"""
assert anchor in s, "loop filter call anchor moved"
s = s.replace(anchor, anchor + dump, 1)
open(path, "w").write(s)
print("postdeblock dump instrumented")
PYD

# --- rung 7: pre-deblock, FULL aligned-buffer row dump (lane-superres r5) -
# EC_AV1_PREFILT_DUMP (rung 1) is used by other lanes at its existing
# y_crop_width/y_crop_height shape -- left untouched. This adds a second,
# additive env var at the SAME anchor (pre-loop-filter) but dumping the full
# y_width/y_height aligned buffer, so a margin-region reconstruction bug can
# be told apart from a margin-region deblock bug.
python3 - "$F" <<'PYW'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_PREFILT_WIDE" in s:
    print("prefilt-wide dump already instrumented (no-op)")
    sys.exit(0)

anchor = """  av1_alloc_cdef_buffers(cm, &pbi->cdef_worker, &pbi->cdef_sync,
                         pbi->num_workers, 1);"""
dump = """  /* EC_INSTRUMENTED_PREFILT_WIDE */
  {
    const char *ec_dump = getenv("EC_AV1_PREFILT_WIDE_DUMP");
    if (ec_dump) {
      static int ec_prefilt_wide_idx = 0;
      char ec_path[1024];
      snprintf(ec_path, sizeof(ec_path), "%s.f%d", ec_dump,
               ec_prefilt_wide_idx++);
      FILE *ec_f = fopen(ec_path, "wb");
      if (ec_f) {
        const YV12_BUFFER_CONFIG *ec_b = &cm->cur_frame->buf;
        for (int ec_r = 0; ec_r < ec_b->y_height; ++ec_r)
          fwrite(ec_b->y_buffer + ec_r * ec_b->y_stride, 1, ec_b->y_width,
                 ec_f);
        if (num_planes > 1) {
          for (int ec_r = 0; ec_r < ec_b->uv_height; ++ec_r)
            fwrite(ec_b->u_buffer + ec_r * ec_b->uv_stride, 1,
                   ec_b->uv_width, ec_f);
          for (int ec_r = 0; ec_r < ec_b->uv_height; ++ec_r)
            fwrite(ec_b->v_buffer + ec_r * ec_b->uv_stride, 1,
                   ec_b->uv_width, ec_f);
        }
        fclose(ec_f);
      }
    }
  }

"""
assert anchor in s, "cdef alloc anchor moved"
s = s.replace(anchor, dump + anchor, 1)
open(path, "w").write(s)
print("prefilt-wide dump instrumented")
PYW
# --- rung 8: palette colour-index map range ladder (detokenize.c) -------
# EC_TRACE_PALETTE=1 -> "EC_PAL row=.. col=.. ctx=.. n=.. rng=.." before every
# colour-index symbol in decode_color_map_tokens's wavefront, "EC_PAL_VAL
# row=.. col=.. color_idx=.. rng=.." after. lane-palette r4: r3 already
# cleared every table/context function against this same source line-for-line
# by hand; this rung is what lets a real per-symbol range compare (class
# compare-range-not-tell / equal-range-means-unread) replace that by-hand
# check instead of re-reading the tables again.
I="$SRC/av1/decoder/detokenize.c"
[ -f "$I" ] || { echo "no oracle source at $I" >&2; exit 1; }

python3 - "$I" <<'PYP'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_PALETTE" in s:
    print("palette map already instrumented (no-op)")
    sys.exit(0)

old = """      const int color_ctx = av1_get_palette_color_index_context(
          color_map, plane_block_width, (i - j), j, n, color_order, NULL);
      const int color_idx = aom_read_symbol(
          r, color_map_cdf[n - PALETTE_MIN_SIZE][color_ctx], n, ACCT_STR);
      assert(color_idx >= 0 && color_idx < n);
      color_map[(i - j) * plane_block_width + j] = color_order[color_idx];"""
new = """/* EC_INSTRUMENTED_PALETTE */
      const int color_ctx = av1_get_palette_color_index_context(
          color_map, plane_block_width, (i - j), j, n, color_order, NULL);
      const int ec_pal_trace = getenv("EC_TRACE_PALETTE") != NULL;
      if (ec_pal_trace) {
        fprintf(stderr, "EC_PAL row=%d col=%d ctx=%d n=%d rng=%u\\n", (i - j),
                j, color_ctx, n, (unsigned)r->ec.rng);
      }
      const int color_idx = aom_read_symbol(
          r, color_map_cdf[n - PALETTE_MIN_SIZE][color_ctx], n, ACCT_STR);
      assert(color_idx >= 0 && color_idx < n);
      color_map[(i - j) * plane_block_width + j] = color_order[color_idx];
      if (ec_pal_trace) {
        fprintf(stderr, "EC_PAL_VAL row=%d col=%d color_idx=%d rng=%u\\n",
                (i - j), j, color_idx, (unsigned)r->ec.rng);
      }"""
assert old in s, "decode_color_map_tokens loop body moved"
s = s.replace(old, new, 1)
s = s.replace(
    "static void decode_color_map_tokens(Av1ColorMapParam *param, aom_reader *r) {",
    "/* EC_INSTRUMENTED_PALETTE */\nstatic void decode_color_map_tokens(Av1ColorMapParam *param, aom_reader *r) {",
    1,
)
open(path, "w").write(s)
print("palette map instrumented")
PYP

# --- rung 8b: palette map[0] (av1_read_uniform) range ladder ------------
python3 - "$I" <<'PYP0'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_PALETTE_UNIFORM" in s:
    print("palette map[0] already instrumented (no-op)")
    sys.exit(0)

old = """  // The first color index.
  color_map[0] = av1_read_uniform(r, n);
  assert(color_map[0] < n);"""
new = """  // The first color index.
  /* EC_INSTRUMENTED_PALETTE_UNIFORM */
  if (getenv("EC_TRACE_PALETTE") != NULL) {
    fprintf(stderr, "EC_PAL row=0 col=0 ctx=-1 n=%d rng=%u\\n", n,
            (unsigned)r->ec.rng);
  }
  color_map[0] = av1_read_uniform(r, n);
  assert(color_map[0] < n);
  if (getenv("EC_TRACE_PALETTE") != NULL) {
    fprintf(stderr, "EC_PAL_VAL row=0 col=0 color_idx=%d rng=%u\\n",
            color_map[0], (unsigned)r->ec.rng);
  }"""
assert old in s, "color_map[0] uniform read moved"
s = s.replace(old, new, 1)
open(path, "w").write(s)
print("palette map[0] instrumented")
PYP0

# --- rung 9: export calculate_intermediate_result for a direct harness call
I="$SRC/av1/common/restoration.c"
[ -f "$I" ] || { echo "no oracle source at $I" >&2; exit 1; }

python3 - "$I" <<'PYA'
import sys
path = sys.argv[1]
s = open(path).read()
if "/* EC_INSTRUMENTED_AB */" in s:
    print("calculate_intermediate_result already exported (no-op)")
    sys.exit(0)

old = """static void calculate_intermediate_result(int32_t *dgd, int width, int height,"""
new = """/* EC_INSTRUMENTED_AB */
void calculate_intermediate_result(int32_t *dgd, int width, int height,"""
assert old in s, "calculate_intermediate_result signature moved"
s = s.replace(old, new, 1)
open(path, "w").write(s)
print("calculate_intermediate_result exported")
PYA
