# lane-av1422ibctxsel — the ss (1, 0) non-split chroma gather of
# `decode_intrabc_owned_rect` is DISCRIMINATED by a pinned witness, routed
# through the pair rule, and gated red-before/green-after

Base `main` = `6d646f65` (branch `lane/av1422ibctxsel`, worktree
`~/.cache/wt/av1422ibctxsel`). Local only: no push, no merge. The fix commit
is `b5e034d4` (source + fixture + gate); this report is a second commit.

## Verdict

**Witnessed and fixed.** A real aomenc 4:2:2 stream enters the non-split
chroma gather of `decode_intrabc_owned_rect` **24 times** and, on **five** of
those entries (plane 2, strips mi (64,16)..(64,80) of frame 0), the two
gathers disagree: `dc_sign_ctx` **1** through the luma-span `around_mi_rect`
vs **0** through libaom's pair rule. Pre-fix that wrong context desyncs the
tile and **no frame decodes at all** (the decoder refuses mid-frame); post-fix
the pinned bytes are **byte-exact against ffmpeg on 4/4 frames**, with the
route counter reading 24. Both non-split chroma arms of the function are
routed under the `ss_x == 1 && ss_y == 0` guard; every other subsampling keeps
the plain gather verbatim. The witness is committed and pinned (size +
fnv1a64 + sha256) and the gate is mutation-proven (route reverted → red).

## 1. The class, and the exact arithmetic a witness has to flip

Both residual branches of `decode_intrabc_owned_rect` (`leaves.is_some()` —
the var-tx tree — and its `leaves == None` twin) read their **chroma**
coefficient context from `neighbours.around_mi_rect((mi_r, mi_c), bw, bh)` —
the block's **luma** span — and hand `around[1]`/`around[2]` to
`read_inter_plane_rect` planes 1 and 2 (`decode.rs:18037→18045/18062` in the
tree branch, `18076→18125/18142` in the twin, base line numbers). At ss
(1, 0) one chroma 4-px cell spans TWO luma mi columns, so the per-mi sum
counts each chroma column twice; libaom's `get_txb_ctx_general` reads one
vote per CHROMA cell, which is `around_mi_422_chroma`'s every-second
sampling (`decode.rs:10555`).

`dc_sign_ctx(vote)` is `vote.signum()` → {0 zero, 1 negative, 2 positive}
(`decode.rs:8254`), so a witness needs the **sign** to differ:

* full gather = `2A + L`; pair rule = `A + L`, where `A` = the pair-rule
  above sum and `L` = the left sum (identical in both).
* Chroma units at 4:2:2 are ≥2 luma mi wide and even-mi aligned, so the
  above part obeys **`full_above = 2 × pair_above` ALWAYS** — the above
  votes alone can never disagree in sign. The asymmetry lives in `L`: a flip
  needs `A` and `L` of **opposite sign with |L| ∈ [|A|, 2|A|)**.
* With one 32x32 chroma TU per strip (the non-split arm's unit), `L = 8 ×
  (previous strip's unit sign)` ∈ {+8, −8, 0} and `A = Σ (W_u/2) s_u` over
  the units covering the row above. The sharpest case is `A = ±8, L = ∓8`:
  `full = ±8` (a sign) vs `pair = 0` (ZERO) — a `dc_sign_ctx` 2-vs-0 or
  1-vs-0 split.

This is also why `lane-av1422ibcrect`'s A1 measured 0/24: on that stream all
above votes shared one sign (and `A = 0` for strips 1..5), so no arithmetic
could flip.

## 2. Encoder facts the hunt established (libaom source in `~/.cache/aom-oracle/src`)

* **`tx_mode` is downgraded to `TX_MODE_LARGEST` when the frame splits no
  transform**: `encodeframe.c:2690` — `if (features->tx_mode == TX_MODE_SELECT
  && cpi->td.mb.txfm_search_info.txb_split_count == 0) features->tx_mode =
  TX_MODE_LARGEST;`. A1's smooth sine content never splits → `txsel=false`
  (the ibcrect report's observation) even with `--enable-tx-size-search=1`.
  **A high-frequency patch makes `txb_split_count > 0` and the header bit
  stays SELECT** (E5+ below) — so the ticket's "vary tx-mode" lever is this.
* **The intraBC hash is LUMA-ONLY**: `hash_motion.c:398` `av1_get_block_hash_
  value(..., const uint8_t *y_src, ...)` hashes 2x2 Y sub-blocks of
  `cpi->source`; chroma is not in the key. Chroma offsets therefore cannot
  break a luma match — but they do enter the DV's RD cost (E4's −20 strip
  band made the encoder drop intraBC on 4 of 6 strips).
* **The geq source runs in the 4:2:0 domain** (`nullsrc … format=yuv420p`),
  so a band written in "luma rows" silently never fires for chroma: E2
  changed NOTHING (bit-identical output to A1, same sha256). The band must be
  expressed in 4:2:0 chroma rows (luma row `y` = chroma row `y/2`).

## 3. The attempt table (8 aomenc encodes, all ≤180 s; budget was ≤12)

Common flags (all rows): `--codec=av1 --i422 --profile=2 --passes=1
--end-usage=q --cpu-used=0 --threads=1 --row-mt=0 --lag-in-frames=0
--kf-max-dist=1 --tile-columns=0 --sb-size=64 --min-partition-size=32
--max-partition-size=64 --enable-rect-partitions=1 --enable-ab-partitions=1
--enable-1to4-partitions=0 --enable-palette=0 --tune-content=screen
--enable-intrabc=1 --deltaq-mode=0 --enable-tx-size-search=1 --obu`,
384x288, 4 frames, from a `geq` source; A1's base content is
`lum=128+90*sin((X+2N)/37)+30*sin(Y/29) cb=128+30*sin(X/23)
cr=128+30*sin((Y+N)/19)`.

| id | delta over A1 | bytes | sha256 | arm | txsel | entries | ctx diffs |
|---|---|---|---|---|---|---|---|
| **A1** control | none (cq 32) | 1244 | `fd36bae4…b397db` | lar twin | false | 24 | **0** |
| E2 | `cr += 30*if(between(Y,192,255))` (luma rows — never fires) | 1244 | `fd36bae4…` (same) | lar | false | 24 | 0 |
| E2b | same band written in 4:2:0 chroma rows (96..127) | 1203 | `3bf9709a…3b1e54` | lar | false | 24 | 0 (above `A=+8`, strips `+8`) |
| E3 | band `−30` (chroma rows 96..127) | 1312 | `77d58313…077306` | lar | false | 11 | 0 (above mostly `None`) |
| E4 | E2b plus strip band `−20` (chroma rows 128..143) | 1339 | `fc68088a…8c3415b` | lar | false | 5 (intraBC survived on 2 of 6 strips) | 0 |
| E5 | luma `+40*sin(X*π/2)` patch, rows 0..31 x<64 | 2173 | `f4495f32…043c26f5e` | lar | **true** | 18 | 0 |
| E6 | E5 + `cr += 25*if(Y in 0..31) + 30*if(Y in 96..127)` + same luma patch inside strip 0's band | 3226 | `8d1bc1ec…e64459166` | lar | true | 17 | **4** (1 visible before the pre-fix refusal aborted the decode) |
| **E7** witness | E6 minus the strip luma patch | 2170 | `4f82d5da8b4c3d6553953fc373001a1c7a729c9b189214b65503af1e04eb6b68` | lar | true | 24 | **5** |

E7's five discriminating entries (probe print, pre-fix):

    EC_IBCNS422P lar mi=(64,16) … ss=(1,0) txsel=true plane=2 fv=-8 pv=0 fctx=1 pctx=0 diff=1
    EC_IBCNS422P lar mi=(64,32) … plane=2 fv=-8 pv=0 fctx=1 pctx=0 diff=1
    EC_IBCNS422P lar mi=(64,48) … plane=2 fv=-8 pv=0 fctx=1 pctx=0 diff=1
    EC_IBCNS422P lar mi=(64,64) … plane=2 fv=-8 pv=0 fctx=1 pctx=0 diff=1
    EC_IBCNS422P lar mi=(64,80) … plane=2 fv=-8 pv=0 fctx=1 pctx=0 diff=1

i.e. `A = −8` (above uniform, the `+30` band) and `L = +8` (each strip's own
32x32 V unit is positive) → `full = 2A + L = −8` (negative, ctx 1) vs
`pair = A + L = 0` (zero, ctx 0). Five strips, frame 0 (frame 0 is the one
whose `cr` phase puts the strip residual positive).

The entry/diff columns above were re-measured in one pass with the temporary
probe (the probe prints the raw gather vs the pair rule explicitly, so its
`diff` is independent of the route the decoder currently takes). E7's 24 is
corroborated by a second, committed instrument: the route counter
`IBC_OWNED_RECT_422_PAIR_HITS` reads exactly 24 in the gate.

The probe (`EC_IBCNS422P`, env-gated, both arms, printing the luma-span
gather vs the pair rule) was **removed before the commit** — no probe and no
`EC_AV1_ALLOW_422_PROBE` remains in the tree.

## 4. The fix

`decode.rs`, `decode_intrabc_owned_rect` only:

* tree branch: `let around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
  hit!(IBC_OWNED_RECT_422_PAIR_HITS); neighbours.around_mi_422_chroma(…) }
  else { neighbours.around_mi_rect(…) };` (plane 0 unused in this branch);
* twin branch: the same gate on a NEW `around_chroma` used by planes 1/2;
  `around` stays the plain luma-span read that plane 0's own context uses
  (`ss_x == 0` / `ss_y != 0` keep it for chroma too, verbatim);
* new counter `IBC_OWNED_RECT_422_PAIR_HITS` + accessor/reset
  (`ibc_owned_rect_422_pair_hits`), in the crate's `hit!` pattern.

The census's stale comment at the tree branch ("UNWITNESSED", "the only
`around_mi_rect` …") was replaced by the dated routing comment; it also
records the honest residue: the tree branch itself still has **no** stream
that splits a transform on the strip (`TX_MODE_SELECT` + `txfm_partition` on
the 64x32 strip) — the witness enters the twin, and both branches share the
same routed code.

## 5. Evidence

* **Gate**: `a_422_intrabc_owned_rect_nonsplit_chroma_context_uses_the_pair_rule`
  (`stream.rs`) — pins size 2170 + fnv1a64 `0x92e2c4e45ca27e34` + sha256,
  asserts `assert_422_stream_pixel_exact(…, 384, 288, 4, 8, true)` (ffmpeg
  `yuv422p`, per-plane coefficient-unit census deltas
  `units=[149, 219, 225] coded=[126, 27, 98]`, all non-zero), asserts the
  route fired exactly **24** times, and asserts the 4:4:4 `r512.obu` arm
  fires it **0** times (guard-does-not-leak arm).
* **Red-before (mutation proof)**: with both routed conditions forced
  `if false` (temporary, reverted), the gate FAILS on the same pinned bytes:
  `decode_stream refused a real 4:2:2 stream: … a block size 4x8, 8x16 or
  16x4 … has no chroma plane block at this frame's subsampling mode`.
* **Green-after**: 4/4 frames byte-exact vs ffmpeg; counter 24; gate + the
  422 `sb128_strip` gate + `a_16x4_intrabc_pair_strip_decodes_pixel_exact`
  (4:2:0) + `a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes`
  (4:4:4) all pass in one scoped run (`4 passed; 846 filtered out`,
  1.8 s). Scoped tests only; no full suite.
* `cargo check -p ec-av1` clean (the one pre-existing `pin_reporting`
  dead-code warning, unrelated).

## 6. Findings for other lanes (measured here, not fixed here)

1. **The pre-fix refusal on E6/E7 was a DESYNC SYMPTOM, not a real
   sub-8x8 shape.** Pre-fix, our decoder refuses those bytes mid-frame while
   **both oracles decode them fully** (`aomdec --rawvideo` → exit 0,
   884736 B = 4×221184; ffmpeg → 884736 B). Post-fix (same bytes, route
   fixed) the refusal is gone and the decode is byte-exact. So the wrong
   `dc_sign_ctx` → wrong dc-sign symbol → desync → the subsize guard fires on
   garbage. The refusal string itself is libaom's
   `decodeframe.c:1456` rule, but its trigger here was ours. `lane-av1422ibcrect`'s
   A2/A4 refusals were NOT re-examined by this lane; their streams were not
   measured against aomdec here.
2. **`scripts/fixture-library.tsv` needs a canonical-environment
   regeneration** (deferred, unblock: run
   `scripts/gen-fixture-library.sh` where `$ROOT/fixtures` is the real
   library). Measured: regenerating on a **pristine `git archive` of
   `6d646f65` with no edits** already differs from the committed manifest by
   651 diff lines here (e.g. "212" vs "48" unresolved media literals,
   double-slash `fixtures//…` rows), and even with
   `EC_FIXTURES=<primary>/fixtures` the output is not idempotent with HEAD's
   file (dropped `encode.rs` rows, unreferenced-pin count 4→6). So this lane
   does NOT commit manifest churn; the new fixture's row (measured:
   `crates/ec-av1/fixtures/ibc422_nonsplit_ctx.obu  crates/ec-av1/src/stream.rs:3587 … 4f82d5da…`)
   and the stream.rs line shifts will land with that regeneration.
3. Process note (recorded for truthfulness): during the mutation proof a
   bulk in-place replace of the routed-condition line matched a THIRD,
   pre-existing site in another function (`around_mi_422_chroma(pair_mi, pw, ph)`
   at base `19863-19872`). It was restored exactly and verified: the final
   `git diff` hunks touch only statics/accessors and
   `decode_intrabc_owned_rect` in `decode.rs`, and that function's text is
   byte-identical to HEAD outside the intended hunks.

## 7. Repro

```text
git worktree add -b lane/av1422ibctxsel ~/.cache/wt/av1422ibctxsel 6d646f65
cd ~/.cache/wt/av1422ibctxsel

# witness (E7): aomenc = ~/.cache/aom-oracle/build/aomenc
ffmpeg -v error -f lavfi -i "nullsrc=size=384x288:rate=25,format=yuv420p,\
 geq=lum='128+90*sin((X+2*N)/37)+30*sin(Y/29)+40*sin(X*1.5707963)*(lt(X,64)*lt(Y,32))':\
 cb='128+30*sin(X/23)':\
 cr='128+30*sin((Y+N)/19)+25*if(between(Y,0,31),1,0)+30*if(between(Y,96,127),1,0)'" \
 -frames:v 4 -pix_fmt yuv422p -strict -1 -f yuv4mpegpipe -y E7.y4m
aomenc --codec=av1 --i422 --profile=2 --passes=1 --end-usage=q --cpu-used=0 \
 --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=1 --tile-columns=0 \
 --sb-size=64 --min-partition-size=32 --max-partition-size=64 \
 --enable-rect-partitions=1 --enable-ab-partitions=1 --enable-1to4-partitions=0 \
 --enable-palette=0 --tune-content=screen --enable-intrabc=1 --deltaq-mode=0 \
 --enable-tx-size-search=1 --obu --cq-level=32 --limit=4 -o E7.obu E7.y4m
sha256sum E7.obu   # 4f82d5da8b4c3d6553953fc373001a1c7a729c9b189214b65503af1e04eb6b68

# gate (green)
CARGO_TARGET_DIR=~/.cache/tgt-av1422ibctxsel \
 cargo test -p ec-av1 --lib a_422_intrabc_owned_rect_nonsplit_chroma_context_uses_the_pair_rule
```

`E7` = `crates/ec-av1/fixtures/ibc422_nonsplit_ctx.obu` (byte-identical).
