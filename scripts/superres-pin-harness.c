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

static void run_case_margin(const char *label, int in_len, int out_len,
                             const uint8_t *input, const uint8_t *margin,
                             int margin_len) {
  uint8_t padded[512];
  const int pad = 32;
  uint8_t *row = padded + pad;
  memset(padded, input[0], pad);
  memcpy(row, input, in_len);
  memcpy(row + in_len, margin, margin_len);
  memset(row + in_len + margin_len, margin[margin_len - 1], pad);

  int32_t x_step_qn = av1_get_upscale_convolve_step(in_len, out_len);
  int32_t x0_qn = get_upscale_convolve_x0(in_len, out_len, x_step_qn);
  printf("%s: x_step_qn=%d x0_qn=%d\n", label, x_step_qn, x0_qn);
  uint8_t out[256];
  av1_convolve_horiz_rs_c(row - 1, 0, out, 0, out_len, 1,
                           &av1_resize_filter_normative[0][0], x0_qn,
                           x_step_qn);
  printf("%s out:", label);
  for (int i = 0; i < out_len; i++) printf(" %d", out[i]);
  printf("\n");
}

// Runs one row through the real compiled libaom convolver, edge-replicated
// padding on both sides (the single-tile case, matching this decoder's
// `superres::upscale_row`). `right_pad_col` lets the caller feed a
// right-edge pad value that differs from `input[in_len-1]`, to test
// r3's charter question (does the padding value change the output at
// all near the right edge, i.e. does the failing case even depend on
// what's beyond column `in_len-1`).
static void run_case(const char *label, int in_len, int out_len,
                      const uint8_t *input, int right_pad_val) {
  uint8_t padded[512];
  const int pad = 32;
  uint8_t *row = padded + pad;
  memset(padded, input[0], pad);
  memcpy(row, input, in_len);
  memset(row + in_len, right_pad_val, pad);

  int32_t x_step_qn = av1_get_upscale_convolve_step(in_len, out_len);
  int32_t x0_qn = get_upscale_convolve_x0(in_len, out_len, x_step_qn);
  printf("%s: in_len=%d out_len=%d x_step_qn=%d x0_qn=%d right_pad=%d\n",
         label, in_len, out_len, x_step_qn, x0_qn, right_pad_val);

  uint8_t out[256];
  av1_convolve_horiz_rs_c(row - 1, 0, out, 0, out_len, 1,
                           &av1_resize_filter_normative[0][0], x0_qn,
                           x_step_qn);
  printf("%s out:", label);
  for (int i = 0; i < out_len; i++) printf(" %d", out[i]);
  printf("\n");
}

int main(void) {
  uint8_t input8[8] = { 10, 20, 30, 40, 50, 60, 70, 80 };
  run_case("in8->out12", 8, 12, input8, input8[7]);
  run_case("in8->out16", 8, 16, input8, input8[7]);

  // r3: the exact failing ratio (43 -> 64), row 6 of the real aomenc
  // superres key-frame gate fixture (`EC_SUPERRES_DEBUG=1` dump of the
  // input row this decoder feeds `upscale_row`, matched against the
  // `DIFF frame=0 row=6 col=62 got=140 want=141` mismatch). Run once
  // with the replicate-from-column-42 padding this decoder uses, and
  // once with a right pad value that visibly differs, to settle whether
  // the failing columns even read past the real input at all.
  uint8_t row6[43] = { 102, 102, 103, 103, 104, 105, 106, 107, 108, 109,
                        110, 111, 112, 113, 114, 115, 116, 117, 118, 119,
                        120, 121, 122, 123, 125, 126, 127, 127, 128, 129,
                        130, 130, 131, 131, 133, 134, 136, 137, 138, 139,
                        140, 140, 141 };
  run_case("row6-replicate", 43, 64, row6, row6[42]);
  run_case("row6-farpad", 43, 64, row6, 255);

  // r3 decisive check: `CROPDBG` dumped the pre-crop reconstructed row at
  // the real (mi-aligned) width 48 -- columns 43..47 are genuine decoded
  // pixels (140,140,140,140,140), NOT a replicate of column 42 (141, this
  // decoder's current padding value). Feed the real trailing pixels
  // instead of a flat replicate and see if the failing columns change.
  uint8_t row6_real48[48] = { 102, 102, 103, 103, 104, 105, 106, 107, 108, 109,
                               110, 111, 112, 113, 114, 115, 116, 117, 118, 119,
                               120, 121, 122, 123, 125, 126, 127, 127, 128, 129,
                               130, 130, 131, 131, 133, 134, 136, 137, 138, 139,
                               140, 140, 141, 140, 140, 140, 140, 140 };
  run_case("row6-realpad48", 48, 64, row6_real48, row6_real48[47]);
  // Same real trailing pixels but in_len still 43 (matches frame_width),
  // right_pad taken from the first real trailing pixel (140) rather than
  // the frame-edge pixel (141) -- the actual libaom border-extend source.
  run_case("row6-realedgeval", 43, 64, row6, 140);

  // r3 round 2: frame 2's rows 17/18 still mismatched by 1 at col 62 with
  // the flat-margin fix landed. Their real margin is NOT flat (a gradient
  // slope continues past the frame edge), so this checks the fix's
  // n-real-pixels-then-replicate strategy against the true 5-pixel margin.
  uint8_t row17[43] = { 109, 109, 109, 110, 111, 112, 112, 113, 114, 115,
                         116, 117, 118, 119, 120, 121, 122, 123, 124, 125,
                         126, 127, 128, 129, 130, 131, 131, 132, 133, 134,
                         135, 136, 137, 139, 139, 140, 140, 141, 142, 142,
                         143, 144, 145 };
  uint8_t margin17[5] = { 146, 146, 147, 147, 147 };
  run_case_margin("row17-margin", 43, 64, row17, margin17, 5);

  uint8_t row18[43] = { 109, 110, 110, 110, 111, 112, 113, 114, 115, 116,
                         117, 118, 119, 120, 121, 122, 122, 123, 124, 125,
                         126, 127, 128, 129, 130, 131, 132, 133, 133, 134,
                         134, 136, 138, 140, 139, 140, 140, 141, 142, 142,
                         143, 144, 145 };
  uint8_t margin18[5] = { 146, 147, 147, 148, 148 };
  run_case_margin("row18-margin", 43, 64, row18, margin18, 5);
  return 0;
}
