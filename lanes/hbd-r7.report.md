# lane-hbd round 7 report

## Step 1: gate on d6bd69a (CDEF fix, unmeasured going in)

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib a_real_aomenc_10bit_stream_decodes_pixel_exact -- --nocapture`
(fixed the `-j4` flag placement -- it belongs before `--`, not after, as a
libtest arg; `cargo test -j4 -p ec-av1 --lib ... -- --nocapture`)

Diff count: **1 of 4096 luma pixels off by exactly 1** (down from round 6's
32). Chroma (U, V) exact. `left: [..., 328, ...] right: [..., 327, ...]`,
extracted via the python re.search method.

EVIDENCE: /tmp/gate_r7.log | cargo test -j4 -p ec-av1 --lib a_real_aomenc_10bit_stream_decodes_pixel_exact -- --nocapture | 1/4096 luma pixels differ by 1, chroma untested (Y assert panics first)

## Step 2: geometry

Added a temporary per-plane diff dump (`HBD_R7_YDIFF`/`UDIFF`/`VDIFF`, since
reverted) before the asserts to get all three planes' diffs without the
first `assert_eq!` short-circuiting. Result: `Y: [(x=1, y=34, ours=328,
theirs=327)]`, `U: []`, `V: []` -- a single luma pixel, near the left frame
edge (x=1), row 34 (inside CDEF 8x8 block row 4, i.e. rows 32-39), not on
an LR-unit or CDEF-block corner.

EVIDENCE: /tmp/gate_r7c.log (dump run) | cargo test -j4 -p ec-av1 --lib a_real_aomenc_10bit_stream_decodes_pixel_exact -- --nocapture | HBD_R7_YDIFF [(1, 34, 328, 327)], U/V empty

## Step 3: root cause -- checked against `~/.cache/aom-oracle/src/av1/common/cdef_block.c` and `cdef.h`

Ruled out, all verified line-for-line against libaom source (not rebuilt,
read only):
- `cdef.h:61` `constrain()` -- matches `cdef_constrain` exactly.
- `cdef_block.c:57-110` `cdef_find_dir_c` -- matches `cdef_find_dir` exactly
  (partial-sum accumulation, div_table, cost formula, `>>128` centring).
- `cdef_block.c:250-256` frame-boundary `CDEF_VERY_LARGE` fill -- matches
  our `ny<0||nx<0||...>=true_w/h` boundary check; this 64x64 fixture is
  exactly one superblock so every frame edge coincides with the SB edge,
  same as libaom's `frame_boundary[LEFT/RIGHT/TOP/BOTTOM]` gating.
- `cdef_block.c:135` `Cdef_Directions` table -- byte-for-byte identical to
  our `CDEF_DIRECTIONS`.
- `cdef_block.c:190` output rounding `(8 + sum - (sum<0)) >> 4` -- identical
  to ours.
- `cdef_block.c:289-292` `adjust_strength` -- identical; and `cdef_block.c:388`
  confirms it's luma-only (`pli ? pri_strength : adjust_strength(...)`,
  matching our chroma path skipping it).
- `cdef_block.c:352-360` direction reuse for 4:2:0 chroma (`xdec==ydec`, no
  `conv422`/`conv440` remap) -- matches our shared `dir`/`uv_dir` reuse.

**Root cause found**: `cdef_block.c:147`
`pri_taps = cdef_pri_taps[(pri_strength >> coeff_shift) & 1]` -- libaom
right-shifts `pri_strength` by `coeff_shift` *before* reading the tap-set
parity bit, because `pri_strength` (and its `adjust_strength` derivative)
was pre-shifted `<< coeff_shift` for the bit-depth scale up in
`av1_cdef_filter_fb`. Our `cdef_filter_block` read `pri_strength & 1`
directly. For 8-bit (`coeff_shift == 0`) this is a no-op and every existing
8-bit gate stayed exact by construction. For 10-bit (`coeff_shift == 2`),
`pri_strength` is always a multiple of 4, so `pri_strength & 1` was always
0 -- CDEF silently always used primary tap set `{4,2}` instead of `{3,3}`
whenever the true (unshifted) strength level was odd. This only produces a
visible pixel difference where the wrong tap set actually changes the
clamped/rounded output, which explains the very sparse (1/4096) residue.

Fix: threaded `coeff_shift` into `cdef_filter_block` and changed the index
to `(pri_strength >> coeff_shift) & 1`, matching libaom exactly. Both call
sites (luma, chroma) updated. Commit `7e007da`.

## Step 1 re-run after the fix

`cargo test -j4 -p ec-av1 --lib a_real_aomenc_10bit_stream_decodes_pixel_exact -- --nocapture`
-> **0 diffs, all three planes** (`HBD_R7_YDIFF []`, `UDIFF []`, `VDIFF []`),
`test result: ok`.

EVIDENCE: /tmp/gate_r7c.log | cargo test -j4 -p ec-av1 --lib a_real_aomenc_10bit_stream_decodes_pixel_exact -- --nocapture | test result: ok. 1 passed; 0 failed

Temporary diff-dump instrumentation reverted from `stream.rs` before commit
(the gate's own three `assert_eq!`s are the permanent check).

## Step 3: full suite

(filled in below once run)

## Step 4: workspace check

(filled in below once run)

## Step 5: decode_probe on real films

(filled in below once run)
