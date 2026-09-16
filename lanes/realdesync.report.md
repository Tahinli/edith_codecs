# lane realdesync (Vp9RealDiag) — FIRST divergent decision on real content

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-realdesync` @ `3dc719b4`, detached.
**No file under `crates/ec-vp9/src/` was edited.** Everything below is read-only diagnosis plus
two throwaway scratch locations: the oracle sources `/tmp/vp9loss/libvpx-src` (instrumented,
see §5) and a full copy of the crate at `/tmp/vp9fix` used to verify the fix in §4.

Content: a real 320x240 screen recording, re-encoded all-intra VP9 (`-g 1 -deadline good
-cpu-used 4 -crf 32 -tile-columns 0 -tile-rows 0`, 3 frames), IVF at `/tmp/ec320/allintra.ivf`.

---

## 1. The first divergent decision

**Block:** `mi_row=16, mi_col=0, bsize=BLOCK_4X4 (0), tx_size=TX_4X4 (0), mode=2, uv_mode=0,
skip=0, segment=0` — the **first block of superblock row 2**; its luma TX_4X4 transform block
sits at pixel `(x=0, y=128)`.

**First syntax element whose value differs:** the first bool of that transform block's
coefficient syntax — the EOB branch byte read with
`coef_probs[TX_4X4][PLANE_TYPE_Y][band 0][ctx][EOB_CONTEXT_NODE]`
(libvpx `vp9_detokenize.c` `decode_coefs`: `band = *band_translate++; prob = coef_probs[band][ctx];
if (!read_bool(r, prob[EOB_CONTEXT_NODE], ...)) break;`).

| | prob | bit | txb_ctx | trace |
|---|---|---|---|---|
| oracle (libvpx `drv`) | **195** | 1 | **0** | `/tmp/ot_trace_txb.txt` line **77376** (`BOR 7113 <count> 195 1`, bool #74792) |
| ours (`ec-vp9`) | **84** | 1 | **1** | `/tmp/ot_ours.txt` line **93153** (`B 7114 <count> 84 1`, bool #74792) |

Everything before it agrees: the first **74 792** `read_bool` calls of the frame are identical in
probability **and** in decoded bit between the two decoders (index-aligned, because both streams
agree by construction to that point), including the 26 bools of that block's own mode info
(partition tree + 4x4 intra modes + tx flag) which immediately precede it.

The prob⇒ctx mapping is not assumed; it is **recovered from the oracle's own trace** (TXBDUMP
gives the ctx of every transform block, BTRACE gives every bool): for `(plane 0, TX_4X4)` the
EOB-node prob is single-valued per ctx —

```
(plane,tx,ctx) -> EOB-node prob (recovered over the whole agreeing prefix)
(0,0,0) -> 195     (0,0,1) -> 84     (0,0,2) -> 18
(1,0,0) -> 214     (1,0,1) -> 132    (1,0,2) -> 42      # U
(2,0,*) -> same as U                                   # V
```

So the oracle asked its question with **ctx=0** and we asked the same question with **ctx=1**;
libvpx ended up decoding `eob=4` for that TU.

Our own decoder's `CTX` trace hook prints the reason directly (it is exactly the code at
`decode.rs:730-738`):

```
[reads= 74792] MODE row=16 col=0 bsize=0 skip=0 tx=0 y=2 uv=0
[reads= 74792] CTX p=0 prob=[84, 32, 64] ax=0 ay=0 a=0 l=1 ctx=1
[reads= 74792] TXB p=0 x=0 y=128 tx=0 a=0 l=1 deq=38,44
```

`a=0` (above eob context) is right; **`l=1` (left eob context) is stale** — the left slot must be
0 at the first column of a superblock row, and it is not.

**Ruled out** (all three would have produced the same "first divergence at a read" shape):
* partition / bsize / tx_size / intra-mode divergences — the 26 mode-info bools and the oracle's
  `MODE 0 16 0 0 0 2 0 0 0` line agree with our `MODE row=16 col=0 bsize=0 skip=0 tx=0 y=2 uv=0`;
* the frame's coefficient probability table (`coef_probs` after the header's diff updates): our
  prob **84** *is* the frame's ctx=1 entry, i.e. the table is right, only the ctx index is wrong;
* a plane/tx-size/band misread: the prob rows our decoder printed (`[84, 32, 64]`) are a single
  coherent (band 0, ctx 1) row of the TX_4X4/Y table.
* decoded-value drift in the arithmetic decoder: the bool *values* agree; only the probability
  differs, so no reader-state desync preceded this point.

---

## 2. Ranked candidate root causes

### 1. (PROVEN — and fixed+verified in a throwaway copy) missing per-superblock-row reset of the left entropy contexts

`crates/ec-vp9/src/decode.rs`, tile loop (`~:270-276`):

```rust
for sb_row in (row_lo..row_hi).step_by(SB_MI) {
    if sb_row == row_lo {                                  // <-- only the TILE'S FIRST SB row
        ectx.iter_mut().for_each(|p| p.left = [0; 32]);
        mi.left_seg = [0; 32];
    }
    for sb_col in (col_lo..col_hi).step_by(SB_MI) { ... }
```

libvpx clears the same state at the start of **every** superblock row — `vp9_decodeframe.c:2256-2258`
(the plain `decode_tiles` path, which is what a 1-thread `drv` takes):

```c
for (mi_row = tile->mi_row_start; mi_row < tile->mi_row_end; mi_row += MI_BLOCK_SIZE) {
  vp9_zero(tile_data->xd.left_context);
  vp9_zero(tile_data->xd.left_seg_context);
  for (mi_col = ...) decode_partition(...);
}
```

with the row-mt twin at `:2114-2115` and the per-tile-row paths at `:1825-1826`, `:1898-1899`.
`MACROBLOCKD.left_context` is an **array**, `ENTROPY_CONTEXT left_context[MAX_MB_PLANE][16]`
(`vp9/common/vp9_blockd.h:192`), so `vp9_zero` clears all 16 slots of all 3 planes; likewise
`left_seg_context[8]` (`blockd.h:195`).

Why this is the whole story of the observed shape:
* the plane-0 left slot for luma is `ay = ((mi_row << 1) + tx_row) % 32` (`decode.rs:723`), so it
  **wraps at mi_row 16** — at the first block of SB row 2, `ay = 0` still holds whatever was
  written there during SB row 0 (the frame's first rows), instead of libvpx's zeroed array;
* every earlier SB row is unaffected in the fixtures because the wrap-around slot only collides
  with a *two-SB-row-old* value, and the two fixtures never leave a nonzero eob in that slot;
* the desync is a hard bitstream desync, which is why rows 128..239 are not "slightly wrong" but
  garbage — after this read the two decoders are reading different syntax.

**Verification (single-line change, run in a copy — the worktree's `src/` was not touched):**

```
cp -r <worktree>/crates /tmp/vp9fix/crates                  # + trimmed workspace Cargo.toml
# patch: delete the `if sb_row == row_lo {` guard (keep both reset statements)
cd /tmp/vp9fix && SWEEP_TILE0=1 SWEEP_SRC=/tmp/small320.mp4 SWEEP_N=3 \
  CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9fix \
  cargo test -p ec-vp9 --test scratch_realsweep -- --nocapture
->
frame 0: 320x240 stride 320 uv_stride 160 Y 0 U 0 V 0
frame 1: 320x240 stride 320 uv_stride 160 Y 0 U 0 V 0
frame 2: 320x240 stride 320 uv_stride 160 Y 0 U 0 V 0
RESULT 3 of 3 frames byte-exact (source /tmp/small320.mp4)
```

Pre-fix, the same command on the same content (lane worktree, unmodified src):

```
frame 0: 320x240 stride 320 uv_stride 160 Y 35527 U 7607 V 7922
RESULT 0 of 1 frames byte-exact
```

The stated per-row shape (0 diffs in rows 0-127, all diffs in rows 128-239) is consistent with
the proven break at `mi_row 16` = y 128; the per-row split itself is task-stated, not re-derived
here (total above is my own measurement on a freshly re-encoded IVF, so the totals differ
slightly from the ones in the task facts — same encode parameters, re-run encode).

### 2. (ruled out) coefficient-probability-table / header prob-update parse bug
Our first divergent prob (84) equals the frame's own ctx=1 entry for that (tx, type, band) —
recovered from the oracle's table via TXBDUMP+ctx. A table-entry bug would have shown a prob that
no ctx of that row carries. Refuted.

### 3. (ruled out) tx_size / plane / band misinterpretation at the SB-row boundary
The mode-info bools and the oracle's MODE line for that block agree (bsize 4x4, tx 4x4, skip 0),
and our prob row `[84, 32, 64]` is a single coherent (band 0, ctx 1) row. Refuted.

### 4. (open, same class — not proven) `mi.left_seg` partition context
The identical guard also held the partition left-context reset (`mi.left_seg`), and libvpx
clears `left_seg_context` at the same two lines. At this block the partition bools *agreed*, so
there is no proven manifestation; the fix above moves both resets out of the guard (that is what
was verified). Anyone tempted to split the fix should keep them together.

### 5. (side observation, not a root cause) our read budget after the desync
Our trace contains 105 917 bool reads for this frame, the oracle 121 264; after bool #74792 the
two streams are unrelated by definition. The tile reader's `ensure(r.overreads() == 0)` did *not*
trip, i.e. our decoder never ran past the tile end — it re-interpreted the remaining bytes as
different syntax rather than over-reading.

---

## 3. Where the fix goes (exact patch — NOT applied)

File: `crates/ec-vp9/src/decode.rs`, in the per-tile `for sb_row in (row_lo..row_hi).step_by(SB_MI)`
loop (currently `~:270-276`). Delete the guard, keep both statements:

```diff
                 for sb_row in (row_lo..row_hi).step_by(SB_MI) {
-                    if sb_row == row_lo {
-                        ectx.iter_mut().for_each(|p| p.left = [0; 32]);
-                        mi.left_seg = [0; 32];
-                    }
+                    // libvpx: decode_tiles zeroes the left ENTROPY and PARTITION
+                    // contexts at the start of EVERY superblock row
+                    // (vp9_decodeframe.c:2256-2258), not only the tile's first one:
+                    // the left edge of an SB row has no pixels to its left.
+                    ectx.iter_mut().for_each(|p| p.left = [0; 32]);
+                    mi.left_seg = [0; 32];
                     for sb_col in (col_lo..col_hi).step_by(SB_MI) {
```

Nothing else changes: `above` must keep carrying across SB rows (`ectx[].above.fill(0)` once per
tile at `:268` is correct, and `mi.above_seg.fill(0)` likewise), and the resets stay inside the
per-tile loop, matching libvpx's per-tile left-context lifetime.

Follow-up taste call (not needed for correctness): libvpx's luma left array is 16 slots indexed
by the row *within* the SB row (`left_context[plane][16]`); ours is 32 slots with a `% 32` index.
Clearing all 32 per SB row is semantically identical to libvpx's clear of all 16 (no slot older
than the current SB row may be read), so the minimal fix is enough.

---

## 4. Reproduction in under a minute

```bash
# 0. lane env
export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9diag
cd <worktree>                       # edith_codecs-realdesync @ 3dc719b4

# 1. build the all-intra IVF (single tile) + confirm the desync numbers
SWEEP_TILE0=1 SWEEP_SRC=/tmp/small320.mp4 SWEEP_N=1 \
  cargo test -p ec-vp9 --test scratch_realsweep -- --nocapture
#   -> frame 0: 320x240 ... Y 35527 U 7607 V 7922 / RESULT 0 of 1 frames byte-exact
mkdir -p /tmp/ec320 && cp /tmp/ec-vp9-sweep/allintra.ivf /tmp/ec320/   # private copy: never point
                                                                      # SWEEP_IVF at the harness's own output path

# 2. oracle bool trace (instrumented libvpx at /tmp/vp9loss/libvpx-src, see §5)
BTRACE=1 MODEDUMP=1 TXBDUMP=1 ONE=1 /tmp/vp9loss/drv /tmp/ec320/allintra.ivf > /tmp/ot_trace_txb.txt 2>&1

# 3. our bool trace (EC_VP9_TRACE also prints the CTX/TXB lines used above)
SWEEP_IVF=/tmp/ec320/allintra.ivf SWEEP_N=1 EC_VP9_TRACE=1 \
  cargo test -p ec-vp9 --test scratch_realsweep -- --nocapture > /tmp/ot_ours.txt 2>&1

# 4. first divergence (python, ~10 lines): zip the "BOR " and "B " lines and find the first
#    index where (prob, bit) differ -> index 74792, oracle prob 195 vs ours 84.
#    grep -n for the neighbourhood:
sed -n '77370,77380p' /tmp/ot_trace_txb.txt      # BOR ... 195 1 at line 77376, MODE line 77375
sed -n '93145,93160p' /tmp/ot_ours.txt           # B ... 84 1, CTX line a=0 l=1 ctx=1

# 5. verify the fix in a throwaway copy (worktree src untouched; verified green, see §2.1)
cp -r crates /tmp/vp9fix/crates   # + root Cargo.toml with members = the 3 crates ec-vp9 needs
#    remove the `if sb_row == row_lo {` guard in /tmp/vp9fix/crates/ec-vp9/src/decode.rs
cd /tmp/vp9fix && SWEEP_TILE0=1 SWEEP_SRC=/tmp/small320.mp4 SWEEP_N=3 \
  CARGO_TARGET_DIR=$HOME/.cache/cargo-target-vp9fix \
  cargo test -p ec-vp9 --test scratch_realsweep -- --nocapture
#   -> RESULT 3 of 3 frames byte-exact
```

---

## 5. Oracle instrumentation added (all inside `/tmp/vp9loss/libvpx-src`, repuildable)

`make -j6` in `/tmp/vp9loss/libvpx-src`, then
`gcc -O1 -g -o /tmp/vp9loss/drv drv.c -Ilibvpx-src -Ilibvpx-src/build/make -Llibvpx-src -lvpx -lm -lpthread`.

| file | change | purpose |
|---|---|---|
| `vpx_dsp/bitreader.h` | `extern const uint8_t *g_btrace_base;` after `vpx_integer.h`; `if (getenv("BTRACE")) fprintf(stderr, "BOR %d %d %d %d\n", (int)(r->buffer - g_btrace_base), r->count, prob, bit);` before `return bit;` in `vpx_read` | header-phase bool trace |
| `vpx_dsp/bitreader.c` | `if (!g_btrace_base) g_btrace_base = buffer;` in `vpx_reader_init` | first-wins frame base |
| `vp9/decoder/vp9_detokenize.c` | the same `BOR` print at both `return 1;` / zero-branch exits of the static `read_bool` (the cached `value/count/range` variant the tokenizer uses) | coefficient-phase bool trace. NOTE: `r->buffer` only advances at refills here, so `pos` is stale by up to 8 bytes in this phase — align on (prob,bit) only |
| `vp9/decoder/vp9_decodeframe.c` | `const uint8_t *g_btrace_base;` + `g_btrace_base = data;` in `setup_token_decoder`; `g_btrace_base = NULL;` at the top of `vp9_decode_frame`; `tpos=%d` appended to the existing `MODEDUMP` line | base + per-block reader offset |

Existing oracle hooks used as-is: `MODEDUMP` (per-block bsize/tx/mode/uv_mode/skip/segment, printed
at the end of `vp9_read_mode_info` in `decode_block`), `TXBDUMP` (per transform block
plane/x/y/tx/ctx/eob/dequant), `PREDDUMP`, `LFTRACE`/`LFMASKS`.

Reading pitfalls confirmed (in addition to `lanes/vp9kf.report.md` §"Trace-reading pitfalls"):
* a `TXB`/`MODE` marker line prints **after** the bools that preceded it — a transform block's
  reads are the range `[previous marker's bool index, this marker's bool index)`, not the range
  starting at the marker. Getting this backwards (as the first pass of this analysis did) makes
  the same `(plane,tx,ctx)` appear to carry several different EOB probs;
* the oracle prints `MODE` from `decode_block`, i.e. only on the non-row-mt path (the path a
  1-thread `drv` takes); `parse_block`/`reconstruct_block` (row-mt) have no such print;
* `SWEEP_IVF` must not point at `/tmp/ec-vp9-sweep/allintra.ivf` — the harness copies source to
  destination, and source == destination truncates the file ("ffmpeg decode of the IVF failed").

---

## 6. Evidence index (command -> observed)

| # | command | observed |
|---|---|---|
| e1 | `SWEEP_TILE0=1 SWEEP_SRC=/tmp/small320.mp4 SWEEP_N=1` `cargo test -p ec-vp9 --test scratch_realsweep -- --nocapture` | `frame 0: 320x240 stride 320 uv_stride 160 Y 35527 U 7607 V 7922` / `RESULT 0 of 1 frames byte-exact` |
| e2 | `BTRACE=1 MODEDUMP=1 TXBDUMP=1 ONE=1 /tmp/vp9loss/drv /tmp/ec320/allintra.ivf` | `bor=121264 txb=3579 mode=996` lines |
| e3 | `SWEEP_IVF=/tmp/ec320/allintra.ivf SWEEP_N=1 EC_VP9_TRACE=1 cargo test -p ec-vp9 --test scratch_realsweep -- --nocapture` | `blines=105917`, same Y/U/V numbers as e1 |
| e4 | index-aligned diff of the two bool streams | first mismatch at index 74792: `O pos=7113 prob=195 bit=1` / `M pos=7114 prob=84 bit=1`; 74792 reads agree before it |
| e5 | last markers before the divergence in `/tmp/ot_trace_txb.txt` | `MODE 0 16 0 0 0 2 0 0 0 tpos=7113` @ bool index 74792 (preceded by `MODE 0 15 37 3 1 0 0 1 0`, `MODE 0 14 38 6 2 9 0 0 0` + its 3 `TXB` lines) |
| e6 | EOB-node prob per ctx recovered from e2's BTRACE+TXBDUMP | `(0,0,0)->195  (0,0,1)->84  (0,0,2)->18` |
| e7 | our `CTX` trace at bool index 74792 | `CTX p=0 prob=[84, 32, 64] ax=0 ay=0 a=0 l=1 ctx=1` + `TXB p=0 x=0 y=128 tx=0 a=0 l=1` |
| e8 | `sed -n '270,276p' crates/ec-vp9/src/decode.rs` | `if sb_row == row_lo { ectx...left = [0; 32]; mi.left_seg = [0; 32]; }` |
| e9 | `sed -n '2256,2258p' /tmp/vp9loss/libvpx-src/vp9/decoder/vp9_decodeframe.c` | `for (mi_row = ...; mi_row += MI_BLOCK_SIZE) { vp9_zero(tile_data->xd.left_context); vp9_zero(tile_data->xd.left_seg_context);` |
| e10 | `grep -n left_context /tmp/vp9loss/libvpx-src/vp9/common/vp9_blockd.h` | `192: ENTROPY_CONTEXT left_context[MAX_MB_PLANE][16];` (array ⇒ `vp9_zero` clears it) |
| e11 | fix applied to `/tmp/vp9fix` copy, `SWEEP_N=3` sweep | `frame 0/1/2: Y 0 U 0 V 0` / `RESULT 3 of 3 frames byte-exact` |

No `BLOCKED:` line — the first divergent decision is a single syntax element, the root cause is
proven and the fix is verified end-to-end. Remaining (optional) follow-up: a regression test that
fails pre-fix, i.e. a witness for the per-SB-row left-context reset, and re-running the 1080p case
(the task's other repro) after the fix.
