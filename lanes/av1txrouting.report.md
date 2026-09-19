# lane-av1-txrouting — the one-routing-predicate hypothesis is REFUTED; work-in-progress

Base: `main` **0cfb9af5** (2026-09-19). Worktree `../edith_codecs-av1txr`, branch
`lane-av1-txrouting`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1txr`.

**STATUS: WIP, NOT READY TO MERGE.** The 4x4 sub-8x8 intrabc path is proven
symbol-for-symbol correct on a real stream; the 4x8/8x4 rect leaf path is
**not yet byte-exact** (see §5). No gate has been run to completion.

---

## 1. Headline: the scout's "ONE routing predicate" is refuted

The charter's three stops do **not** share a routing predicate. First-divergence
traces (instrumented `aomdec` oracle at `~/.cache/aom-oracle/build/aomdec` +
`decode_probe`, `EC_TRACE_MODE_STEP`) give each stop a distinct cause:

| stop | stream (reproduced) | first divergence | real cause |
|---|---|---|---|
| 3.2 / "Golomb tail" | `aomenc 4:4:4 --cpu-used=6/7` | block `(0,0)`, chroma transform entry rng **39857** | **4:4:4 chroma-subsampling is not implemented** (see §3) |
| 3.3 / "sub-8x8 intrabc" | `aomenc --allintra` | block `(45,52)` rng 37280 | a **genuine** 4x4 intrabc block (see §4) |
| 3.4 / "sub-8x8 intrabc" | `aomenc --enable-warped-motion=1 --cpu-used=0` | block `(37,62)` | the same, at a 4x8/8x4 rect leaf |

The report's §3.2/§3.3 claim that "`aomdec` reads `tx_depth` where ours reads
`txfm_split`/`intrabc`/`uv_mode`" is an **instrumentation artifact**: aomdec's
`read_intrabc_info` had no print, and `read_tx_size_vartx`'s print is gated on
`EC_VARTX`, not `EC_ISTEP`. With the oracle instrumented for both
(`EC_ISTEP name=intrabc`, `EC_DV`, `EC_COEFF`), ours and aomdec read
**identical `intrabc` flags and identical DVs** at these blocks — no desync.

Similarly the §3.2 "aomdec reads `tx_depth`, ours reads `txfm_split`" claim:
the 4:4:4 divergence is **after** the tx-depth read (both read
`tx_depth val=1 ctx=0 cat=1`) and is a chroma-transform size mismatch, not a
tx-size routing mismatch.

## 2. Reproduced streams (all re-derivable)

```
cd ~/.cache/av1txr
# 4:4:4 (aomenc) -- cu6/cu7 REFUSE, cu4/cu5/cu8 decode-but-wrong
aomenc --codec=av1 --passes=1 --threads=1 --cpu-used=6 --obu -o s/a444-cu6.obu s/s444.y4m
# sub8 intrabc
aomenc --codec=av1 --passes=1 --threads=1 --obu --allintra --cpu-used=6 -o s/allintra.obu s/s420.y4m
aomenc --codec=av1 --passes=1 --threads=1 --obu --enable-warped-motion=1 --cpu-used=0 -o s/warped.obu s/s420.y4m
# (s420.y4m / s444.y4m = ffmpeg testsrc2 320x240 rate 8 dur 3)
```

Pre-fix: `allintra`/`allintra0`/`warped` → `REFUSED: a sub-8x8 leaf that uses
intrabc …`; `a444-cu6/7` → `REFUSED: a Golomb tail longer than this decoder
reads`; `a444-cu4/5/8` → `OK` **but garbage**.

## 3. Stop 3.2 — 4:4:4 is entirely unsupported (root-caused)

`probe EC_PROBE_OUT8` vs `ffmpeg -pix_fmt yuv420p` for **every** 4:4:4 stream
(`a444-cu4/5/8`, `ff444-cu5..8`) **differs** — the "decoding" 4:4:4 streams are
silently desynced garbage. The fixture story ("`av1-profile1-444` decodes
fine") was never byte-exactness-checked.

**First divergence (a444-cu6, block `(0,0)`, an 8x16 rect leaf, depth 1):**

```
aom:  EC_ISTEP tx_depth val=1 ctx=0 cat=1 rng=44248
      EC_COEFF mi_row=0 mi_col=0 plane=0 tx=1 rng=44248   # TX_8X8
      EC_COEFF mi_row=0 mi_col=0 plane=0 tx=1 rng=41886   # TX_8X8
      EC_COEFF mi_row=0 mi_col=0 plane=1 tx=7 rng=39857   # TX_8X16  <-- chroma
      EC_COEFF mi_row=0 mi_col=0 plane=2 tx=7 rng=64008   # TX_8X16
ours: … luma units rng 44248, 41886 (MATCH) …
      read_coeffs_rect chroma entry rng=39857, w=4 h=8      # 4:2:0 half  <-- DIVERGE
```

The decoder hardcodes 4:2:0 chroma extents at every rect site
(`let (chroma_w, chroma_h) = (bw / 2, bh / 2);` — `decode.rs:9476,10888,11378,
11767,12772,14326,15042,26798`) and the probe only ever emits 4:2:0 planes.
`subsampling_x/y` is parsed by `ec-av1-syntax` but **never read by the decoder**.

**Disposition: needs a `num_planes`/subsampling threading lane** (like the
sibling mono lane). The honest interim fix is a **new named refusal at the
sequence-header level** for `subsampling_x != 1 || subsampling_y != 1`
(`num_planes > 1`), so a 4:4:4/4:2:2 stream refuses by name instead of
producing garbage. **Not yet written** (budget).

## 4. Stops 3.3 / 3.4 — genuine sub-8x8 intrabc (implemented, WIP)

The trace proves both streams code **real** sub-8x8 intrabc blocks:
`aomdec`'s instrumented `read_intrabc_info` reads `intrabc val=1` and an
`EC_DV` at `allintra (45,52)/(45,53)` and `warped (37,62)`. libaom's
`av1_allow_intrabc` is a frame-level test with no shape term, and the 4x4 block
`is_chroma_ref` (last sub-block) makes its chroma an intrabc copy too.

Implemented on the lane (see §6 diff):
- `read_intra_mode_sub8` reads the DV (`read_intrabc_dv`) and RETURNS
  (skip, DC_PRED, 0, None, None, Some(dv)) — no mode/uv/filter-intra syntax.
- `read_intrabc_dv` generalized from a square `side` to a `(n4_w, n4_h)`
  footprint (rect leaves scan 1x2/2x1, not 2x2).
- `decode_leaf_split4` / `decode_leaf_rect8` reconstruct the intrabc leaf:
  `flush_recon` + `mc::predict_with_filter(.., Bilinear)` frame copy injected
  via the `set_palette_pred` override slot, residual read through the **inter**
  sets (`Luma4Inter`/`Luma4InterSet1`, `LumaRect8x4Inter`/`Set1`), no
  `tx_depth` symbol (libaom `block_signals_txsize` false below 8x8),
  `record_intrabc_mi(_rect)` publishing the true footprint.
- **Chroma tx-type inheritance** (the subtle one): libaom `av1_get_tx_type`
  (blockd.h:1278) gives a non-Y plane the LUMA's `tx_type_map` entry when
  `is_inter_block(mbmi)` — **intrabc counts as inter**. So the group's chroma
  eob/scan class is the luma's coded type, not `DCT_DCT`. Armed via the
  existing `fctx.intrabc_chroma_tx` slot (the square path's mechanism).

**Evidence the 4x4 path is byte-exact (allintra, block (45,52)):** the luma
intrabc residual traces **identically** to aomdec
(`tx_type txtype=11`/`HDct`, `eob=14`, every `base`/`br`/`sign`/`post_golomb`
rng equal), and ours reaches `(45,53) skip rng=52504` — aomdec's exact value.
With the chroma-inheritance fix the following block's `skip` also matched.

## 5. What is NOT done (do not merge without it)

1. **`warped` is not byte-exact.** With the implementation it decodes 24
   frames but `EC_PROBE_OUT8` vs `ffmpeg -pix_fmt yuv420p` differs at byte
   **41217** (frame 0, luma row 128), and the entropy desyncs at
   `(38,60) skip`: aom 48630 vs ours 60320. The **rect intrabc leaf**
   (4x8/8x4) is the suspect: unverified `predict_with_filter` buffer shape,
   residual set, or the rect DV scan.
2. **`allintra` now stops one shape later**: `REFUSED: intra block copy on a
   HORZ/VERT/1:4 rect intra strip (reconstruction is not ported at this
   shape)` (the `decode.rs:9122` rect reader). `allintra0` (cpu-used 0)
   **panics** at `transform.rs:1134` (a 4x4 dequant given a 1024-entry grid)
   via `decode_leaf8` — a further reachable path after the sub8 blocks pass.
3. **4:4:4/4:2:2 named refusal** (§3) not written.
4. **No gates run.** Only `cargo build --release -p ec-av1 --example
   decode_probe` (0 errors/warnings) and `cargo check`-equivalent builds.
   `cargo test -p ec-av1 --lib` (592/0/60 baseline) NOT run.
5. **Inventory re-pin not done.** §3.3's two refuted PROVEN entries
   (`read_golomb_reads_every_value…`, `a_sub8_leaf_census…`) still claim
   "unreachable"; they must be re-pinned to reachable-with-witness. The report
   §5 tripwire `a_real_libaom_monochrome_key_frame_is_refused_by_name` is
   untouched (mono stream unaffected — its `num_planes == 1`).

## 6. Diff summary (lane, uncommitted-at-first-write)

`crates/ec-av1/src/decode.rs`:
- `read_intra_mode_sub8`: +`Option<(i32,i32)>` return, intrabc branch (DV read,
  early return), `istep!("intrabc")`.
- `read_intrabc_dv`: `side` → `(n4_w, n4_h)`; `EC_DV` trace print; call sites
  updated (square path passes `(side/MI, side/MI)`).
- `record_intrabc_mi` split into `record_intrabc_mi_rect` (rect footprint).
- `decode_leaf_split4` + `decode_leaf_rect8`: intrabc leaf arm (frame copy +
  inter residual) and intrabc chroma arm (frame copy + `Chroma4`, luma tx-type
  inheritance, cleared after).
- Trace prints added at previously-silent `tx_depth`/`intrabc` sites so a
  cross-decoder first-divergence diff aligns.

## 7. Repro / oracle recipes

```
# build
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1txr cargo build --release -p ec-av1 --example decode_probe

# byte-exactness, 8-bit
decode_probe x.obu out8.raw                          # argv[2] = 8-bit planes
ffmpeg -i x.obu -pix_fmt yuv420p -f rawvideo ref8.raw -y
cmp out8.raw ref8.raw
#   (EC_PROBE_OUT16 is yuv420p10le and is NOT comparable to ffmpeg's
#    yuv420p10le for an 8-bit source — ffmpeg left-shifts, the probe does not.
#    Use the 8-bit form for 8-bit streams. This bit me once.)

# first divergence vs the instrumented oracle
EC_TRACE_MODE_STEP=1 ~/.cache/aom-oracle/build/aomdec --rawvideo -o /dev/null x.obu 2>a.step
EC_TRACE_MODE_STEP=1 decode_probe x.obu 2>o.step
# per-(mi_row,mi_col,name) index compare in python (see §1 method)
```

Oracle build: `~/.cache/aom-oracle` (cmake+ninja) instrumented with
`EC_ISTEP name=intrabc` (decodemv.c `read_intrabc_info`), `EC_DV`
(post-`assign_dv`), `EC_COEFF` (decodeframe.c `read_coeffs_tx_intra_block`).

## 8. Recommended next steps (ordered)

1. Fix the rect intrabc leaf (warped) — trace the rect leaf's DV/residual vs
   aomdec; then verify `EC_PROBE_OUT8` byte-exact.
2. Unblock `allintra`'s 1:4 rect strip intrabc (`decode.rs:9122`) and the
   `decode_leaf8` dequant panic.
3. Add the sequence-header subsampling refusal (§3) and re-pin the inventory.
4. Run `cargo test -p ec-av1 --lib` + `cargo check -p ec-av1 --all-targets`.

---

## 9. Continuation (2026-09-20, second lane pass)

Five defects fixed, each proven by a first-divergence move and/or a gate. **Warped
is still NOT byte-exact** (§9.4) and `allintra`/`allintra0` still stop at NAMED
refusals that belong to the 1:4-rect-strip lane (§9.3). Honest status first.

### 9.1 Silent-garbage guard: non-4:2:0 chroma is refused by name

`stream.rs`'s `decode_stream` now refuses `seq.subsampling_x != 1 ||
seq.subsampling_y != 1` (threaded through `SeqFlags::subsampling_x/y`). A
monochrome header parses as 1,1, so it is untouched (the mono lane's stream and
its tripwire keep their meaning).

Evidence:
- `a_non_420_subsampled_sequence_header_is_refused_by_name` (stream.rs tests):
  a hand-built profile-1 (4:4:4) and profile-2 (4:2:2) header each refuse with
  the named string, and the profile-0 CONTROL still decodes — so the gate is
  about the shape, not the construction.
- Real encoder stream: `aomenc` auto-promotes to profile 1 for `yuv444p` input;
  `EC_PROBE_OUT` vs `ffmpeg -pix_fmt yuv444p` on that stream **refuses by name**
  where the same code path used to decode silently wrong pixels.

### 9.2 Parsed-but-unread pixel-affecting header field sweep

A census of every `SequenceHeader`/`ColorConfig`/frame-header field against the
decoder's reads (scout report, re-derivable by grep) found exactly two
pixel-affecting fields the decoder never consumed; both are now closed:

1. `subsampling_x/y` — §9.1 (refusal).
2. **`disable_cdf_update`** (the new find): spec 8.3.2 skips ALL per-symbol CDF
   adaptation when set, and libaom's tile entry does exactly that
   (`decodeframe.c:2909`, `allow_update_cdf && !disable_cdf_update`). The
   decoder adapted unconditionally. Fixed at the one authoring point:
   `msac::SymbolDecoder::adapt` (per READER, not global — frames decode on
   different workers) + `FrameCtx::disable_cdf_update` + one line in
   `decode_frame`.

   Evidence, both directions:
   - `a_real_aomenc_stream_with_cdf_update_disabled_decodes_pixel_exact`
     (real `aomenc --cdf-update-mode=0`, 3 frames): asserts every frame header
     carries the bit, then compares all planes against ffmpeg. Green.
   - fail-pre-fix, measured: with `set_cdf_update` forced ON the SAME stream
     differs from ffmpeg at **byte 6** (`OK: 3 frames` but wrong pixels).
   - `a_reader_told_not_to_adapt_matches_a_non_adapting_writer` (msac unit):
     a non-adapting reader reproduces a non-adapting writer symbol-for-symbol
     and its tables stay untouched; the default reader does not (flag
     load-bearing).

Everything else never-read is dispositioned as metadata (color_primaries /
transfer_characteristics / color_range / chroma_sample_position, timing and
frame-id fields, `showable_frame`, `render_width/height` — deliberate per
`encode.rs:15917-15921`), or consumed indirectly by the parser (seq gates forced
into the frame header: superres/CDEF/restoration/warped/enable_ref_frame_mvs,
`separate_uv_delta_q`, `seq_force_*`). `mono_chrome`/`num_planes` is the sibling
lane-av1-mono's.

### 9.3 transform.rs:1134 panic — ROOT-CAUSED AND FIXED (not a refusal)

`txbset_for_inter` was a near-duplicate of `inter_txbset_for` whose `4` fell
through to `TxbSet::Luma64`: a 4x4 intrabc/inter TU read 32x32-class tables (a
1024-entry grid) and panicked in `inverse_transform_2d_typed_wh` the first time
such a TU coded a non-zero coefficient (`allintra0`, cpu-used 0). libaom reads a
4x4 grid with the 16-symbol INTER ext-tx set there (`EXT_TX_SET_ALL16` /
`_DTT4_IDTX` reduced). Fix: delete the duplicate, route all inter-table sites
through `inter_txbset_for` (which already had the 4 arms). Result: **no panic**;
`allintra0` now stops at the EXISTING named refusal "an intrabc block whose
var-tx tree resolved to mixed leaf transform sizes" (`decode.rs:14327`) — a
lane-of-its-own gap, not a crash.

### 9.4 warped: four more defects out of the way, still not byte-exact

Ladder evidence (`EC_TRACE_MODE_STEP=1` on the instrumented aomdec vs ours,
rng-ladder subsequence walk, frame 0). First divergence moved
**(38,60) → (38,62) → (46,66)**; consuming steps matched 916 → 1119 → 1349 of
aomdec's 2204 before the next divergence.

1. **Rect intrabc leaves read no transform symbol** (the charter's named stop):
   `block_signals_txsize` is `bsize > BLOCK_4X4` (`blockd.h:1027`), TRUE at
   `BLOCK_4X8`/`BLOCK_8X4` — so an UNSKIPPED intrabc rect leaf reads the INTER
   var-tx tree (`read_tx_size_vartx` over `max_txsize_rect_lookup[BLOCK_4X8] ==
   TX_4X8`), not nothing. Fixed in `decode_leaf_rect8`'s intrabc arm
   (`read_var_tx_size` with the rect entry + per-4x4-TU residual/reconstruction
   when the tree splits, `RECT_INTRABC_VARTX_HITS` counter, probe line
   `rect_intrabc_vartx`). aomdec's `EC_VARTX mi_row=37 mi_col=62 bsize=2
   tx_size=6 ctx=19 rng=37745` is now reproduced exactly.
2. **Sub-8x8 leaves never published the TXFM context bands**: libaom runs
   `set_txfm_ctxs` for EVERY block; `decode_leaf_split4`/`decode_leaf_rect8` had
   no write, so an intrabc block's `txfm_partition_context` read a stale 8 where
   aomdec read 4 (ctx 18 vs 19). Fixed (+ unit bug caught on the way: the rect
   helper takes PIXELS for the parent span, not mi).
3. **`intrabc_dv` slot leak**: `read_intra_mode_sub8` armed the frame slot and
   the sub-8x8 callers never took it, so the NEXT 8x8+ block inherited the
   vector and read an inter var-tx symbol where libaom read an intra
   `tx_depth`. The DV already travels in the return value; the arm is gone.
4. (9.3's table fix also feeds this path: the split rect leaf's 4x4 TUs read
   `Luma4Inter`/`Set1`.)

**Remaining first divergence (frame 0, mi (46,66), 8x8 intra block):**
`tx_depth ctx` ours 0 vs aomdec 1. Root cause, localised: the key-frame
square/leaf tx-depth context is read through `tx_size_context` — the DEBLOCK
grid approximation — while libaom's `get_tx_size_context` (`pred_common.h:342`)
reads the **TXFM bands** with the inter-neighbour block-size override. The
approximation only coincides while every neighbour codes one whole-block
transform, "which is exactly what `TxMode::Select` ends" (the helper's own
doc) — warped's key frame IS `TxMode::Select` with intrabc. The fix is the
lane's next step and is NOT small: `publish_txfm_bands_if_in_inter` returns
early on a key frame, so switching the read to libaom's formula requires
publishing the bands from EVERY key-frame block (strips included) in the same
change, or currently-byte-exact streams can regress. Left unpushed.

Consequences for the rest of the corpus: `allintra` and `warped` both now stop
at `decode.rs:9155` "intra block copy on a HORZ/VERT/1:4 rect intra strip
(reconstruction is not ported at this shape)" — after **16 more mi rows** of
frame 0 than before the fix (was (38,60), now (54,64)). That refusal is lane 3's
and is left standing.

### 9.5 Inventory re-pin (done)

- The sub-8x8-intrabc refusal no longer exists (the capability landed), so its
  `REFUSALS` entry is replaced by a note and its claims-table pair is retired;
  the census gate stays as the non-vacuous premise witness (`reached = 0` now).
- The Golomb entry is RE-PINNED with the measurement: the reader covers the
  whole legal value domain, but the string is reachable as a DESYNC SYMPTOM
  (real monochrome libaom stream, `stream.rs`'s mono tripwire) — the claim pins
  value coverage, not unreachability.

### 9.6 Gates

- `cargo test -p ec-av1 --lib -- <the three new gates>`: 3 passed, 0 warnings.
- Full `cargo test -p ec-av1 --lib` and `cargo check -p ec-av1 --all-targets`
  were launched at the end of this pass; see the commit message / next session
  for their totals (baseline 592/0/60).
- `cargo build --release -p ec-av1 --example decode_probe`: 0 warnings.

### 9.7 Next steps (ordered)

1. Read the key-frame tx-depth context from the TXFM bands (libaom
   `get_tx_size_context`) and publish the bands from every key-frame block —
   then re-run the (46,66) ladder rung and the corpus sweep (§5's tripwires
   included).
2. `allintra`'s 1:4 rect-strip intrabc (lane 3) and the mixed-leaf var-tx
   refusal (the new stop for `allintra0`).
3. Add the 4x8/8x4 intrabc witnesses to the media-gated corpus (a real stream
   that codes one: `warped.obu` reaches `RECT_INTRABC_VARTX_HITS == 1` and now
   decodes past the old divergence).
