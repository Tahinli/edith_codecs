#include <stdio.h>
#include <stdint.h>
#include <string.h>

#define RS_SUBPEL_BITS 6
#define RS_SCALE_SUBPEL_BITS 14
#define RS_SCALE_SUBPEL_MASK ((1 << RS_SCALE_SUBPEL_BITS) - 1)
#define RS_SCALE_EXTRA_BITS (RS_SCALE_SUBPEL_BITS - RS_SUBPEL_BITS)
#define RS_SCALE_EXTRA_OFF (1 << (RS_SCALE_EXTRA_BITS - 1))
#define UPSCALE_NORMATIVE_TAPS 8

extern const int16_t av1_resize_filter_normative[1 << RS_SUBPEL_BITS][UPSCALE_NORMATIVE_TAPS];
extern int32_t av1_get_upscale_convolve_step(int in_length, int out_length);
extern void av1_convolve_horiz_rs_c(const uint8_t *src, int src_stride,
                                     uint8_t *dst, int dst_stride, int w,
                                     int h, const int16_t *x_filters,
                                     int x0_qn, int x_step_qn);

// verbatim copy of resize.c's static get_upscale_convolve_x0 -- the one
// formula in the pipeline this harness does not exercise via a real
// exported symbol; everything else (the coefficient table + the
// convolution/rounding kernel, the actual bug risk) runs real compiled
// libaom code.
static int32_t get_upscale_convolve_x0(int in_length, int out_length,
                                        int32_t x_step_qn) {
  const int err = out_length * x_step_qn - (in_length << RS_SCALE_SUBPEL_BITS);
  const int32_t x0 =
      (-((out_length - in_length) << (RS_SCALE_SUBPEL_BITS - 1)) +
       out_length / 2) /
          out_length +
      RS_SCALE_EXTRA_OFF - err / 2;
  return (int32_t)((uint32_t)x0 & RS_SCALE_SUBPEL_MASK);
}

int main(void) {
  const int in_len = 8, out_len = 16;
  uint8_t input[8] = { 10, 20, 30, 40, 50, 60, 70, 80 };
  const int border = UPSCALE_NORMATIVE_TAPS / 2 + 1; // 5
  uint8_t padded[8 + 2 * 16];
  uint8_t *row = padded + 16; // row[0..7] == input
  memset(padded, input[0], 16);
  memcpy(row, input, 8);
  memset(row + 8, input[7], 16);

  int32_t x_step_qn = av1_get_upscale_convolve_step(in_len, out_len);
  int32_t x0_qn = get_upscale_convolve_x0(in_len, out_len, x_step_qn);
  printf("x_step_qn=%d x0_qn=%d\n", x_step_qn, x0_qn);

  uint8_t out[12];
  av1_convolve_horiz_rs_c(row - 1, 0, out, 0, out_len, 1,
                           &av1_resize_filter_normative[0][0], x0_qn,
                           x_step_qn);
  printf("out:");
  for (int i = 0; i < out_len; i++) printf(" %d", out[i]);
  printf("\n");
  (void)border;
  return 0;
}
