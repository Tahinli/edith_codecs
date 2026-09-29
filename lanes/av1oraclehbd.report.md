# lane-av1oraclehbd — the four u8-narrowing oracle rungs, root cause and fix

Branch `lane-av1oraclehbd`, branched from `3eec02c8` ("lanes: merge-wave-3a
report"). Worktree `~/.cache/wt/av1oraclehbd`. Private oracle copy
`~/.cache/aom-oracle3` (same source revision as the shared oracle,
`9bb526a8fe938500d0392c93f20e30bf7bdda0a9` = `v3.13.3-7-g9bb526a`, copied with
`cp -a` from `~/.cache/aom-oracle/src` and configured/built in place by
`scripts/build-aom-oracle.sh AOM_ORACLE_ROOT=~/.cache/aom-oracle3`). The
shared oracle, `~/.cache/aom-oracle2`, and every gate were not touched. The
primary checkout was never edited (`git status --porcelain` there is empty).

Diff: `scripts/instrument-aom-oracle.sh` only, +93 lines, one new trailing
block. No C file in the repo, no gate, no decoder change.

## 1. Root cause — it is not the element size, it is a HALVED pointer

The charter's hypothesis was "`fwrite(..., 1, w, f)` on a u16 plane reads past
the buffer". That is a real second defect, but it is not what segfaults, and
fixing only it would leave every rung emitting the wrong row. The faulting
mistake is in the **address**, and it is libaom's own pointer encoding:

`aom_ports/mem.h:75-76`

```c
#define CONVERT_TO_SHORTPTR(x) ((uint16_t *)(((uintptr_t)(x)) << 1))
#define CONVERT_TO_BYTEPTR(x)   ((uint8_t *)(((uintptr_t)(x)) >> 1))
```

`YV12_BUFFER_CONFIG` stores plane pointers in **halved** form at high bit
depth. `aom_scale/generic/yv12config.c:161-180`:

```c
    buf = ybf->buffer_alloc;
    if (use_highbitdepth) {
      buf = CONVERT_TO_BYTEPTR(ybf->buffer_alloc);   /* == buffer_alloc >> 1 */
      ybf->flags = YV12_FLAG_HIGHBITDEPTH;
    }
    ybf->y_buffer = (uint8_t *)aom_align_addr(
        buf + (border * y_stride) + border, aom_byte_align);
```

so `ybf->y_buffer` is `real_address >> 1`, and the real byte address is
`CONVERT_TO_SHORTPTR(y_buffer)` (`y_buffer << 1`). The four rungs did

```c
fwrite(ec_b->y_buffer + ec_r * ec_b->y_stride, 1, ec_b->y_crop_width, ec_f);
```

i.e. pointer arithmetic on the *halved* value. Two errors compound:

1. **Wrong address.** `y_buffer` itself is half the real address, so the very
   first `fwrite` reads an unrelated, unmapped address. Measured on
   `av112bit-key.obu`: `y_buffer = 0x3ffffbbb7850` while the real plane is
   `0x7ffff776f0a0` (`y_buffer << 1`, and `0x7ffff776f0a0 - 0x7ffff7766020 =
   36992 = 2 * (border*y_stride + border) = 2 * (64*288 + 64)`, which is the
   allocation base plus the border offset in BYTES — the halving is confirmed
   arithmetically, not inferred). `write(2)` to `/dev/null` "succeeds" on that
   address, which is why a naive probe lies; a `mincore()` and a real load both
   report it unmapped.
2. **Wrong row.** `y_stride` / `uv_stride` count SAMPLES (`288` = `160 + 2*64`
   border, `144` = `288 >> ss_x`), so `+ r*stride` in byte units also addresses
   the wrong row even where the address happens to be readable.

At 8 bit `use_highbitdepth` is 0, `buf` is the real pointer and no halving
happens — which is exactly why the same line is correct on an 8-bit stream and
faults on every 10/12-bit stream.

`gdb` on the unfixed binary (`EC_AV1_PREFILT_DUMP` set, 12-bit fixture):
SIGSEGV in `__memmove_avx512_unaligned_erms` ← `_IO_default_xsputn` ←
`_IO_file_xsputn` ← `fwrite` ← `av1_decode_tg_tiles_and_wrapup`, with
`rsi = 0x3ffffbbb7850` (= `y_buffer`, row 0), `rdx = 0xa0` (160).

Rung 12 (`EC_AV1_FINAL_DUMP`) is depth-correct because it does the conversion:
`CONVERT_TO_SHORTPTR(ec_p8[ec_pl]) + (size_t)ec_r * ec_st[ec_pl]` with
`fwrite(..., 2, w, f)`. That is the shape every one of the four needed.

## 2. Reproduction (before)

Private oracle, `av112bit-key.obu` (sha256
`c74f62b57f71f7c36c49ea1feeb482786a2650e5c7f66610949b5f65fbbed8d1`, 160x128
4:2:0, bit_depth 12, one frame):

| rung | exit | file |
|---|---|---|
| `EC_AV1_PREFILT_DUMP` | 139 | 0 B |
| `EC_AV1_POSTDEBLOCK_DUMP` | 139 | 0 B |
| `EC_AV1_PREFILT_WIDE_DUMP` | 139 | 0 B |
| `EC_AV1_POSTCDEF_DUMP` | 139 | 0 B |
| `EC_AV1_FINAL_DUMP` (rung 12, untouched) | 0 | 61440 B = `160*128*2 + 2*80*64*2` |

The shared oracle (`~/.cache/aom-oracle/build/aomdec`) reproduces the same four
139s on the same fixture, so this is a pre-existing property of the committed
instrumentation, not of the private copy.

## 3. The fix

One helper, inserted before its first user, plus every legacy row loop
rewritten to call it (`scripts/instrument-aom-oracle.sh`, new trailing block
"depth contract for the four u8-narrowing dump rungs"):

```c
static void ec_dump_narrow_row(FILE *f, const YV12_BUFFER_CONFIG *b,
                               const uint8_t *plane8, int stride, int row,
                               int w) {
  if (!(b->flags & YV12_FLAG_HIGHBITDEPTH)) {
    fwrite(plane8 + (size_t)row * stride, 1, (size_t)w, f);   /* unchanged 8-bit bytes */
    return;
  }
  const uint16_t *p = CONVERT_TO_SHORTPTR(plane8) + (size_t)row * stride;
  uint8_t *narrow = (uint8_t *)aom_malloc((size_t)w);
  if (!narrow) return;
  for (int c = 0; c < w; ++c) narrow[c] = (uint8_t)(p[c] & 0xFF);
  fwrite(narrow, 1, (size_t)w, f);
  aom_free(narrow);
}
```

`p[c] & 0xFF` is the C spelling of the decoder side's `s as u8`. The 8-bit
branch is the original expression verbatim, so 8-bit output cannot move. The
row buffer is built per row so the rung still issues one `fwrite` per row, as
before. Rung 12 is not touched, and no rung the decoder does not pair with is
touched.

The block is a **repair pass**, not just a fresh-derivation step: it rewrites
the loops an earlier instrumentation left behind, so re-running
`instrument-aom-oracle.sh` on an already-instrumented tree fixes it in place.

Verified three ways on the C source:

- repair on the shared-baseline `decodeframe.c` → output **byte-identical** to
  the hand-applied fix in `~/.cache/aom-oracle3` (`diff` clean), 12 loops
  rewritten;
- second run → `no legacy row loop left (already depth-correct)`, file
  unchanged (idempotent);
- fresh derivation from the pristine upstream `decodeframe.c`
  (`git show 92d4c37:av1/decoder/decodeframe.c` from the oracle's own clone)
  through the whole script → the same helper text and the same 4 luma + 4 U +
  4 V `ec_dump_narrow_row` call sites, zero legacy loops.

## 4. After

Same fixture, same binary path, fixed oracle: all five rungs exit 0, and the
four fixed ones write `30720 B = 160*128 + 2*80*64` = one byte per sample.

## 5. Pairing proof — fixed oracle vs the decoder's own `as u8` dumps

Decoder side: `cargo run -p ec-av1 --example decode_probe <fixture>` with the
rung's env var set (`decode.rs:19722` `dump_stage`, `:19752`
`dump_prefilter_wide`, `:35263` prefilt — all narrow with `s as u8`).

**12-bit** (`av112bit-key.obu`, 160x128 4:2:0, bit_depth 12):

| rung | oracle | decoder | size | verdict |
|---|---|---|---|---|
| PREFILT | 0 | 0 | 30720 / 30720 | byte-identical |
| POSTDEBLOCK | 0 | 0 | 30720 / 30720 | byte-identical |
| PREFILT_WIDE | 0 | 0 | 30720 / 30720 | byte-identical |
| POSTCDEF | 0 | 0 | 30720 / 30720 | byte-identical |

Not vacuous: 30619 of 30720 bytes are non-zero.

**10-bit**, two fixtures, all four rungs byte-identical at 73728 B
(= `256*192 + 2*128*96`, one byte per sample):
`palette_screen_witness_10bit.obu` (sha256 `a60a8bc132e5db7b…`, 256x192,
15 frames) and `palette_screen_strip16_witness_10bit.obu` (sha256
`aaabbb001d87f61e…`).

**Multi-frame**, all frames identical: `av112bit-inter.obu` (sha256
`e679d577e61b4a8b…`, 2 frames) and `av112bit-compound.obu` (sha256
`a839cff534b89d5c…`, 6 frames) — 30720 B per frame on both sides, every
`.f<N>` equal.

**8-bit no-op**: fixed private oracle vs the shared oracle, same env var, same
stream, `cmp`:

| fixture | rung | shared | fixed | verdict |
|---|---|---|---|---|
| `422_allskip_2f.obu` (128x128 4:2:2) | all four | 0, 32768 B | 0, 32768 B | identical |
| `golden4-pin.obu` (64x64) | all four | 0, 6144 B | 0, 6144 B | identical |
| `av112bit-key.obu` (12-bit) | all four | **139, 0 B** | 0, 30720 B | the fix |

## 6. What a downstream lane should expect

Byte counts, one byte per sample at every depth, Y then U then V concatenated:

- `EC_AV1_PREFILT_DUMP` — crop extent: `y_crop_w*y_crop_h + 2*uv_crop_w*uv_crop_h`
  (12-bit 160x128 4:2:0 → 30720; 8-bit 64x64 → 6144).
- `EC_AV1_PREFILT_WIDE_DUMP`, `EC_AV1_POSTDEBLOCK_DUMP`, `EC_AV1_POSTCDEF_DUMP` —
  **mi-ALIGNED** extent `y_width*y_height + 2*uv_width*uv_height`, and
  **pre-superres / pre-LR**: the buffer is the coded one, so on a superres
  stream these are legitimately *not* the display size. `444_lossy_superres_256x128_d12_10bit.obu`
  measures exactly that: PREFILT_WIDE pairs byte-identically (67584 B) while the
  three crop-shaped rungs differ in size (65664 / 67584 vs the decoder's
  73728). Prove a rung on a NON-superres fixture, or pair the WIDE rung.
- `EC_AV1_FINAL_DUMP` (rung 12, unchanged) is the depth-*correct 16-bit* one:
  `2 * (crop samples)`, 61440 B on the 12-bit fixture. It is the only rung
  whose file length encodes the depth.

Rules of thumb:

- a size that is neither `w*h` per plane at one byte per sample (these four
  rungs) nor `2*w*h` (rung 12) means **the rung is wrong, not the decoder**;
- before blaming the decoder for a crashed aomdec, check the exit code: 139
  with a 0-byte dump file is a segfaulting rung, not a decode;
- a rung crash is not evidence about pixel content. Nothing was concluded about
  any decode from these crashes.

## 7. Notes for the next instrumentation addition

- Any new libaom rung that touches `YV12_BUFFER_CONFIG` planes must go through
  `CONVERT_TO_SHORTPTR` at high bit depth. Grep the new code for
  `->_buffer` arithmetic on the raw `uint8_t*`; that pattern is the bug.
- Strides in that struct are SAMPLES, not bytes, at every depth.
- Rung 12 was already right and is the model to copy.
- A segfaulting rung is silent about pixels; the file being 0 bytes is the only
  signal, and it looks exactly like a decoder crash from the outside.

## 8. Addendum (round 2, after PASS) — loud failure and the derivation check

### 8.1 Blast radius, stated plainly

Affected rungs, all four of them, all of them on **every** 10/12-bit stream:
`EC_AV1_PREFILT_DUMP`, `EC_AV1_POSTDEBLOCK_DUMP`, `EC_AV1_PREFILT_WIDE_DUMP`,
`EC_AV1_POSTCDEF_DUMP`. `EC_AV1_FINAL_DUMP` (rung 12) was never affected and is
still untouched.

The **shared** oracle reproduces all four 139s on the same fixture
(`~/.cache/aom-oracle/build/aomdec`, unmodified — see §5's last table), so this
was pre-existing for every lane that ever paired against those four rungs, not
something the private copy introduced. The damage is the *silence*: the rungs
left a 0-byte file behind, which downstream reads as "the oracle has no data
for this stage" and turns into a phantom stage diff — or, after the crash is
noticed, as evidence about the decoder when it is evidence about the oracle.

Rung 12 was correct all along, and that is what made the fix's shape obvious
rather than a guess: it is the one rung that calls
`CONVERT_TO_SHORTPTR(ec_p8[ec_pl])` and passes `2` as the element size. The
four broken rungs are the same code with that one conversion missing.

### 8.2 The failure is now loud

`ec_dump_finish()` is called instead of `fclose()` by all four narrowing rungs
(rung 12 keeps its own `fclose`). It compares the bytes the rung just wrote
against the shape that rung claims — `Y + U + V` at one byte per sample, crop
extent for PREFILT, mi-aligned extent for the other three — and on a mismatch
prints one line and `abort()`s:

```
EC_DUMP_ABORT rung=EC_AV1_PREFILT_DUMP bit_depth=12 hbd=1 expect=30720 wrote=30560 (one byte per sample, Y+U+V)
```

Measured non-vacuity: dropping PREFILT's last luma row (`ec_r <
y_crop_height - 1`) makes aomdec exit **134** with exactly that line; restoring
it gives exit 0 and 30720 B. A future narrowing regression now dies inside the
oracle with a self-naming message instead of handing the decoder an empty file.

No configuration legitimately writes 0 bytes: a frame's crop extent is
non-zero by construction, and a failed `fopen` skips the whole block, so a bad
output path is a missing file, not an abort. Monochrome streams are handled
(`expect = Y` when `num_planes == 1`).

Wiring is keyed on the presence of a rewritten `ec_dump_narrow_row` call in the
block, so rung 12 cannot be reached by the rewriter, and the terminator
alternation (`fclose(ec_f);` or an existing `ec_dump_finish(...)`) keeps each
block matched to its own close — a re-run cannot run one block's body forward
into a later rung's `fclose`.

### 8.3 Anti-regression for the script itself

`scripts/check-aom-oracle-rungs.sh` — a runnable command, not a note:

```
scripts/check-aom-oracle-rungs.sh [SRC_DIR] [BASE_REF]   # defaults: ~/.cache/aom-oracle/src, v3.13.3
```

It copies the oracle source to a scratch dir (the real tree is never written),
replaces `av1/decoder/decodeframe.c` with the **pristine upstream** file from
`$BASE_REF`, runs `instrument-aom-oracle.sh` over the copy, and asserts:

- 0 legacy `fwrite(ec_b->…_buffer + ec_r*ec_b->…, 1, …)` row loops;
- exactly 12 `ec_dump_narrow_row` call sites (4 rungs x 3 planes);
- exactly 4 `ec_dump_finish` call sites, one per named narrowing rung;
- rung 12 still calls `CONVERT_TO_SHORTPTR` and is NOT routed through the
  narrowing checker;
- a second run of the instrument script changes nothing (idempotence).

Current output: 10 `ok` lines, exit 0. Non-vacuity: with the depth block
deleted from a copy of the script, the same command prints 7 `FAIL` lines —
`legacy u8 row loops left: got 12, want 0` — and exits 1, in under a second and
with no build. A future edit that reintroduces the old expression reds on the
derivation path instead of on somebody's decode.

### 8.4 Bytes unchanged by the loudness work

Re-ran the full pairing after wiring the checker: `av112bit-key.obu` (12-bit)
and `palette_screen_witness_10bit.obu` (10-bit), all four rungs, oracle vs
decoder — all `IDENTICAL` (30720 B and 73728 B). The checker only reads
`ftell`; it cannot move a byte.
