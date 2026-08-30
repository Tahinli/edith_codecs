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
  return 0;
}
