# lane-av1muchunk422sym — the first wrong SYMBOL of `422_muchunk_compound_128root.obu`

Base: `main` @ `df681fb7`. Branch: `lane/av1muchunk422sym`.

**Verdict: the first wrong symbol is the DC SIGN of the V-plane unit at mu chunk
`(cr = 1, cc = 1)` of decode frame 2 — the last unit pair of the compound
`side > 64` mu-chunk walk. The context was chosen by `Neighbours::around_mi_rect`
(`decode.rs` 10623) as called by that walk: at ss (1, 0) it reads the unit's
above dc vote over 16 LUMA mi cells where libaom's `get_txb_ctx_general` reads
`txb_w_unit` = 8 CHROMA cells, doubling the above vote and flipping
`dc_sign_ctx`'s signum. FIXED, GATED, MUTATION-PROVEN.**

## 1. The first wrong symbol, measured not argued

Paired our `EC_TRACE_COEFF` / `EC_ECDUMP_IN` / `EC_DCDUMP` ladders (decode frame
2, via `EC_TRACE_COEFF_FRAME=2`) against the instrumented oracle's
(`~/.cache/aom-oracle/build/aomdec`, `EC_TRACE_COEFF` + `EC_ECDUMP_IN` +
`EC_ECDUMP`), unit-for-unit, on `(tag, symbol value, rng)` — the ECIN `(value,
rng)` pair matched on every unit up to the fork (the oracle's third field is a
different bit-position convention, off by a constant).

Every unit and every symbol agrees until:

| | ours | oracle |
|---|---|---|
| unit | plane V, `bc=8 br=16 tx=3` (chunk `(1,1)`, mi `(16,16)`) | same |
| symbol | `tag=sign c=0` (the DC sign) | same |
| value | `sign=0` | `sign=0` |
| **context** | **`dc_sign_ctx = 1`** (`dc_sign_cdf[1]`) | **`dc_sign_ctx = 0`** (`dc_sign_cdf[0]`) |
| `rng` after | `62776` | `37548` |

Both decoders read the SAME sign value from DIFFERENT CDF rows, so the entropy
state forks at that read. That is exactly the "same magnitudes, different
signs" the diagnostic lane measured in the dequantised grid, and it is why Y
and U (and every earlier chunk of V) stayed bit-exact.

The choosing functions, bottom-up:

* `decode_inter_block`'s compound mu-chunk walk, `decode.rs` 43927 (pre-fix):
  `let cu_around = neighbours.around_mi_rect(unit_mi, unit_luma_w, unit_luma_h);`
* `Neighbours::around_mi_rect`, `decode.rs` 10623 — sums `dc_vote` over
  `unit_luma_w / MI = 64 / 4 = 16` above cells.
* `read_inter_plane`, `decode.rs` 39532 — `dc_sign_ctx(around.2)`.

Measured at that unit (our `EC_DCDUMP mi=(16,16) plane=2 wh=(64,32)`):
`above` = 16 cells all `Some(true)` → `-16`; `left` = 8 cells all `Some(false)`
→ `+8`; vote `-8` → ctx 1. libaom's sum is one chroma cell per PAIR of luma mi
columns (ss_x 1): `-8 + 8 = 0` → ctx 0, which is the oracle's row.

## 2. Why that gather is the 4:2:2 pair rule and not a geometry error

`get_txb_ctx_general` (oracle `av1/common/txb_common.h`) walks
`tx_size_wide_unit[TX_32X32] = 8` cells of `pd->above_entropy_context`, which is
indexed in the CHROMA plane's own 4-px units (`av1_set_entropy_contexts` writes
`blk_col + k` for `k < txb_w_unit`). `decode_token_recon_block` enters the
chroma unit at `blk_col = col >> ss_x`, so those 8 cells are chroma columns
8..15 — 8 cells, not 16. Our single luma-mi-indexed array
(`record_mi_chroma` stamps the unit's whole 16-luma-mi-column span, both
columns of each chroma column carrying the same whole-unit dc sign) therefore
counts each above column twice. `around_mi_422_chroma` samples every second
above cell, which IS libaom's sum — the same rule the file already applies at
`decode.rs` 13169 / 19810 / 16699.

The walk's ADDRESSING is not implicated (the diagnostic lane proved it); only
the context gather.

## 3. Fix

Route the mu-chunk chroma gather through the pair rule when ss (1, 0), luma and
every other subsampling shape keeping the plain rect gather verbatim:

```rust
let cu_around = if ss_x(fctx) == 1 && ss_y(fctx) == 0 {
    neighbours.around_mi_422_chroma(unit_mi, unit_luma_w, unit_luma_h)
} else {
    neighbours.around_mi_rect(unit_mi, unit_luma_w, unit_luma_h)
};
```

Three source copies, all the same walk:

1. `decode_inter_block` compound mu-chunk walk (`decode.rs` ~43927);
2. `decode_inter_block` single-reference twin (~45730) — same source text,
   same rule; its own votes do not cancel on this fixture (decode frame 1 was
   already exact), so it is a measured-reachable latent instance of the class;
3. `decode_intrabc_128rect`'s chroma chunk walk (~15435) — the IntraBC twin of
   the same walk. Reachability MEASURED, not assumed: `EC_IBC128_BANDS` prints
   at ss (1, 0) on the committed `422_intrabc_sb128_strip.obu` (96 lines).
   That fixture's own votes do not cancel either, so no committed cell
   discriminates the change; its source-scan gate
   `the_intrabc_128rect_chroma_chunk_walk_stays_per_axis` now pins the routing
   instead (`neighbours.around_mi_422_chroma(` and the guard literal, once
   each), so it cannot be silently reverted.

At 4:2:0 (ss 1/1) and 4:4:4 the guard is false and nothing changes; the
pre-existing 4:2:0 exactness is explained by the same arithmetic — there BOTH
extents double (`2a + 2l`), so the signum is untouched.

## 4. Evidence

`ffmpeg -i X.obu -pix_fmt yuv422p -f rawvideo` (6 shown frames), ours before vs
after, differing bytes per shown frame (Y / U / V):

| frame | 0 | 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|---|
| before | 0 | V 2063 | V 2138 | V 2105 | V 1988 | 0 |
| after | 0 | 0 | 0 | 0 | 0 | 0 |

Instrumented aomdec `EC_AV1_FINAL_DUMP` in DECODE order (7 frames = 6 shown + 1
hidden alt-ref), ours vs oracle, after the fix: **f0..f6 all EXACT**.

## 5. Gate + mutation proof

`a_422_muchunk_compound_128root_pinned_stream_reaches_its_unit_walk` now carries
BOTH halves (the diagnostic lane's hand-over: "when a fix lane lands, add the
oracle compare to this test"):

* the counter half, untouched (compound units ≥ 32, intra-in-inter 0);
* an oracle-free pinned half that CANNOT silently skip: `fnv1a64` of shown
  frame 2's V plane must be `0xa0e58bf4207b29ba` (pre-fix
  `0xc4e7fdb873433479`) and `V[3876]` (chroma (36, 60), the first sample the
  diagnostic lane measured) must be `112` (pre-fix 113); shown frame 0's V
  plane is pinned as the unaffected control (`0xcc5e329119159024`);
* `decode_all_frames_vs_oracle` for the 7-decode-order-frame compare when an
  oracle is present.

Red-before, measured: restoring the square gather at the COMPOUND site only
(`git`-restorable edit) and re-running produced

```text
panicked at crates/ec-av1/src/stream.rs:42992:
  422_muchunk_compound_128root.obu shown frame 2's V plane moved ...
  left: 14188588119703630969   right: 11593826696368433594
```

`14188588119703630969 = 0xc4e7fdb873433479`, byte-for-byte the pre-fix hash.
Reverted, green again.

Scoped regression runs, all green with `EC_AV1_AOMDEC` pointed at the
instrumented oracle: the three touched gates, the 20 `422`-named tests, and the
`chroma` (64), `rect`, `intrabc`, `sb128` filters.

## 6. Residue — chroma rect gathers still outside the pair rule

Enumerated mechanically (46 `around_mi_rect` call sites in `decode.rs`); the
ones that gather a CHROMA plane's coefficient context, feed `dc_sign_ctx`, lack
the pair rule, and belong to a DIFFERENT walk than the one this lane's symbol
named — each needs its own fixture-level discrimination and reachability
measurement, so this lane did not touch them:

| `decode.rs` | function | walk |
|---|---|---|
| ~24924 | `decode_block_128rect` | intra 128-root rect per-chunk chroma walk |
| ~49552 | `decode_intra_sub8_leaf` | sub-8 intra leaf chroma rect |
| ~50400 | `decode_inter_sub8_rect2` | sub-8 inter rect chroma |

Sites inside `sub8_leaf_chroma444` (~26798, ~26889) are 4:4:4-scoped (ss 0/0),
where the rule does not apply. Every other `around_mi_rect` site is luma
(`[0]`) or already routes through `around_mi_422_chroma`.

## 7. Repro

```text
git worktree add -b lane/av1muchunk422sym ~/.cache/wt/av1muchunk422sym df681fb7
cd ~/.cache/wt/av1muchunk422sym
CARGO_TARGET_DIR=~/.cache/tgt-av1muchunk422sym cargo build -p ec-av1 --example dump_yuv
B=~/.cache/tgt-av1muchunk422sym/debug/examples
EC_TRACE_COEFF=1 EC_TRACE_COEFF_FRAME=2 EC_ECDUMP_IN=1 EC_DCDUMP=1 EC_DQCOEFF=1 \
  $B/dump_yuv crates/ec-av1/fixtures/422_muchunk_compound_128root.obu /tmp/ours 2>ours.trace
EC_TRACE_COEFF=1 EC_ECDUMP_IN=1 EC_ECDUMP=1 EC_DQCOEFF=1 \
  ~/.cache/aom-oracle/build/aomdec --codec=av1 -o /tmp/o.y4m \
  crates/ec-av1/fixtures/422_muchunk_compound_128root.obu 2>oracle.trace
# pair on (tag, value, rng): first fork is tag=sign c=0 of the V unit bc=8 br=16
TMPDIR=$HOME/work-422sym/tmp EC_AV1_AOMDEC=~/.cache/aom-oracle/build/aomdec \
  cargo test -p ec-av1 --lib -- 422
```

Fixture: `crates/ec-av1/fixtures/422_muchunk_compound_128root.obu`, 895 bytes,
fnv1a64 `0xfb28f9a7cf239897`, sha256
`9942bae279a60ba6fd4d35ba55469271317b655d5ea03158125deb4428e26ae7`,
128x128 4:2:2 (ss 1/0), 128-superblock, one inter block per frame.
