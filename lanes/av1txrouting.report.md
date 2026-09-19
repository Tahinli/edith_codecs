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
