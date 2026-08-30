/* lane-intrarect r1: standalone C reimplementation of libaom's rect intra
 * predictors, transcribed verbatim from the source read this round
 * (aom_dsp/intrapred.c's dc_predictor_rect/smooth_predictor/paeth_predictor
 * /v_predictor/h_predictor, av1/common/reconintra.c's
 * av1_dr_prediction_z1/z2/z3_c, intra_edge_filter_strength,
 * av1_use_intra_edge_upsample, av1_filter_intra_edge_c,
 * av1_upsample_intra_edge_c). Compiled and run independently of the Rust
 * port -- see lanes/wedge_dump.c for the pattern this copies and the
 * charter's shared-oracle-blindness note on why an independent
 * transcription is the check, not the table data.
 *
 * Every predictor here takes `above`/`left` pointers offset so index -1 is
 * the corner, exactly like the real decoder passes them (`above_row`/
 * `left_col` in reconintra.c) -- the harness's caller fills bw+bh worth of
 * real samples (no repeat-extension case exercised; that logic is untouched
 * square-path code already proven by the 223/223 lib suite).
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef uint8_t u8;

static inline int round2(int value, int shift) {
  return (value + (1 << (shift - 1))) >> shift;
}
static inline int clip_pixel(int v) { return v < 0 ? 0 : v > 255 ? 255 : v; }

static const u8 smooth_weights[] = {
  255, 149, 85, 64,
  255, 197, 146, 105, 73, 50, 37, 32,
  255, 225, 196, 170, 145, 123, 102, 84, 68, 54, 43, 33, 26, 20, 17, 16,
  255, 240, 225, 210, 196, 182, 169, 157, 145, 133, 122, 111, 101, 92, 83, 74,
  66, 59, 52, 45, 39, 34, 29, 25, 21, 17, 14, 12, 10, 9, 8, 8,
  255, 248, 240, 233, 225, 218, 210, 203, 196, 189, 182, 176, 169, 163, 156,
  150, 144, 138, 133, 127, 121, 116, 111, 106, 101, 96, 91, 86, 82, 77, 73, 69,
  65, 61, 57, 54, 50, 47, 44, 41, 38, 35, 32, 29, 27, 25, 22, 20, 18, 16, 15,
  13, 12, 10, 9, 8, 7, 6, 6, 5, 5, 4, 4, 4
};

/* dc_predictor_rect, aom_dsp/intrapred.c: bw != bh always goes through this
 * multiply-shift approximate divide (never the exact (sum+half)/count a
 * square block uses). */
static int dc_predictor_rect(int bw, int bh, const u8 *above, const u8 *left) {
  int sum = 0;
  for (int i = 0; i < bw; i++) sum += above[i];
  for (int i = 0; i < bh; i++) sum += left[i];
  int d = bw + bh, shift1 = 0;
  while ((d & 1) == 0) { d >>= 1; shift1++; }
  int multiplier = d == 3 ? 0x5556 : 0x3334;
  int interm = (sum + ((bw + bh) >> 1)) >> shift1;
  return (interm * multiplier) >> 16;
}

static void smooth_predictor(u8 *dst, int bw, int bh, const u8 *above, const u8 *left) {
  const u8 below_pred = left[bh - 1];
  const u8 right_pred = above[bw - 1];
  const u8 *sm_w = smooth_weights + bw - 4;
  const u8 *sm_h = smooth_weights + bh - 4;
  for (int r = 0; r < bh; r++) {
    for (int c = 0; c < bw; c++) {
      unsigned this_pred = (unsigned)sm_h[r] * above[c] + (unsigned)(256 - sm_h[r]) * below_pred +
                            (unsigned)sm_w[c] * left[r] + (unsigned)(256 - sm_w[c]) * right_pred;
      dst[r * bw + c] = (u8)round2((int)this_pred, 9);
    }
  }
}

static void paeth_predictor(u8 *dst, int bw, int bh, const u8 *above, const u8 *left) {
  const int tl = above[-1];
  for (int r = 0; r < bh; r++) {
    for (int c = 0; c < bw; c++) {
      int base = above[c] + left[r] - tl;
      int dl = base - left[r]; dl = dl < 0 ? -dl : dl;
      int da = base - above[c]; da = da < 0 ? -da : da;
      int dc = base - tl; dc = dc < 0 ? -dc : dc;
      dst[r * bw + c] = (u8)(dl <= da && dl <= dc ? left[r] : da <= dc ? above[c] : tl);
    }
  }
}

/* intra_edge_filter_strength (reconintra.c) -- includes the blk_wh<=12
 * branch that has no square-side equivalent (bw==bh's sum is always even
 * and >= 8, never lands in 9..=12), which is why the pre-lane Rust port
 * never needed it. */
static int intra_edge_filter_strength(int bs0, int bs1, int delta, int type) {
  int d = delta < 0 ? -delta : delta;
  int blk_wh = bs0 + bs1;
  int strength = 0;
  if (!type) {
    if (blk_wh <= 8) { if (d >= 56) strength = 1; }
    else if (blk_wh <= 12) { if (d >= 40) strength = 1; }
    else if (blk_wh <= 16) { if (d >= 40) strength = 1; }
    else if (blk_wh <= 24) { if (d >= 8) strength = 1; if (d >= 16) strength = 2; if (d >= 32) strength = 3; }
    else if (blk_wh <= 32) { if (d >= 1) strength = 1; if (d >= 4) strength = 2; if (d >= 32) strength = 3; }
    else { if (d >= 1) strength = 3; }
  } else {
    if (blk_wh <= 8) { if (d >= 40) strength = 1; if (d >= 64) strength = 2; }
    else if (blk_wh <= 16) { if (d >= 20) strength = 1; if (d >= 48) strength = 2; }
    else if (blk_wh <= 24) { if (d >= 4) strength = 3; }
    else { if (d >= 1) strength = 3; }
  }
  return strength;
}

static int use_intra_edge_upsample(int bs0, int bs1, int delta, int type) {
  int d = delta < 0 ? -delta : delta;
  int blk_wh = bs0 + bs1;
  if (d == 0 || d >= 40) return 0;
  return type ? blk_wh <= 8 : blk_wh <= 16;
}

static void filter_intra_edge(u8 *p, int sz, int strength) {
  if (!strength) return;
  static const int kernel[3][5] = { { 0, 4, 8, 4, 0 }, { 0, 5, 6, 5, 0 }, { 2, 4, 4, 4, 2 } };
  const int filt = strength - 1;
  u8 edge[300];
  memcpy(edge, p, sz);
  for (int i = 1; i < sz; i++) {
    int s = 0;
    for (int j = 0; j < 5; j++) {
      int k = i - 2 + j;
      k = k < 0 ? 0 : k > sz - 1 ? sz - 1 : k;
      s += edge[k] * kernel[filt][j];
    }
    p[i] = (u8)((s + 8) >> 4);
  }
}

static void filter_intra_edge_corner(u8 *above, u8 *left) {
  int s = left[0] * 5 + above[-1] * 6 + above[0] * 5;
  s = (s + 8) >> 4;
  above[-1] = (u8)s;
  left[-1] = (u8)s;
}

/* av1_upsample_intra_edge_c -- p[-1..sz-1] in, p[-2..2*sz-2] out. */
static void upsample_intra_edge(u8 *p, int sz) {
  u8 in[300];
  in[0] = p[-1];
  in[1] = p[-1];
  for (int i = 0; i < sz; i++) in[i + 2] = p[i];
  in[sz + 2] = p[sz - 1];
  p[-2] = in[0];
  for (int i = 0; i < sz; i++) {
    int s = -in[i] + 9 * in[i + 1] + 9 * in[i + 2] - in[i + 3];
    s = clip_pixel((s + 8) >> 4);
    p[2 * i - 1] = (u8)s;
    p[2 * i] = in[i + 2];
  }
}

/* dr_intra_derivative (spec 9.3). */
static int dr_intra_derivative(int angle) {
  static const struct { int a, v; } table[] = {
    {3,1023},{6,547},{9,372},{14,273},{17,215},{20,178},{23,151},{26,132},
    {29,116},{32,102},{36,90},{39,80},{42,71},{45,64},{48,57},{51,51},
    {54,45},{58,40},{61,35},{64,31},{67,27},{70,23},{73,19},{76,15},
    {81,11},{84,7},{87,3},
  };
  for (size_t i = 0; i < sizeof(table) / sizeof(table[0]); i++)
    if (table[i].a == angle) return table[i].v;
  fprintf(stderr, "no derivative for %d\n", angle);
  return 0;
}

/* The whole directional path (reconintra.c's edge filter/upsample steps
 * plus av1_dr_prediction_z1/z2/z3_c), rect reach `bw + bh` throughout,
 * cross-axis n_px extensions (above gets `bh`, left gets `bw`) exactly as
 * `build_directional_and_filter_intra_predictors` computes them. */
static void directional(u8 *dst, int bw, int bh, int angle, int enable_edge_filter,
                        int smooth_neighbor, int n_top, int n_left,
                        u8 *above_buf, u8 *left_buf) {
  /* above_buf/left_buf point at index -1 (corner); valid out to bw+bh-1. */
  u8 *above = above_buf;
  u8 *left = left_buf;
  int reach = bw + bh;
  int need_above = angle < 180, need_left = angle > 90;
  int need_right = angle < 90, need_bottom = angle > 180;

  if (enable_edge_filter && angle != 90 && angle != 180) {
    if (need_above && need_left && reach >= 24) filter_intra_edge_corner(above, left);
    if (need_above && n_top > 0) {
      int strength = intra_edge_filter_strength(bw, bh, angle - 90, smooth_neighbor);
      int n_px = n_top + 1 + (need_right ? bh : 0);
      filter_intra_edge(above - 1, n_px, strength);
    }
    if (need_left && n_left > 0) {
      int strength = intra_edge_filter_strength(bh, bw, angle - 180, smooth_neighbor);
      int n_px = n_left + 1 + (need_bottom ? bw : 0);
      filter_intra_edge(left - 1, n_px, strength);
    }
  }
  int upsample_above = enable_edge_filter && use_intra_edge_upsample(bw, bh, angle - 90, smooth_neighbor);
  int upsample_left = enable_edge_filter && use_intra_edge_upsample(bh, bw, angle - 180, smooth_neighbor);
  if (need_above && upsample_above) upsample_intra_edge(above, bw + (need_right ? bh : 0));
  if (need_left && upsample_left) upsample_intra_edge(left, bh + (need_bottom ? bw : 0));

  int dx = angle < 90 ? dr_intra_derivative(angle) : angle > 90 && angle < 180 ? dr_intra_derivative(180 - angle) : 0;
  int dy = angle > 180 ? dr_intra_derivative(270 - angle) : angle > 90 && angle < 180 ? dr_intra_derivative(angle - 90) : 0;

  if (angle < 90) {
    int max_base_x = (reach - 1) << upsample_above;
    int frac_bits = 6 - upsample_above;
    int base_inc = 1 << upsample_above;
    int x = dx;
    for (int r = 0; r < bh; r++, x += dx) {
      int base = x >> frac_bits;
      int shift = ((x << upsample_above) & 0x3F) >> 1;
      for (int c = 0; c < bw; c++, base += base_inc) {
        if (base < max_base_x) {
          int val = above[base] * (32 - shift) + above[base + 1] * shift;
          dst[r * bw + c] = (u8)round2(val, 5);
        } else {
          dst[r * bw + c] = above[max_base_x];
        }
      }
    }
  } else if (angle > 180) {
    int max_base_y = (reach - 1) << upsample_left;
    int frac_bits = 6 - upsample_left;
    int base_inc = 1 << upsample_left;
    int y = dy;
    for (int c = 0; c < bw; c++, y += dy) {
      int base = y >> frac_bits;
      int shift = ((y << upsample_left) & 0x3F) >> 1;
      for (int r = 0; r < bh; r++, base += base_inc) {
        if (base < max_base_y) {
          int val = left[base] * (32 - shift) + left[base + 1] * shift;
          dst[r * bw + c] = (u8)round2(val, 5);
        } else {
          dst[r * bw + c] = left[max_base_y];
        }
      }
    }
  } else {
    int min_base_x = -(1 << upsample_above);
    int frac_bits_x = 6 - upsample_above;
    int frac_bits_y = 6 - upsample_left;
    for (int r = 0; r < bh; r++) {
      for (int c = 0; c < bw; c++) {
        int val;
        int y = r + 1;
        int x = (c << 6) - y * dx;
        int base_x = x >> frac_bits_x;
        if (base_x >= min_base_x) {
          int shift = ((x * (1 << upsample_above)) & 0x3F) >> 1;
          val = above[base_x] * (32 - shift) + above[base_x + 1] * shift;
          val = round2(val, 5);
        } else {
          int x2 = c + 1;
          int y2 = (r << 6) - x2 * dy;
          int base_y = y2 >> frac_bits_y;
          int shift = ((y2 * (1 << upsample_left)) & 0x3F) >> 1;
          val = left[base_y] * (32 - shift) + left[base_y + 1] * shift;
          val = round2(val, 5);
        }
        dst[r * bw + c] = (u8)val;
      }
    }
  }
}

static uint64_t checksum(const u8 *dst, int bw, int bh) {
  uint64_t sum = 0;
  for (int i = 0; i < bh; i++)
    for (int j = 0; j < bw; j++) sum += (uint64_t)dst[i * bw + j] * (uint64_t)(i * bw + j + 1);
  return sum;
}

/* Deterministic synthetic neighbours: above[i] and left[i] both a simple
 * ramp-with-wobble so the checksum is sensitive to every index used. */
static void fill(u8 *buf, int n, int seed) {
  for (int i = 0; i < n; i++) buf[i] = (u8)((i * 7 + seed * 13 + (i % 5) * 3) % 256);
}

int main(void) {
  int shapes[][2] = { {8,4},{4,8},{16,8},{8,16},{4,16},{16,4},{32,16},{16,32} };
  /* mode tags: 0=DC 1=V 2=H 3=SMOOTH 4=SMOOTH_V 5=SMOOTH_H 6=PAETH,
   * 7..=13 directional angles handled separately below. */
  for (size_t s = 0; s < sizeof(shapes) / sizeof(shapes[0]); s++) {
    int bw = shapes[s][0], bh = shapes[s][1];
    int reach = bw + bh;
    u8 above_buf[300], left_buf[300];
    fill(above_buf + 1, reach, 1);
    fill(left_buf + 1, reach, 2);
    u8 corner = (u8)((bw * 3 + bh * 5) % 256);
    above_buf[0] = corner;
    left_buf[0] = corner;
    u8 *above = above_buf + 1, *left = left_buf + 1;
    u8 dst[512];

    int dc = dc_predictor_rect(bw, bh, above, left);
    printf("%dx%d DC value=%d\n", bw, bh, dc);

    smooth_predictor(dst, bw, bh, above, left);
    printf("%dx%d SMOOTH checksum=%llu\n", bw, bh, (unsigned long long)checksum(dst, bw, bh));

    paeth_predictor(dst, bw, bh, above, left);
    printf("%dx%d PAETH checksum=%llu\n", bw, bh, (unsigned long long)checksum(dst, bw, bh));

    /* directional: a handful of angles across all three zones, with and
     * without the edge filter, both smooth_neighbor states. */
    /* Only angles actually reachable as Mode_To_Angle[mode] + angle_delta *
     * ANGLE_STEP (delta in -3..=3, step 3) for some directional mode --
     * dr_intra_derivative's table is sparse and panics outside this set. */
    int angles[] = { 45, 48, 64, 67, 84, 87, 113, 116, 135, 138, 157, 160,
                      171, 183, 186, 203, 206 };
    for (size_t a = 0; a < sizeof(angles) / sizeof(angles[0]); a++) {
      for (int ef = 0; ef <= 1; ef++) {
        for (int sn = 0; sn <= 1; sn++) {
          u8 ab[300], le[300];
          memcpy(ab, above_buf, sizeof(above_buf));
          memcpy(le, left_buf, sizeof(left_buf));
          directional(dst, bw, bh, angles[a], ef, sn, bw, bh, ab + 1, le + 1);
          printf("%dx%d DR angle=%d ef=%d sn=%d checksum=%llu\n", bw, bh, angles[a], ef, sn,
                 (unsigned long long)checksum(dst, bw, bh));
        }
      }
    }
  }
  return 0;
}
