/* lane-recttx r1: pins the 14 rectangular inverse transform sizes against
 * the REAL libaom 1D kernels (av1_idct4/8/16/32/64, DCT_DCT only), linked
 * from libaom.a -- NOT the higher-level `av1_inv_txfm2d_add_WxH_c` facade,
 * which segfaults called standalone from a bare harness (its
 * `av1_get_inv_txfm_cfg` path resolves a NULL function pointer outside a
 * real decoder's init sequence; root cause not chased further -- turn
 * budget). Calling the plain per-axis kernels directly sidesteps that and
 * is still linked, not transcribed, code for the part that actually needs
 * checking here: the axis-swap / rect-scale / row-shift PLUMBING is this
 * harness's own C, ported line-for-line from `inv_txfm2d_add_c`
 * (av1_inv_txfm2d.c:234-315) -- see the charter's `reference-layout-not-spec`
 * note: libaom's row-pass input is COLUMN-major (`input[c * txfm_size_row +
 * r]`, av1_inv_txfm2d.c:275), our own `dequant` is ROW-major
 * (`dequant[i * w + j]`) -- this harness transposes at the boundary,
 * feeding the SAME logical coefficient grid the Rust side gets, so a
 * transposed-axis bug in the Rust port cannot be cancelled by an
 * equally-transposed harness.
 *
 * stage_range is fixed at 16 for every stage: `av1_gen_inv_stage_range`
 * (av1_inv_txfm2d.c:194-196) sets `opt_range_row = opt_range_col = 16` for
 * `bd == 8`, which is exactly what this lane's row_clamp/col_clamp already
 * are at bit_depth 8 (`bit_depth + 8 = 16`, `max(bit_depth + 6, 16) = 16`).
 *
 * Build: cc -O2 -I<aom-src> lanes/recttx_dump.c <aom-build>/libaom.a \
 *   -lm -lpthread -o /tmp/recttx_dump
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef int32_t tran_low_t;

extern void av1_idct4(const int32_t *input, int32_t *output, int8_t cos_bit, const int8_t *stage_range);
extern void av1_idct8(const int32_t *input, int32_t *output, int8_t cos_bit, const int8_t *stage_range);
extern void av1_idct16(const int32_t *input, int32_t *output, int8_t cos_bit, const int8_t *stage_range);
extern void av1_idct32(const int32_t *input, int32_t *output, int8_t cos_bit, const int8_t *stage_range);
extern void av1_idct64(const int32_t *input, int32_t *output, int8_t cos_bit, const int8_t *stage_range);

#define INV_COS_BIT 12
static const int8_t STAGE_RANGE[16] = { 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16 };

typedef void (*idct_fn)(const int32_t *, int32_t *, int8_t, const int8_t *);

static idct_fn idct_for(int n) {
  switch (n) {
    case 4: return av1_idct4;
    case 8: return av1_idct8;
    case 16: return av1_idct16;
    case 32: return av1_idct32;
    case 64: return av1_idct64;
  }
  return NULL;
}

static int round_shift(int64_t x, int bit) { return (int)((x + (1LL << (bit - 1))) >> bit); }

/* `Transform_Row_Shift` / `av1_inv_txfm_shift_ls` (av1_inv_txfm2d.c:132-158,
 * `shift[0]`): keyed on the full (w, h) TX size, identical table the Rust
 * side's `row_shift_wh` carries. */
static int row_shift_wh(int w, int h) {
  if (w == 4 && h == 4) return 0;
  if (w == 8 && h == 8) return 1;
  if ((w == 16 && h == 16) || (w == 32 && h == 32) || (w == 64 && h == 64)) return 2;
  if ((w == 4 && h == 8) || (w == 8 && h == 4)) return 0;
  if ((w == 8 && h == 16) || (w == 16 && h == 8)) return 1;
  if ((w == 16 && h == 32) || (w == 32 && h == 16)) return 1;
  if ((w == 32 && h == 64) || (w == 64 && h == 32)) return 1;
  if ((w == 4 && h == 16) || (w == 16 && h == 4)) return 1;
  if ((w == 8 && h == 32) || (w == 32 && h == 8)) return 2;
  if ((w == 16 && h == 64) || (w == 64 && h == 16)) return 2;
  fprintf(stderr, "no row shift for %dx%d\n", w, h);
  return -1;
}

/* Asymmetric per (row, col) coefficient in the coded corner (min(w,32) x
 * min(h,32)), zero elsewhere -- row weight (24) and col weight (17) differ
 * so a transposed-axis bug lands on a different value, not a
 * coincidentally equal one. */
static int32_t coeff(int i, int j) {
  if (i >= 32 || j >= 32) return 0;
  if (i == 0 && j == 0) return 640; /* DC */
  if (i < 4 && j < 4) return (i + 1) * 24 - (j + 1) * 17;
  return 0;
}

/* `inv_txfm2d_add_c`'s DCT_DCT path (av1_inv_txfm2d.c:234-315), transcribed
 * for (w, h) with `dequant` in ROW-major order as our Rust side keeps it --
 * libaom's own `input[c * txfm_size_row + r]` column-major addressing is
 * reproduced here (`col_major`), not copied into our indexing convention. */
static void run(const char *name, int w, int h) {
  tran_low_t row_major[64 * 64], col_major[64 * 64];
  for (int i = 0; i < h; i++)
    for (int j = 0; j < w; j++) row_major[i * w + j] = coeff(i, j);
  for (int c = 0; c < w; c++)
    for (int r = 0; r < h; r++) col_major[c * h + r] = row_major[r * w + c];

  int rect_type_abs1 = (__builtin_ctz(w) - __builtin_ctz(h) == 1) || (__builtin_ctz(h) - __builtin_ctz(w) == 1);
  int shift0 = row_shift_wh(w, h);
  idct_fn row_fn = idct_for(w), col_fn = idct_for(h);

  int32_t buf[64 * 64];
  int32_t temp_in[64], temp_out[64];
  for (int r = 0; r < h; r++) {
    for (int c = 0; c < w; c++) {
      int32_t v = col_major[c * h + r];
      temp_in[c] = rect_type_abs1 ? round_shift((int64_t)v * 2896, 12) : v;
    }
    row_fn(temp_in, buf + r * w, INV_COS_BIT, STAGE_RANGE);
    for (int c = 0; c < w; c++) buf[r * w + c] = round_shift(buf[r * w + c], shift0);
  }

  int32_t out[64 * 64];
  for (int c = 0; c < w; c++) {
    for (int r = 0; r < h; r++) temp_in[r] = buf[r * w + c];
    col_fn(temp_in, temp_out, INV_COS_BIT, STAGE_RANGE);
    for (int r = 0; r < h; r++) out[r * w + c] = round_shift(temp_out[r], 4);
  }

  int64_t checksum = 0;
  for (int r = 0; r < h; r++)
    for (int c = 0; c < w; c++) {
      int idx = r * w + c;
      checksum += (int64_t)out[idx] * (int64_t)(idx + 1);
    }
  (void)name;
  printf("%dx%d DCT_DCT checksum=%lld\n", w, h, (long long)checksum);
}

int main(void) {
  run("4x8", 4, 8);
  run("8x4", 8, 4);
  run("8x16", 8, 16);
  run("16x8", 16, 8);
  run("16x32", 16, 32);
  run("32x16", 32, 16);
  run("32x64", 32, 64);
  run("64x32", 64, 32);
  run("4x16", 4, 16);
  run("16x4", 16, 4);
  run("8x32", 8, 32);
  run("32x8", 32, 8);
  run("16x64", 16, 64);
  run("64x16", 64, 16);
  return 0;
}
