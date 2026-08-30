# lane-palette report

VERDICT: NOTHING LANDED. Recon-only round -- turn budget (75 cap) was spent
entirely on reading decode.rs/cdf_state.rs/cdf.rs and the libaom oracle
(decodemv.c, pred_common.c/h, entropymode.c, detokenize.c, decoder.h) to
derive a complete, concrete implementation plan for palette Y (stage 1). No
edit was made; worktree is clean at 402ee40. This report is the handoff so
the next round can implement directly instead of re-deriving the algorithm.

## Why nothing landed
The charter's stage 1 alone touches: two new CDF tables (with their 4-site
wiring), two new `Neighbours` fields + a new record path, a new decode
function (color cache merge + delta-coded colour read + wavefront colour-index
map), a prediction-injection point in `PlaneBuf::reconstruct`, and a new gate
with a hand-built fixture. Deriving the exact algorithm (matched byte-for-byte
against libaom, per this project's `compare-range-not-tell` method) from cold
took the whole budget; implementing it untested would violate "never claim
green without running it," so I stopped rather than land unverified code.

## The derived plan (verified against libaom source, not yet against a
## decoded stream -- next round's job is to type this in and range-ladder it)

### 1. `crates/ec-av1/src/cdf.rs` -- new default tables
Add `PALETTE_Y_COLOR_INDEX: [[[u16; 9]; 5]; 7]` and
`PALETTE_UV_COLOR_INDEX: [[[u16; 9]; 5]; 7]` (indexed `[n-2][color_ctx]`,
width 9 = up to 8 symbols + terminal 32768 + 0 count, sliced `[..=n]` at the
call site since `SymbolDecoder::symbol` derives `nsyms` from `cdf.len()-1`).
Raw values transcribed from `~/.cache/aom-oracle/src/av1/common/entropymode.c`
`default_palette_y_color_index_cdf` / `default_palette_uv_color_index_cdf`
(entropymode.c:679-731 Y, :732-785 UV) -- copy the AOM_CDF macro arguments
directly, same convention this file already uses for `PALETTE_Y_SIZE` etc.
(raw forward values, not inverted, with a trailing `32768, 0`).

### 2. `crates/ec-av1/src/cdf_state.rs` -- 4-site wiring
- struct field: `pub palette_y_color_index: [[[u16; 9]; 5]; 7]`,
  `pub palette_uv_color_index: [[[u16; 9]; 5]; 7]` (next to `palette_y_size`
  at line ~428).
- defaults array (~line 1112): `palette_y_color_index: cdf::PALETTE_Y_COLOR_INDEX`,
  `palette_uv_color_index: cdf::PALETTE_UV_COLOR_INDEX`.
- save/restore: automatic (`Cdfs` is `#[derive(Clone, Copy)]`, whole-struct).
- counter reset (~line 652, alongside `reset3(&mut self.palette_y_mode)`):
  `reset3(&mut self.palette_y_color_index); reset3(&mut self.palette_uv_color_index);`
  -- `reset3::<N,M,K>` is generic over any `[[[u16;N];M];K]`, compiles as-is.

### 3. `crates/ec-av1/src/decode.rs` -- `Neighbours` (struct at line ~1239)
Add Y-only palette neighbour state (UV needs its own pair in stage 2):
```
above_palette_size: Vec<usize>,
left_palette_size: Vec<usize>,
above_palette_colors: Vec<[u16; 8]>,
left_palette_colors: Vec<[u16; 8]>,
```
init in `Neighbours::new` alongside `above_mode`/`left_mode`
(`vec![0; cols]` / `vec![[0u16; 8]; cols]` and the `left_*` twins).
New method, mirroring `record_rect` (line 1716)'s SUB-grid loop:
```
fn record_palette_y(&mut self, at: (usize, usize), side: usize, size: usize, colors: [u16; 8]) {
    let (r, c) = at;
    for cell in 0..side / SUB {
        self.above_palette_size[c + cell] = size;
        self.above_palette_colors[c + cell] = colors;
    }
    for cell in 0..side / SUB {
        self.left_palette_size[r + cell] = size;
        self.left_palette_colors[r + cell] = colors;
    }
}
```
Call it unconditionally (size=0 for a non-palette block, to clear stale
neighbour state) at the 3 call sites that can produce a palette block per
the charter's named scope: the key-frame square path (currently around
decode.rs:3061 and :3137, both branches of `decode_block`) and the
inter-frame intra-block reader (around :7953, alongside its existing
`neighbours.record_rect` call). Do NOT touch the two rect-strip
`record_rect` call sites (:2184, :2352) or the 8x8-leaf path (~:8909) --
those refuse palette syntax unconditionally already (unchanged scope,
charter does not name them), so their neighbours must stay at the default 0.

### 4. Palette mode/colour read (replaces the two refusal blocks at
decode.rs:2483-2496 and :7795-7805)
`palette_mode_ctx` (`av1_get_palette_mode_ctx`, pred_common.h:197): **no**
SB-row restriction, just `(above_size>0) as usize + (left_size>0) as usize`
read straight off the new `Neighbours` fields at column `c`/row `r`.
`palette_uv_mode_ctx`: `(y_palette_size > 0) as usize` -- the CURRENT block's
own just-decided Y size, not a neighbour lookup at all; trivial one-line fix,
must land in stage 1 even though UV reconstruction itself is stage 2 (the
refusal string stays, only the ctx changes from hardcoded 0).

Colour CACHE (`av1_get_palette_cache`, pred_common.c:73) has its own,
*different* restriction: null the **above** neighbour only (never left) when
`r % (SUB_PER_SB) == 0` i.e. the block sits at the very top row of a 64x64
superblock (`row % (1 << MIN_SB_SIZE_LOG2)`, MIN_SB_SIZE_LOG2=4 in 4x4-mi
units = 16 mi = 4 SUB-cells at SUB=16px -- so `r % 4 == 0` in this decoder's
SUB-grid units). Merge-sorted cache (ascending, dedup consecutive, cap
`2*PALETTE_MAX_SIZE`) -- straight port of `palette_add_to_cache` +
`av1_get_palette_cache`'s two-pointer merge, both bodies pasted in the recon
notes I fed the next round via chat (also derivable straight from
pred_common.c:73-116, already open in the oracle checkout).

`read_palette_colors_y` (decodemv.c:478-501) port, bit_depth=8 fixed (this
decoder is 8-bit only): read up to `n` cached-colour accept bits via
`dec.literal(1)`, then delta-code the remainder via `dec.literal(bit_depth)`
first value + `dec.literal(bits)` deltas (`bits` starts at `bit_depth-3 +
dec.literal(2)`, shrinks each step via `ceil_log2(range)`), then
`merge_colors` (decodemv.c:461-475, straight port) interleaves the cached and
transmitted lists back into ascending order. `av1_read_uniform`
(decoder.h:425) needed too, for the colour-index map's first symbol --
`get_unsigned_bits(n) = if n<=1 {0} else {32 - ((n-1) as u32).leading_zeros()}`,
then the NS(n) two-branch read already used implicitly nowhere else in this
file (new helper).

### 5. Colour index map (`decode_color_map_tokens`, detokenize.c:25-63 +
`av1_get_palette_color_index_context`, entropymode.c:893-967)
Wavefront over a `side x side` map (this decoder's blocks are always square
and this decoder already writes full `side*side` regions unconditionally
elsewhere -- e.g. the skip path's `vec![0i32; side*side]` residual -- so no
edge-cropping special case is needed, unlike libaom's rows/cols vs
plane_width/height split for non-block-aligned frame edges).
`map[0] = read_uniform(dec, n)`; then for `i in 1..2*side-1`, `j` from
`min(i, side-1)` down to `max(0, i-side+1)`: compute `(color_ctx, color_order)`
from the left/up-left/up already-decoded neighbours (weights `[2,1,2]`,
top-3 selection-sort exactly mirroring entropymode.c:924-946, hash
`scores[0]*1 + scores[1]*2 + scores[2]*2` through the fixed 9-entry
`av1_palette_color_index_context_lookup` table
`[-1,-1,0,-1,-1,4,3,2,1]`), read `dec.symbol(&mut cdfs.palette_y_color_index[n-2][ctx][..=n])`,
store `color_order[symbol]` at `map[row*side+col]`.

### 6. Wiring the map into reconstruction WITHOUT touching 32 call sites
`PlaneBuf::reconstruct` (decode.rs:2762) already branches on
`filter_intra: Option<usize>` to skip `predict()`; add a third override ahead
of it, a thread-local exactly like the existing `ENABLE_EDGE_FILTER` idiom
(decode.rs:478, `predict()`'s own comment at :2786-2799 names this exact
pattern as the reason to reach for a thread-local instead of a 24-call-site
signature change):
```
thread_local! {
    static PALETTE_PRED: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}
```
At the top of `reconstruct`, `if let Some(buf) = PALETTE_PRED.with(|c| c.borrow_mut().take())`
use `buf` as `prediction` directly (skips `edges()`/`predict()`/
`predict_filter_intra` entirely -- palette needs no edge pixels). `read_plane`
needs **zero** changes: it already just forwards to `plane.reconstruct(..)`,
so setting the thread-local right before calling it from `decode_block`
threads the override through transparently. Because AV1 palette blocks still
carry `mode == DC_PRED` (the palette-use symbol is gated on it, both call
sites), `default_intra_tx_type`/coefficient reading need no palette-specific
branch either -- the existing DC_PRED-derived tx_type is already correct.

Set the thread-local (`y_pred[i] = colors[map[i]] as u8`, row-major, matching
`prediction[row*side+col]`'s own indexing) right before each luma
`reconstruct`/`read_plane` call for a palette block: the skip branch's
`y.reconstruct` (line ~3024), the single-TU branch's `read_plane` (~3067),
and -- to avoid a same-shape refusal-vs-silent-desync gap -- each iteration of
the split-luma-TU loop (~3161), slicing the full `side*side` map down to that
TU's `tu_side x tu_side` sub-rectangle first. If that slicing proves too
error-prone under budget, the acceptable fallback is a named refusal
("a palette block with a split luma transform") rather than skipping it
silently -- confirm which by testing against a real aomenc stream with
`--enable-palette=1 --tune-content=screen` before deciding; do not guess.

### 7. Gate
New gate in `stream.rs`, matching an existing shape (`a_real_aomenc_stream_with_*`
pattern), `EC_AV1_REQUIRE_AOMENC=1`, `--threads=1 --row-mt=0
--enable-palette=1 --tune-content=screen`, fixture NOT `gradients_source`
(ignores its seed per an existing lane-vorbis dead-end already on file, and
per this charter's own note) -- a synthetic few-colour/repeated-tile pattern,
bounded with `-t`, hashed twice to prove determinism. Hard-assert a new
`PALETTE_HITS` thread-local (`fn record_palette_y`'s `size>0` branch
increments it) fires >0. Delete `enable-palette` from `gate_coverage.rs`'s
`NEVER_EXERCISED` list in the same commit that lands a firing gate.

## Next lever
Type in sections 1-6 above in one pass (they were derived to compile against
this exact codebase's existing helpers -- `dec.literal(bits)` already exists
and matches `aom_read_literal`/`aom_read_bit` bit-for-bit, `SymbolDecoder::symbol`
already derives `nsyms` from slice length so no new CDF-arity machinery is
needed), then range-ladder one aomenc `--enable-palette=1` stream against
`EC_TRACE`/`compare-range-not-tell` before writing the gate. Stage 2 (UV) is
almost free once stage 1's `merge_colors`/cache/map machinery exists --
`read_palette_colors_uv` only adds a differently-shaped V-channel branch
(decodemv.c:538-563, delta OR raw literal per its own leading bit).
