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
