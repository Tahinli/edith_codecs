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
#       (rung 13 adds "stack=" to that line and an "EC_MODE_MV" line right
#       after assign_mv)
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
#   EC_AV1_POSTCDEF_DUMP=<prefix> -> <prefix>.f<N> per frame, same shape as
#       the POSTDEBLOCK rung (full ALIGNED-buffer rows), written right after
#       the CDEF pass and before superres/LR -- splits a final-output
#       mismatch into "already in CDEF" vs "introduced by loop restoration"
#       without the 16-bit-only DUMP16 rungs (lane-av1422filter: on the
#       pinned 4:2:2 keyframe this named the 251/259-sample CDEF chroma
#       defect whose first samples were the ticket's first final samples).
#   EC_TRACE_MODE_STEP=1 -> per-symbol range ladder inside
#       ec_read_intra_frame_mode_info_impl (rung 5's renamed body):
#       "EC_ISTEP mi_row=.. mi_col=.. name=skip val=.. rng=.." after
#       read_skip_txfm, name=cdef/dq after read_cdef/read_delta_q_params,
#       name=mode after mbmi->mode, name=angle_y after the Y angle_delta read,
#       name=uv_mode after mbmi->uv_mode, name=angle_uv after the UV
#       angle_delta read (rung 10, lane-sbpart r6). EC_TRACE_MODE's own
#       before/after prints only bracket the WHOLE block; this is the
#       per-symbol ladder the charter asked for to localize block2's first
#       wrong read.
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

# --- rung 10: per-symbol range ladder inside intra key-frame mode info --
python3 - "$SRC/av1/decoder/decodemv.c" <<'PYS'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_INTRA_STEP" in s:
    print("intra mode-info per-symbol ladder already instrumented (no-op)")
    sys.exit(0)

def step(name, val):
    return """  {
    const int ec_trace = getenv("EC_TRACE_MODE_STEP") != NULL;
    if (ec_trace) {
      fprintf(stderr, "EC_ISTEP mi_row=%%d mi_col=%%d name=%s val=%%d rng=%%u\\n",
              xd->mi_row, xd->mi_col, (int)(%s), (unsigned)r->ec.rng);
    }
  }
""" % (name, val)

old = "  mbmi->skip_txfm = read_skip_txfm(cm, xd, mbmi->segment_id, r);\n"
assert old in s, "read_skip_txfm call moved"
s = s.replace(old, old + step("skip", "mbmi->skip_txfm"), 1)

old = "  read_cdef(cm, r, xd);\n"
assert old in s, "read_cdef call moved"
s = s.replace(old, old + step("cdef", "0"), 1)

old = "  read_delta_q_params(cm, xd, r);\n"
assert old in s, "read_delta_q_params call moved"
s = s.replace(old, old + step("dq", "xd->current_base_qindex"), 1)

old = "  mbmi->mode = read_intra_mode(r, get_y_mode_cdf(ec_ctx, above_mi, left_mi));\n"
assert old in s, "y mode read moved"
s = s.replace(old, old + step("mode", "mbmi->mode"), 1)

old = """  mbmi->angle_delta[PLANE_TYPE_Y] =
      (use_angle_delta && av1_is_directional_mode(mbmi->mode))
          ? read_angle_delta(r, ec_ctx->angle_delta_cdf[mbmi->mode - V_PRED])
          : 0;
"""
assert old in s, "angle_delta_y block moved"
s = s.replace(old, old + step("angle_y", "mbmi->angle_delta[PLANE_TYPE_Y]"), 1)

old = """    mbmi->uv_mode =
        read_intra_mode_uv(ec_ctx, r, is_cfl_allowed(xd), mbmi->mode);
"""
assert old in s, "uv_mode read moved"
s = s.replace(old, old + step("uv_mode", "mbmi->uv_mode"), 1)

old = """    mbmi->angle_delta[PLANE_TYPE_UV] =
        (use_angle_delta && av1_is_directional_mode(intra_mode))
            ? read_angle_delta(r, ec_ctx->angle_delta_cdf[intra_mode - V_PRED])
            : 0;
"""
assert old in s, "angle_delta_uv block moved"
s = s.replace(old, old + step("angle_uv", "mbmi->angle_delta[PLANE_TYPE_UV]"), 1)

marker_old = "static int read_mv_component(aom_reader *r, nmv_component *mvcomp,"
assert marker_old in s, "anchor for EC_INSTRUMENTED_INTRA_STEP marker moved"
s = s.replace(marker_old, "/* EC_INSTRUMENTED_INTRA_STEP */\n" + marker_old, 1)

open(path, "w").write(s)
print("intra mode-info per-symbol ladder instrumented")
PYS

# --- rung 11: per-symbol range ladder inside read_coeffs_txb (decodetxb.c) --
# lane-sbpart r7: rung 3's EC_TRACE_COEFF only bracketed a whole coefficient
# block (entry/exit rng) -- too coarse to find WHICH symbol inside a Luma64
# corner-scan block diverges. This adds EC_COEFF_STEP tag=eob/base_eob/
# after_bases/sign/post_golomb lines, rng after each, under the same
# EC_TRACE_COEFF flag (no new env var). Localized r7's own bisect to the
# `base` symbol at scan position (row=1,col=0) inside block2's luma corner:
# entry rng and eob/base_eob both match ours byte-for-byte, then our `base`
# read at that position decodes value=3 (triggering an extra `br` read)
# where the oracle's equivalent position decodes level=1 -- see
# lanes/sbpart-r7.report.md.
G="$SRC/av1/decoder/decodetxb.c"
[ -f "$G" ] || { echo "no oracle source at $G" >&2; exit 1; }

python3 - "$G" <<'PYC11'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_COEFF_STEP" in s:
    print("decodetxb rung 11 already applied (no-op)")
    sys.exit(0)

old1 = "  *eob = rec_eob_pos(eob_pt, eob_extra);\n"
new1 = old1 + """  {
    const int ec_trace2 = getenv("EC_TRACE_COEFF") != NULL;
    if (ec_trace2) {
      fprintf(stderr, "EC_COEFF_STEP tag=eob eob=%d rng=%u\\n", *eob, (unsigned)r->ec.rng);
    }
  }
"""
assert old1 in s, "eob assign not found"
s = s.replace(old1, new1, 1)

old2 = "    levels[get_padded_idx(pos, bhl)] = level;\n  }\n  if (*eob > 1) {"
new2 = """    levels[get_padded_idx(pos, bhl)] = level;
    if (getenv("EC_TRACE_COEFF") != NULL) {
      fprintf(stderr, "EC_COEFF_STEP tag=base_eob level=%d rng=%u\\n", level, (unsigned)r->ec.rng);
    }
  }
  if (*eob > 1) {"""
assert old2 in s, "base_eob block not found"
s = s.replace(old2, new2, 1)

old3 = """      read_coeffs_reverse(r, tx_size, tx_class, 0, *eob - 1 - 1, scan, bhl,
                          levels, base_cdf, br_cdf);
    }
  }
"""
new3 = old3 + """  if (getenv("EC_TRACE_COEFF") != NULL) {
    fprintf(stderr, "EC_COEFF_STEP tag=after_bases rng=%u\\n", (unsigned)r->ec.rng);
  }
"""
assert old3 in s, "after_bases site not found"
s = s.replace(old3, new3, 1)

old4 = """      if (level >= MAX_BASE_BR_RANGE) {
        level += read_golomb(xd, r);
      }

      if (c == 0) dc_val = sign ? -level : level;"""
new4 = """      if (getenv("EC_TRACE_COEFF") != NULL) {
        fprintf(stderr, "EC_COEFF_STEP tag=sign c=%d sign=%d rng=%u\\n", c, sign, (unsigned)r->ec.rng);
      }
      if (level >= MAX_BASE_BR_RANGE) {
        level += read_golomb(xd, r);
      }
      if (getenv("EC_TRACE_COEFF") != NULL) {
        fprintf(stderr, "EC_COEFF_STEP tag=post_golomb c=%d level=%d rng=%u\\n", c, level, (unsigned)r->ec.rng);
      }

      if (c == 0) dc_val = sign ? -level : level;"""
assert old4 in s, "sign/golomb site not found"
s = s.replace(old4, new4, 1)

open(path, "w").write(s)
print("decodetxb rung 11 (per-symbol coeff ladder) instrumented")
PYC11

# --- rung 12: FINAL reconstruction dump (lane-hidden r1) ----------------
# EC_AV1_FINAL_DUMP=<prefix> -> <prefix>.f<N> per DECODED frame (decode order,
# hidden alt-ref frames included), written after CDEF + superres + loop
# restoration have all run, i.e. exactly the frame as it is stored into the
# reference buffer. Y then U then V, crop-sized rows (post-superres
# y_crop_width), 8-bit as u8 and high bitdepth as u16 LE -- bit-depth
# correct, unlike the u8-narrowing debug dumps.
#
# Rungs 1/6 (pre-filter / post-deblock) stop before CDEF+LR+superres, and
# every ffmpeg/aomdec pixel gate in this repo compares SHOWN frames only
# (class gate-blind-to-hidden-frames), so before this rung no instrument in
# the repo could compare a hidden frame's final pixels at all.
python3 - "$F" <<'PYF'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_FINAL" in s:
    print("final dump already instrumented (no-op)")
    sys.exit(0)

anchor = """  if (!pbi->dcb.corrupted) {
    if (cm->features.refresh_frame_context == REFRESH_FRAME_CONTEXT_BACKWARD) {"""
dump = """  /* EC_INSTRUMENTED_FINAL */
  {
    const char *ec_dump = getenv("EC_AV1_FINAL_DUMP");
    if (ec_dump) {
      static int ec_final_idx = 0;
      char ec_path[1024];
      snprintf(ec_path, sizeof(ec_path), "%s.f%d", ec_dump, ec_final_idx++);
      FILE *ec_f = fopen(ec_path, "wb");
      if (ec_f) {
        const YV12_BUFFER_CONFIG *ec_b = &cm->cur_frame->buf;
        const int ec_hbd = (ec_b->flags & YV12_FLAG_HIGHBITDEPTH) != 0;
        const uint8_t *const ec_p8[3] = { ec_b->y_buffer, ec_b->u_buffer,
                                          ec_b->v_buffer };
        const int ec_st[3] = { ec_b->y_stride, ec_b->uv_stride,
                               ec_b->uv_stride };
        const int ec_w[3] = { ec_b->y_crop_width, ec_b->uv_crop_width,
                              ec_b->uv_crop_width };
        const int ec_h[3] = { ec_b->y_crop_height, ec_b->uv_crop_height,
                              ec_b->uv_crop_height };
        for (int ec_pl = 0; ec_pl < (num_planes > 1 ? 3 : 1); ++ec_pl) {
          for (int ec_r = 0; ec_r < ec_h[ec_pl]; ++ec_r) {
            if (ec_hbd) {
              const uint16_t *ec_s = CONVERT_TO_SHORTPTR(ec_p8[ec_pl]);
              fwrite(ec_s + (size_t)ec_r * ec_st[ec_pl], 2, ec_w[ec_pl], ec_f);
            } else {
              fwrite(ec_p8[ec_pl] + (size_t)ec_r * ec_st[ec_pl], 1,
                     ec_w[ec_pl], ec_f);
            }
          }
        }
        fclose(ec_f);
      }
    }
  }

"""
assert anchor in s, "final dump anchor moved"
s = s.replace(anchor, dump + anchor, 1)
open(path, "w").write(s)
print("final dump instrumented")
PYF

# --- rung 13: MV ground truth inside the inter mode ladder (lane-inter8 r3) --
# lane-inter8 r3 patched the live oracle tree BY HAND and rebuilt aomdec, so
# these two probes existed only in ~/.cache/aom-oracle/src and any rebuild
# from this script would have silently dropped them (class: instrument that
# lives only in a build tree). Both extend rung 4 and are gated by the same
# EC_TRACE_MODE:
#   * EC_MODE_MV .. mode=.. mv0=(..,..) rng=..  -- printed immediately after
#     assign_mv, i.e. the MV as decoded, BEFORE interintra/motion-mode/
#     interp-filter syntax moves the range further; lets a lane separate an
#     MV defect from a later-symbol defect on the same block.
#   * stack=<ref_mv_count[ref_frame_type]> added to rung 4's EC_MODE_VAL --
#     the reference's own mv-stack depth for the block's reference pair.
H="$SRC/av1/decoder/decodemv.c"
[ -f "$H" ] || { echo "no oracle source at $H" >&2; exit 1; }

python3 - "$H" <<'PYMV'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_MODE_MV" in s:
    print("decodemv rung 13 already applied (no-op)")
    sys.exit(0)
assert "EC_INSTRUMENTED" in s, "rung 13 needs rung 4 (run this script top to bottom)"

# (a) stack= on rung 4's EC_MODE_VAL
old_val = """            "mv0=(%d,%d) rng=%u\\n",
            ec_xd->mi_row, ec_xd->mi_col, (int)mbmi->mode,
            (int)mbmi->ref_frame[0], (int)mbmi->ref_frame[1],
            mbmi->mv[0].as_mv.row, mbmi->mv[0].as_mv.col,
            (unsigned)r->ec.rng);"""
new_val = """            "mv0=(%d,%d) stack=%d rng=%u\\n",
            ec_xd->mi_row, ec_xd->mi_col, (int)mbmi->mode,
            (int)mbmi->ref_frame[0], (int)mbmi->ref_frame[1],
            mbmi->mv[0].as_mv.row, mbmi->mv[0].as_mv.col,
            /* EC_STACK */ (int)dcb->ref_mv_count[av1_ref_frame_type(mbmi->ref_frame)],
            (unsigned)r->ec.rng);"""
assert old_val in s, "rung 4's EC_MODE_VAL body moved"
s = s.replace(old_val, new_val, 1)

# (b) EC_MODE_MV right after assign_mv's corrupted-flag merge
anchor = "  aom_merge_corrupted_flag(&dcb->corrupted, mv_corrupted_flag);\n"
assert s.count(anchor) == 1, "assign_mv corrupted-flag merge is no longer unique"
probe = """  if (getenv("EC_TRACE_MODE") != NULL) {
    fprintf(stderr,
            "EC_MODE_MV mi_row=%d mi_col=%d mode=%d mv0=(%d,%d) rng=%u\\n",
            xd->mi_row, xd->mi_col, (int)mbmi->mode, mbmi->mv[0].as_mv.row,
            mbmi->mv[0].as_mv.col, (unsigned)r->ec.rng);
  }
"""
s = s.replace(anchor, anchor + probe, 1)
open(path, "w").write(s)
print("decodemv rung 13 (EC_MODE_MV + EC_MODE_VAL stack=) instrumented")
PYMV

# --- rung 14: temporal-MV (MFMV) ground truth (lane-interbis, scripted by ----
# --- lane-refstamp r1) -----------------------------------------------------
# lane-interbis patched the live oracle tree BY HAND for its motion-field hunt,
# so these two probes lived only in ~/.cache/aom-oracle/src and a rebuild from
# this script would have dropped them (class: instrument that lives only in a
# build tree -- same reason rung 13 exists).
#   * EC_TRACE_TPL=1 -> "EC_TPL mi_row=.. mi_col=.. blk=(..,..) mfmv0=(..,..)
#     rfo=.." (or ".. INVALID") for every temporal candidate `add_tpl_ref_mv`
#     probes -- the reference's own projected motion field, cell by cell, to
#     compare against ours (EC_TRACE_TPL on our side).
#   * the resolved MV STACK itself, one "EC_STACK mi_row=.. mi_col=.. ref=..
#     i=.. this=(..,..) comp=(..,..) w=.." line per entry, printed under rung
#     4's EC_TRACE_MODE right after EC_MODE_VAL (rung 13 only prints the
#     stack DEPTH).
python3 - "$SRC/av1/common/mvref_common.c" <<'PYT'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_TRACE_TPL" in s:
    print("mvref_common rung 14 already applied (no-op)")
    sys.exit(0)
old = "  if (prev_frame_mvs->mfmv0.as_int == INVALID_MV) return 0;\n"
assert old in s, "add_tpl_ref_mv's INVALID_MV early return moved"
new = """  if (prev_frame_mvs->mfmv0.as_int == INVALID_MV) {
    if (getenv("EC_TRACE_TPL"))
      fprintf(stderr, "EC_TPL mi_row=%d mi_col=%d blk=(%d,%d) INVALID\\n",
              mi_row, mi_col, blk_row, blk_col);
    return 0;
  }
  if (getenv("EC_TRACE_TPL"))
    fprintf(stderr,
            "EC_TPL mi_row=%d mi_col=%d blk=(%d,%d) mfmv0=(%d,%d) rfo=%d\\n",
            mi_row, mi_col, blk_row, blk_col, prev_frame_mvs->mfmv0.as_mv.row,
            prev_frame_mvs->mfmv0.as_mv.col, prev_frame_mvs->ref_frame_offset);
"""
s = s.replace(old, new, 1)
open(path, "w").write(s)
print("mvref_common rung 14 (EC_TPL) instrumented")
PYT

python3 - "$SRC/av1/decoder/decodemv.c" <<'PYST'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_STACK mi_row" in s:
    print("decodemv rung 14 already applied (no-op)")
    sys.exit(0)
assert "EC_MODE_MV" in s, "rung 14 needs rung 13 (run this script top to bottom)"
anchor = """            /* EC_STACK */ (int)dcb->ref_mv_count[av1_ref_frame_type(mbmi->ref_frame)],
            (unsigned)r->ec.rng);
"""
assert s.count(anchor) == 1, "rung 13's EC_MODE_VAL body moved"
dump = """    /* EC_INSTRUMENTED lane-interbis: the resolved mv stack itself */
    const MV_REFERENCE_FRAME ec_rt = av1_ref_frame_type(mbmi->ref_frame);
    for (int ec_i = 0; ec_i < dcb->ref_mv_count[ec_rt]; ec_i++) {
      fprintf(stderr,
              "EC_STACK mi_row=%d mi_col=%d ref=%d i=%d this=(%d,%d) "
              "comp=(%d,%d) w=%d\\n",
              ec_xd->mi_row, ec_xd->mi_col, (int)ec_rt, ec_i,
              ec_xd->ref_mv_stack[ec_rt][ec_i].this_mv.as_mv.row,
              ec_xd->ref_mv_stack[ec_rt][ec_i].this_mv.as_mv.col,
              ec_xd->ref_mv_stack[ec_rt][ec_i].comp_mv.as_mv.row,
              ec_xd->ref_mv_stack[ec_rt][ec_i].comp_mv.as_mv.col,
              ec_xd->weight[ec_rt][ec_i]);
    }
"""
s = s.replace(anchor, anchor + dump, 1)
open(path, "w").write(s)
print("decodemv rung 14 (per-entry EC_STACK) instrumented")
PYST

# --- rung 15: post-CDEF, pre-superres 8-bit row dump (lane-av1422filter) ---
# The 8-bit mirror of rung 6 one stage later: written right after the CDEF
# pass (the `if (do_cdef) {...}` block) and before superres/LR, so a final
# chroma/luma mismatch splits into "already in CDEF" vs "introduced by
# loop restoration". The *_DUMP16 family covers this stage only in 16-bit
# builds (its 8-bit run SEGFAULTS); this rung is the 8-bit one. lane-
# av1422filter added it to the live oracle tree by hand first, then it was
# transcribed here (class: instrument that lives only in a build tree).
python3 - "$F" <<'PYC'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_INSTRUMENTED_POSTCDEF" in s:
    print("postcdef dump already instrumented (no-op)")
    sys.exit(0)

anchor = """      if (do_cdef) {
        if (pbi->num_workers > 1) {
          av1_cdef_frame_mt(cm, &pbi->dcb.xd, pbi->cdef_worker,
                            pbi->tile_workers, &pbi->cdef_sync,
                            pbi->num_workers, av1_cdef_init_fb_row_mt,
                            do_extend_border_mt);
        } else {
          av1_cdef_frame(&pbi->common.cur_frame->buf, cm, &pbi->dcb.xd,
                         av1_cdef_init_fb_row);
        }
      }
"""
dump = """
      /* EC_INSTRUMENTED_POSTCDEF (8-bit twin of the POSTDEBLOCK rung:
       * full aligned buffer, post-CDEF pre-superres/LR) */
      {
        const char *ec_dump = getenv("EC_AV1_POSTCDEF_DUMP");
        if (ec_dump) {
          static int ec_postcdef_idx = 0;
          char ec_path[1024];
          snprintf(ec_path, sizeof(ec_path), "%s.f%d", ec_dump,
                   ec_postcdef_idx++);
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
assert anchor in s, "do_cdef block anchor moved"
s = s.replace(anchor, anchor + dump, 1)
open(path, "w").write(s)
print("postcdef dump instrumented")
PYC

# --- depth contract for the four u8-narrowing dump rungs ------------------
# PREFILT (rung 1), POSTDEBLOCK (rung 3), PREFILT_WIDE (rung 7) and POSTCDEF
# are paired with ec-av1 decoder rungs that narrow every sample with `as u8`,
# so the oracle side owes ONE BYTE PER SAMPLE at every depth. As instrumented
# above they wrote `fwrite(plane + r*stride, 1, w, f)`, which is wrong twice
# over on a high-bit-depth stream:
#   1. YV12_BUFFER_CONFIG stores plane pointers in HALVED form -- aom_ports/mem.h
#      defines CONVERT_TO_BYTEPTR(x) = x >> 1 -- so the real address is
#      CONVERT_TO_SHORTPTR(p) (p << 1). Pointer arithmetic on the raw uint8_t*
#      therefore yields an unrelated half-address, and fwrite faults inside
#      memmove: SIGSEGV (exit 139) with a 0-byte file, on EVERY HBD stream, for
#      all four rungs, while rung 12 (which does convert) works. A lane hitting
#      it sees a crashed aomdec and cannot tell it from a decoder defect.
#   2. y_stride/uv_stride count SAMPLES, so `+ r*stride` in bytes also picks the
#      wrong row even where the address happens to be readable.
# At 8 bit neither applies: buf is the real pointer and flags is 0, so the
# bytes emitted are bit-for-bit what these rungs always wrote. This block owns
# the contract for all four, runs last, and is a REPAIR pass: it rewrites the
# loops an earlier (or pre-fix) instrumentation left behind, so re-running this
# script on an already-instrumented tree fixes it in place.
python3 - "$F" <<'PYN'
import re
import sys

path = sys.argv[1]
s = open(path).read()

helper = '''/* EC_INSTRUMENTED_NARROW_ROW: emit one plane row as ONE BYTE PER SAMPLE, the
 * shape the ec-av1 decoder's own EC_AV1_*_DUMP rungs write (they narrow every
 * sample with `as u8`).
 *
 * Two HBD traps this replaces, both fatal until fixed:
 *  1. YV12_BUFFER_CONFIG stores plane pointers in HALVED form: the real
 *     address is `CONVERT_TO_SHORTPTR(p)` (aom_ports/mem.h defines
 *     `CONVERT_TO_BYTEPTR(x) = x >> 1`), so `p + r*stride` computed on the raw
 *     uint8_t* is an unrelated address. `fwrite(y_buffer + r*y_stride, 1, w,
 *     f)` on a 10/12-bit stream therefore hands fwrite a half-address that
 *     faults inside memmove -- SIGSEGV with a 0-byte file, which reads as a
 *     decoder crash rather than a broken rung.
 *  2. y_stride/uv_stride count SAMPLES, so a byte-wise `+ r*stride` addresses
 *     the wrong row as well.
 * At 8 bit neither applies: the pointer is the real one and the bytes written
 * are exactly what the rung always emitted. */
static void ec_dump_narrow_row(FILE *f, const YV12_BUFFER_CONFIG *b,
                               const uint8_t *plane8, int stride, int row,
                               int w) {
  if (!(b->flags & YV12_FLAG_HIGHBITDEPTH)) {
    fwrite(plane8 + (size_t)row * stride, 1, (size_t)w, f);
    return;
  }
  const uint16_t *p = CONVERT_TO_SHORTPTR(plane8) + (size_t)row * stride;
  uint8_t *narrow = (uint8_t *)aom_malloc((size_t)w);
  if (!narrow) return;
  for (int c = 0; c < w; ++c) narrow[c] = (uint8_t)(p[c] & 0xFF);
  fwrite(narrow, 1, (size_t)w, f);
  aom_free(narrow);
}

/* EC_INSTRUMENTED_DUMP_CHECK: the harm the depth bug above caused is not a
 * wrong number, it is a SILENT 0-byte dump -- downstream that reads as "the
 * oracle has no data for this stage" and becomes a phantom stage diff. So
 * every narrowing rung checks the byte count it just produced against the
 * shape it claims to write and aborts IN THE ORACLE, naming itself, instead
 * of handing the decoder an empty file. `expect` is Y + U + V at ONE BYTE PER
 * SAMPLE: the crop extent when `aligned` is 0 (PREFILT), the mi-aligned
 * extent when 1 (PREFILT_WIDE / POSTDEBLOCK / POSTCDEF). No configuration
 * writes 0 bytes: a frame's crop extent is non-zero by construction, and a
 * failed fopen skips the block entirely (so a bad path is not an abort). */
static void ec_dump_finish(FILE *f, const char *rung,
                           const YV12_BUFFER_CONFIG *b, int num_planes,
                           int aligned) {
  const size_t y = aligned ? (size_t)b->y_width * b->y_height
                           : (size_t)b->y_crop_width * b->y_crop_height;
  const size_t uv = aligned ? (size_t)b->uv_width * b->uv_height
                            : (size_t)b->uv_crop_width * b->uv_crop_height;
  const size_t expect = y + (num_planes > 1 ? 2 * uv : 0);
  const long wrote = ftell(f);
  if (wrote < 0 || (size_t)wrote != expect || fflush(f) != 0) {
    fprintf(stderr,
            "EC_DUMP_ABORT rung=%s bit_depth=%u hbd=%d expect=%zu wrote=%ld "
            "(one byte per sample, Y+U+V)\\n",
            rung, b->bit_depth,
            (b->flags & YV12_FLAG_HIGHBITDEPTH) ? 1 : 0, expect, wrote);
    abort();
  }
  fclose(f);
}

'''

MARKER_ROW = "EC_INSTRUMENTED_NARROW_ROW"
MARKER_CHECK = "EC_INSTRUMENTED_DUMP_CHECK"
missing = ""
if MARKER_ROW not in s:
    missing = helper
elif MARKER_CHECK not in s:
    # The tree already carries the row helper from an earlier run of this
    # script: add the byte-count checker on its own so a repaired tree
    # converges instead of erroring.
    missing = helper[helper.index("/* " + MARKER_CHECK):]
if missing:
    # Both helpers must precede every user: the POSTCDEF rung was refactored
    # into a static helper that sits ABOVE av1_decode_tg_tiles_and_wrapup, so
    # anchor there when it exists and at the function itself otherwise.
    for anchor in ("static void ec_dump_postcdef(const AV1_COMMON *cm, int num_planes) {",
                   "void av1_decode_tg_tiles_and_wrapup(AV1Decoder *pbi, const uint8_t *data,"):
        if anchor in s:
            s = s.replace(anchor, missing + anchor, 1)
            break
    else:
        raise SystemExit("dump helpers: no anchor before the dump rungs")
    print("dump helpers inserted: %s"
          % ("narrow-row + byte-count check" if len(missing) == len(helper)
             else "byte-count check only"))

row_loop = re.compile(
    r"for \(int ec_r = 0; ec_r < ec_b->(\w+); \+\+ec_r\)\n"
    r"(\s*)fwrite\(ec_b->(\w+)_buffer \+ ec_r \* ec_b->(\w+), 1,\s*\n?\s*ec_b->(\w+),\s*\n?\s*ec_f\);")


def narrow(match):
    rows, indent, plane, stride, width = match.groups()
    return ("for (int ec_r = 0; ec_r < ec_b->%s; ++ec_r)\n%s"
            "ec_dump_narrow_row(ec_f, ec_b, ec_b->%s_buffer, ec_b->%s, ec_r,\n"
            "%s%s ec_b->%s);" % (rows, indent, plane, stride, indent, indent, width))


s, rewritten = row_loop.subn(narrow, s)
if rewritten:
    print("narrow-row depth fix: rewrote %d plane row loops" % rewritten)
else:
    print("narrow-row depth fix: no legacy row loop left (already depth-correct)")

# Close each narrowing rung through the checker. Keyed on the presence of a
# rewritten row loop, so rung 12 (EC_AV1_FINAL_DUMP, 2 bytes per sample and
# already depth-correct) is never reached: its block has no ec_dump_narrow_row.
# The terminator alternation keeps each block matched to ITS OWN close, so a
# re-run cannot run a block's body forward into a later rung's fclose.
dump_block = re.compile(
    r'(FILE \*ec_f = fopen\(ec_path, "wb"\);)(.*?)'
    r'((?:fclose\(ec_f\);)|(?:ec_dump_finish\([^;]*\);))',
    re.S)

WIRED = []

def finish(match):
    opened, body, closed = match.groups()
    if "ec_dump_narrow_row" not in body:
        return match.group(0)
    # The rung name can sit ABOVE the fopen (the POSTCDEF rung was refactored
    # into a helper that does getenv -> fopen at the top), so take the nearest
    # preceding EC_AV1_* env lookup rather than searching the block body.
    preceding = re.findall(r'getenv\("(EC_AV1_[A-Z_]+)"\)', s[:match.start()])
    if not preceding:
        raise SystemExit("dump check: no rung env var above a narrowing block")
    aligned = 0 if "y_crop_height" in body else 1
    if closed.startswith("fclose"):
        WIRED.append(preceding[-1])
    return "%s%s%s" % (
        opened, body,
        'ec_dump_finish(ec_f, "%s", ec_b, num_planes, %d);' % (preceding[-1], aligned))


s, _ = dump_block.subn(finish, s)
if WIRED:
    print("dump byte-count check: wired into %d narrowing rung(s): %s"
          % (len(WIRED), ", ".join(WIRED)))
else:
    print("dump byte-count check: every narrowing rung already checks its bytes")
open(path, "w").write(s)
PYN

# --- rung 16: EC_PREDOUT8, the 8-bit predicted block (lane-av1cmpaudit) ----
# This rung lived ONLY in the hand-patched oracle tree -- `instrument-aom-
# oracle.sh` never transcribed it -- so a rebuild from this script silently
# dropped it (class: instrument that lives only in a build tree, the same
# reason rung 13 exists). It is transcribed here at BOTH 8-bit prediction
# sites in reconintra.c: the non-directional early-return path (DC, SMOOTH*,
# PAETH) and the directional / filter-intra tail. lane-av1cmpaudit also grew
# `mode=` on both: without it, an agreeing-top / disagreeing-bottom pattern
# on a non-directional unit could not be attributed to a mode at all.
# `mode=` sits after `txh=`, matching EC_PREDND's own field order; every
# pre-existing field keeps its position.
#
# Idempotent, and an UPGRADE as well as an install: the oracle tree's git HEAD
# already carries the rung WITHOUT `mode=`, so the script has to rewrite that
# form too, not just insert the block into a pristine upstream file.
I="$SRC/av1/common/reconintra.c"
[ -f "$I" ] || { echo "no oracle source at $I" >&2; exit 1; }

python3 - "$I" <<'PYRI'
import sys
path = sys.argv[1]
s = open(path).read()

if 'EC_PREDOUT8' not in s:
    # Pristine upstream: insert the rung at both 8-bit prediction sites.
    nd_old = """    build_non_directional_intra_predictors(ref, ref_stride, dst, dst_stride,
                                           mode, tx_size, n_top_px, n_left_px);
    return;
  }"""
    nd_new = """    build_non_directional_intra_predictors(ref, ref_stride, dst, dst_stride,
                                           mode, tx_size, n_top_px, n_left_px);
    /* EC_INSTRUMENTED_PREDOUT8 (non-directional, 8-bit) */
    if (getenv("EC_PRED")) {
      long ec_sum = 0;
      for (int r = 0; r < txhpx; ++r)
        for (int c = 0; c < txwpx; ++c) ec_sum += dst[r * dst_stride + c];
      fprintf(stderr, "EC_PREDOUT8 mi_row=%d mi_col=%d plane=%d row_off=%d col_off=%d txw=%d txh=%d mode=%d sum=%ld row0=", xd->mi_row, xd->mi_col, plane, row_off, col_off, txwpx, txhpx, mode, ec_sum);
      for (int c = 0; c < txwpx && c < 8; ++c) fprintf(stderr, "%d,", dst[c]);
      fprintf(stderr, " col0=");
      for (int r = 0; r < txhpx && r < 8; ++r) fprintf(stderr, "%d,", dst[r * dst_stride]);
      fprintf(stderr, "\\n");
    }
    return;
  }"""
    assert nd_old in s, "non-directional intra predictor tail moved"
    s = s.replace(nd_old, nd_new, 1)

    dir_old = """  build_directional_and_filter_intra_predictors(
      ref, ref_stride, dst, dst_stride, mode, p_angle, filter_intra_mode,
      tx_size, disable_edge_filter, n_top_px, n_topright_px, n_left_px,
      n_bottomleft_px, intra_edge_filter_type);
}"""
    dir_new = """  build_directional_and_filter_intra_predictors(
      ref, ref_stride, dst, dst_stride, mode, p_angle, filter_intra_mode,
      tx_size, disable_edge_filter, n_top_px, n_topright_px, n_left_px,
      n_bottomleft_px, intra_edge_filter_type);
  /* EC_INSTRUMENTED_PREDOUT8 (directional / filter-intra, 8-bit) */
  if (getenv("EC_PRED")) {
    long ec_sum = 0;
    for (int r = 0; r < txhpx; ++r)
      for (int c = 0; c < txwpx; ++c) ec_sum += dst[r * dst_stride + c];
    fprintf(stderr, "EC_PREDOUT8 mi_row=%d mi_col=%d plane=%d row_off=%d col_off=%d txw=%d txh=%d mode=%d sum=%ld row0=", mi_row, mi_col, plane, row_off, col_off, txwpx, txhpx, mode, ec_sum);
    for (int c = 0; c < txwpx && c < 8; ++c) fprintf(stderr, "%d,", dst[c]);
    fprintf(stderr, " col0=");
    for (int r = 0; r < txhpx && r < 8; ++r) fprintf(stderr, "%d,", dst[r * dst_stride]);
    fprintf(stderr, "\\n");
  }
}"""
    assert dir_old in s, "directional intra predictor tail moved"
    s = s.replace(dir_old, dir_new, 1)
    open(path, "w").write(s)
    print("EC_PREDOUT8 installed (both 8-bit paths, with mode=)")
    sys.exit(0)

# Already present: upgrade the rung to carry `mode=` if it does not yet.
nd_fmt_old = 'txw=%d txh=%d sum=%ld row0=", xd->mi_row'
nd_fmt_new = 'txw=%d txh=%d mode=%d sum=%ld row0=", xd->mi_row'
dir_fmt_old = 'txw=%d txh=%d sum=%ld row0=", mi_row'
dir_fmt_new = 'txw=%d txh=%d mode=%d sum=%ld row0=", mi_row'
upgraded = 0
arg_old = "col_off, txwpx, txhpx, ec_sum);"
arg_new = "col_off, txwpx, txhpx, mode, ec_sum);"
if nd_fmt_old in s:
    s = s.replace(nd_fmt_old, nd_fmt_new, 1)
    assert arg_old in s, "EC_PREDOUT8 non-directional argument list moved"
    s = s.replace(arg_old, arg_new, 1)
    upgraded += 1
if dir_fmt_old in s:
    s = s.replace(dir_fmt_old, dir_fmt_new, 1)
    assert arg_old in s, "EC_PREDOUT8 directional argument list moved"
    s = s.replace(arg_old, arg_new, 1)
    upgraded += 1
if upgraded:
    open(path, "w").write(s)
    print("EC_PREDOUT8 upgraded to mode= (%d site(s))" % upgraded)
else:
    print("EC_PREDOUT8 already carries mode= (no-op)")
PYRI

# --- rung 17: EC_PREDND, the hbd twin of EC_PREDOUT8 (lane-av1cmpaudit) ----
# The 10/12-bit non-directional twin of rung 16, and like it this rung lived
# ONLY in the hand-patched oracle tree. Without it a rebuild drops the only
# prediction probe that fires on a high-bitdepth stream -- rung 16 and the
# pre-existing EC_PREDOUT both sit after the `is_hbd` early return and print
# nothing there. EC_PREDND already carried `mode=`, so unlike rung 16 this one
# is an INSTALL-ONLY rung: it is inserted into a pristine upstream file and is
# a no-op on the already-instrumented tree.
I="$SRC/av1/common/reconintra.c"
[ -f "$I" ] || { echo "no oracle source at $I" >&2; exit 1; }

python3 - "$I" <<'PYND'
import sys
path = sys.argv[1]
s = open(path).read()
if "EC_PREDND" in s:
    print("EC_PREDND already instrumented (no-op)")
    sys.exit(0)

old = """      highbd_build_non_directional_intra_predictors(
          ref, ref_stride, dst, dst_stride, mode, tx_size, n_top_px, n_left_px,
          xd->bd);
      return;
    }"""
new = """      highbd_build_non_directional_intra_predictors(
          ref, ref_stride, dst, dst_stride, mode, tx_size, n_top_px, n_left_px,
          xd->bd);
      /* EC_INSTRUMENTED_PREDND (non-directional, high bitdepth) */
      if (getenv("EC_PRED")) {
        const int ec_mi_row = -xd->mb_to_top_edge >> (3 + MI_SIZE_LOG2);
        const int ec_mi_col = -xd->mb_to_left_edge >> (3 + MI_SIZE_LOG2);
        const uint16_t *d16 = CONVERT_TO_SHORTPTR(dst);
        long sum = 0;
        for (int r = 0; r < txhpx; ++r)
          for (int c = 0; c < txwpx; ++c) sum += d16[r * dst_stride + c];
        fprintf(stderr,
                "EC_PREDND mi_row=%d mi_col=%d plane=%d row_off=%d col_off=%d "
                "txw=%d txh=%d mode=%d n_top=%d n_left=%d sum=%ld row0=",
                ec_mi_row, ec_mi_col, plane, row_off, col_off, txwpx, txhpx,
                mode, n_top_px, n_left_px, sum);
        for (int c = 0; c < txwpx && c < 8; ++c) fprintf(stderr, "%d,", d16[c]);
        fprintf(stderr, " col0=");
        for (int r = 0; r < txhpx && r < 8; ++r)
          fprintf(stderr, "%d,", d16[r * dst_stride]);
        fprintf(stderr, "\\n");
      }
      return;
    }"""
assert old in s, "highbd non-directional intra predictor tail moved"
s = s.replace(old, new, 1)
open(path, "w").write(s)
print("EC_PREDND instrumented (non-directional, high bitdepth)")
PYND
