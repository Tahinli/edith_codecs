#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>

extern int av1_apply_selfguided_restoration_c(
    const uint8_t *dat8, int width, int height, int stride, int eps,
    const int *xqd, uint8_t *dst8, int dst_stride, int32_t *tmpbuf,
    int bit_depth, int highbd);
extern int av1_selfguided_restoration_c(const uint8_t *dgd8, int width,
                                         int height, int dgd_stride,
                                         int32_t *flt0, int32_t *flt1,
                                         int flt_stride, int sgr_params_idx,
                                         int bit_depth, int highbd);
// Exported by scripts/instrument-aom-oracle.sh's rung 6 (normally `static`
// in restoration.c) so this harness can get real per-tap A[]/B[] instead of
// only the combined flt0/flt1 -- the missing half of r6's 9-tap cross-check.
extern void calculate_intermediate_result(int32_t *dgd, int width, int height,
                                          int dgd_stride, int bit_depth,
                                          int sgr_params_idx, int radius_idx,
                                          int pass, int32_t *A, int32_t *B);

#define SGRPROJ_BORDER_VERT 3
#define SGRPROJ_BORDER_HORZ 3

int main(void) {
  const int bw = 102, bh = 10;
  // real libaom splits stripe_width into RESTORATION_PROC_UNIT_SIZE=64
  // chunks (sgrproj_filter_stripe's `for j += procunit_width` loop) --
  // av1_apply_selfguided_restoration_c's internal fixed-size arrays are
  // sized for that max, so replicate the chunking (target col=6 is in
  // the first 64-wide chunk).
  const int w = 64, h = 4;
  uint8_t *buf = malloc(bw * bh);
  FILE *f = fopen("/tmp/lr_full.bin", "rb");
  if (!f) { perror("open"); return 1; }
  if (fread(buf, 1, bw * bh, f) != (size_t)(bw * bh)) { fprintf(stderr, "short read\n"); return 1; }
  fclose(f);

  const uint8_t *dgd8 = buf + 3 * bw + 3; // logical (0,0)
  int xqd[2] = { -16, -32 };
  uint8_t dst[w * h];
  // flt1 = tmpbuf + RESTORATION_UNITPELS_MAX (~152100); undersizing this
  // segfaults inside av1_apply_selfguided_restoration_c.
  static int32_t tmpbuf[2 * 400 * 400];

  int ret = av1_apply_selfguided_restoration_c(dgd8, w, h, bw, /*eps=*/6, xqd,
                                                 dst, w, tmpbuf, 8, 0);
  printf("ret=%d\n", ret);
  // target pixel: row i=1, col j=6
  printf("out[1][6] = %d\n", dst[1 * w + 6]);
  printf("dgd[1][6] = %d\n", dgd8[1 * bw + 6]);

  static int32_t flt0[400 * 400], flt1[400 * 400];
  int r2 = av1_selfguided_restoration_c(dgd8, w, h, bw, flt0, flt1, w, 6, 8, 0);
  printf("selfguided ret=%d flt0[1][6]=%d flt1[1][6]=%d\n", r2, flt0[1 * w + 6],
         flt1[1 * w + 6]);

  // --- rung 6: real per-tap A[]/B[] ground truth for the dense (r1) arm ---
  // Replicate av1_selfguided_restoration_c's own uint8->int32 border-extend
  // copy (that function's `dgd32` setup, restoration.c) so
  // calculate_intermediate_result sees exactly the same input it would from
  // the real call path above.
  const int dgd32_stride = w + 2 * SGRPROJ_BORDER_HORZ;
  static int32_t dgd32_buf[(64 + 6) * (4 + 6)];
  int32_t *dgd32 = dgd32_buf + dgd32_stride * SGRPROJ_BORDER_VERT + SGRPROJ_BORDER_HORZ;
  for (int i = -SGRPROJ_BORDER_VERT; i < h + SGRPROJ_BORDER_VERT; ++i)
    for (int j = -SGRPROJ_BORDER_HORZ; j < w + SGRPROJ_BORDER_HORZ; ++j)
      dgd32[i * dgd32_stride + j] = dgd8[i * bw + j];

  const int width_ext = w + 2 * SGRPROJ_BORDER_HORZ;
  const int buf_stride = ((width_ext + 3) & ~3) + 16;
  static int32_t A_[2 * 400 * 400], B_[2 * 400 * 400];
  // radius_idx=1 (dense/r1 arm), pass=0 (every row, matches
  // selfguided_restoration_internal's own call).
  calculate_intermediate_result(dgd32, w, h, dgd32_stride, /*bit_depth=*/8,
                                /*sgr_params_idx=*/6, /*radius_idx=*/1,
                                /*pass=*/0, A_, B_);
  int32_t *A = A_ + SGRPROJ_BORDER_VERT * buf_stride + SGRPROJ_BORDER_HORZ;
  int32_t *B = B_ + SGRPROJ_BORDER_VERT * buf_stride + SGRPROJ_BORDER_HORZ;
  const int ti = 1, tj = 6;
  const char *names[9] = { "c", "u", "d", "l", "r", "ul", "ur", "dl", "dr" };
  const int di[9] = { 0, -1, 1, 0, 0, -1, -1, 1, 1 };
  const int dj[9] = { 0, 0, 0, -1, 1, -1, 1, -1, 1 };
  for (int t = 0; t < 9; ++t) {
    int k = (ti + di[t]) * buf_stride + (tj + dj[t]);
    printf("real_ab %s = (%d,%d)\n", names[t], A[k], B[k]);
  }
  return 0;
}
