/* lane-wedge r3: standalone C reimplementation of libaom
 * av1/common/reconinter.c's wedge master-mask + per-block codebook
 * derivation (wedge_master_oblique_odd/even, wedge_master_vertical,
 * shift_copy, init_wedge_master_masks, get_wedge_mask_inplace,
 * init_wedge_masks), transcribed verbatim from the source read this round.
 * Compiled and run independently of the Rust port to catch translation
 * bugs (indexing, shift direction, sign) even though the table DATA is
 * shared with the source -- see charter's shared-oracle-blindness note.
 */
#include <stdio.h>
#include <stdint.h>
#include <string.h>

#define MASK_MASTER_SIZE 64
#define MASK_MASTER_STRIDE 64
#define WEDGE_WEIGHT_BITS 6
#define MAX_WEDGE_TYPES 16

enum { WEDGE_HORIZONTAL = 0, WEDGE_VERTICAL, WEDGE_OBLIQUE27, WEDGE_OBLIQUE63,
       WEDGE_OBLIQUE117, WEDGE_OBLIQUE153, WEDGE_DIRECTIONS };

static const uint8_t wedge_master_oblique_odd[MASK_MASTER_SIZE] = {
  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  1,  2,  6,  18,
  37, 53, 60, 63, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
  64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
};
static const uint8_t wedge_master_oblique_even[MASK_MASTER_SIZE] = {
  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  1,  4,  11, 27,
  46, 58, 62, 63, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
  64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
};
static const uint8_t wedge_master_vertical[MASK_MASTER_SIZE] = {
  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  2,  7,  21,
  43, 57, 62, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
  64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
};

static void shift_copy(const uint8_t *src, uint8_t *dst, int shift, int width) {
  if (shift >= 0) {
    memcpy(dst + shift, src, width - shift);
    memset(dst, src[0], shift);
  } else {
    shift = -shift;
    memcpy(dst, src + shift, width - shift);
    memset(dst + width - shift, src[width - 1], shift);
  }
}

static uint8_t wedge_mask_obl[2][WEDGE_DIRECTIONS][MASK_MASTER_SIZE * MASK_MASTER_SIZE];

static void init_wedge_master_masks(void) {
  int i, j;
  const int w = MASK_MASTER_SIZE, h = MASK_MASTER_SIZE, stride = MASK_MASTER_STRIDE;
  int shift = h / 4;
  for (i = 0; i < h; i += 2) {
    shift_copy(wedge_master_oblique_even, &wedge_mask_obl[0][WEDGE_OBLIQUE63][i * stride], shift, MASK_MASTER_SIZE);
    shift--;
    shift_copy(wedge_master_oblique_odd, &wedge_mask_obl[0][WEDGE_OBLIQUE63][(i + 1) * stride], shift, MASK_MASTER_SIZE);
    memcpy(&wedge_mask_obl[0][WEDGE_VERTICAL][i * stride], wedge_master_vertical, MASK_MASTER_SIZE);
    memcpy(&wedge_mask_obl[0][WEDGE_VERTICAL][(i + 1) * stride], wedge_master_vertical, MASK_MASTER_SIZE);
  }
  for (i = 0; i < h; ++i) {
    for (j = 0; j < w; ++j) {
      const int msk = wedge_mask_obl[0][WEDGE_OBLIQUE63][i * stride + j];
      wedge_mask_obl[0][WEDGE_OBLIQUE27][j * stride + i] = msk;
      wedge_mask_obl[0][WEDGE_OBLIQUE117][i * stride + w - 1 - j] =
          wedge_mask_obl[0][WEDGE_OBLIQUE153][(w - 1 - j) * stride + i] = (1 << WEDGE_WEIGHT_BITS) - msk;
      wedge_mask_obl[1][WEDGE_OBLIQUE63][i * stride + j] =
          wedge_mask_obl[1][WEDGE_OBLIQUE27][j * stride + i] = (1 << WEDGE_WEIGHT_BITS) - msk;
      wedge_mask_obl[1][WEDGE_OBLIQUE117][i * stride + w - 1 - j] =
          wedge_mask_obl[1][WEDGE_OBLIQUE153][(w - 1 - j) * stride + i] = msk;
      const int mskx = wedge_mask_obl[0][WEDGE_VERTICAL][i * stride + j];
      wedge_mask_obl[0][WEDGE_HORIZONTAL][j * stride + i] = mskx;
      wedge_mask_obl[1][WEDGE_VERTICAL][i * stride + j] =
          wedge_mask_obl[1][WEDGE_HORIZONTAL][j * stride + i] = (1 << WEDGE_WEIGHT_BITS) - mskx;
    }
  }
}

typedef struct { int direction, x_offset, y_offset; } wedge_code_type;

/* wedge_codebook_16_heqw -- the ONLY codebook this decoder's reachable
 * (square 8x8/16x16/32x32) leaves use. */
static const wedge_code_type heqw[16] = {
  { WEDGE_OBLIQUE27, 4, 4 },  { WEDGE_OBLIQUE63, 4, 4 },
  { WEDGE_OBLIQUE117, 4, 4 }, { WEDGE_OBLIQUE153, 4, 4 },
  { WEDGE_HORIZONTAL, 4, 2 }, { WEDGE_HORIZONTAL, 4, 6 },
  { WEDGE_VERTICAL, 2, 4 },   { WEDGE_VERTICAL, 6, 4 },
  { WEDGE_OBLIQUE27, 4, 2 },  { WEDGE_OBLIQUE27, 4, 6 },
  { WEDGE_OBLIQUE153, 4, 2 }, { WEDGE_OBLIQUE153, 4, 6 },
  { WEDGE_OBLIQUE63, 2, 4 },  { WEDGE_OBLIQUE63, 6, 4 },
  { WEDGE_OBLIQUE117, 2, 4 }, { WEDGE_OBLIQUE117, 6, 4 },
};
/* wedge_signflip_lookup rows for BLOCK_8X8/16X16/32X32 -- identical row. */
static const uint8_t signflip[16] = { 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1, 1, 0, 1 };

static const uint8_t *get_wedge_mask_inplace(int wedge_index, int neg, int bw, int bh) {
  const wedge_code_type *a = &heqw[wedge_index];
  const uint8_t wsignflip = signflip[wedge_index];
  int woff = (a->x_offset * bw) >> 3;
  int hoff = (a->y_offset * bh) >> 3;
  return wedge_mask_obl[neg ^ wsignflip][a->direction] +
         MASK_MASTER_STRIDE * (MASK_MASTER_SIZE / 2 - hoff) + MASK_MASTER_SIZE / 2 - woff;
}

static void dump_bsize(const char *name, int bw, int bh) {
  for (int sign = 0; sign < 2; ++sign) {
    for (int w = 0; w < MAX_WEDGE_TYPES; ++w) {
      const uint8_t *mask = get_wedge_mask_inplace(w, sign, bw, bh);
      uint64_t sum = 0;
      for (int i = 0; i < bh; ++i)
        for (int j = 0; j < bw; ++j)
          sum += (uint64_t)mask[i * MASK_MASTER_STRIDE + j] * (uint64_t)(i * bw + j + 1);
      printf("%s sign=%d idx=%d checksum=%llu\n", name, sign, w, (unsigned long long)sum);
    }
  }
}

int main(void) {
  init_wedge_master_masks();
  dump_bsize("8x8", 8, 8);
  dump_bsize("16x16", 16, 16);
  dump_bsize("32x32", 32, 32);
  return 0;
}
