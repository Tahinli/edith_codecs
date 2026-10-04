# lane-av1422gather3 — the three chroma gathers `av1muchunk422sym` left outside the pair rule

Base `main` @ `90778367`. Branch `lane/av1422gather3`.

`lanes/av1muchunk422sym.report.md` §6 enumerated three `around_mi_rect`
call sites that "gather a CHROMA plane's coefficient context, feed
`dc_sign_ctx`, lack the pair rule, and belong to a DIFFERENT walk". Each was
to be measured on its own terms. Verdicts, per site:

| site (base line) | function | calls `around_mi_422_chroma` at ss (1,0)? | reached at ss (1,0)? | verdict |
|---|---|---|---|---|
| ~24924 → **24934** | `decode_block_128rect` | **no** (square `around_mi_rect`) | **YES — 72 gathers** on each pinned `422_intrabc_sb128_strip*.obu` | **ROUTED + GATED** |
| ~49552 → **49562** | `decode_intra_sub8_leaf` | no (inside `chroma_444`) | **NO — 0 hits** over the whole 4:2:2 corpus | **CLOSED (unreachable)** |
| ~50400 → **50410** | `decode_inter_sub8_rect2` | no (inside `chroma_444`) | **NO — 0 hits** over the whole 4:2:2 corpus | **CLOSED (unreachable)** |

The first is a real 4:2:2 instance of the class. The other two are stale rows:
the mechanical enumeration keyed on "`around_mi_rect` feeding a chroma context"
and did not notice that both gathers sit inside an `if chroma_444 { … }` arm
(`chroma_444 = ss_x == 0 && ss_y == 0`, `decode.rs` 49359 / 49796), i.e. they
only run at 4:4:4, where `ss_x == ss_y == 0` and the pair rule does not apply.
`decode_inter_sub8_rect2`'s actual 4:2:2 arm already reads
`around_mi_422_chroma(ctx_mi, 8, 4)` (`decode.rs` ~50538).

## 1. Site 1 — `decode_block_128rect` (the intra 128-root rect chroma walk)

The per-chunk chroma-unit walk gathers its coefficient context from
`cu_mi` over the unit's LUMA span (`luma_span = chroma_tx << ss_x = 64`,
`luma_span_h = chroma_tx << ss_y = 32`) and feeds `cu_around[plane_pass]` to
`read_plane` → `dc_sign_ctx(around.2)`. Same rule as the mu-chunk walks: at
ss (1,0) libaom's `get_txb_ctx_general` reads the above votes over
`txb_w_unit = chroma_tx / 4 = 8` CHROMA cells, and one chroma cell spans TWO
luma mi columns, so the square per-mi sum counts each above column twice.
The left extent is unsubsquared at ss_y 0 and already lines up one-to-one.

**Reachability, measured not argued.** A temporary env-free probe at the
gather (printing when `ss_x == 1 && ss_y == 0`) fired **72 times on each of**
`422_intrabc_sb128_strip.obu` and `422_intrabc_sb128_strip_notxsearch.obu`
and **0 times on the other 19 committed 4:2:2 cells** (all `422_*` / `s422_*`).
The 72 is the same walk's own replay counter
(`sb128rect_chroma_replay_hits().0`, whose doc records `replay = 72,
mismatch = 9` on exactly those two cells) — two independent instruments, same
number.

**No committed cell discriminates the change.** With the pair rule in place
every committed 4:2:2 cell was decoded and compared plane-by-plane against
ffmpeg rawvideo:

```text
422_allskip_2f EXACT (2 frames)          422_sb128_3f EXACT (3)
422_inter_160x128_3f EXACT (3)           s422_12bit_160x128 EXACT (3)
422_intrabc_sb128_strip EXACT (5)        s422_320x246/322x240/322x246 EXACT (16)
422_intrabc_sb128_strip_notxsearch EXACT (5)  s422_352x242_10b EXACT (16)
422_key_64x64 EXACT (1)                  s422_416x242_10b/416x250_10b EXACT (16)
422_muchunk_compound_128root EXACT (6)   s422_grain_160x128 EXACT (3)
422_muchunk_intra_in_inter_128root EXACT (6)  s422_superres_160x128 EXACT (3)
422_palette_intra_in_inter_384x240_17f EXACT (16)
422_residual_compound_warp_16f EXACT (16)
422_residual_compound_warp_nolr_16f EXACT (16)
(422_header_edge16_walk refuses by design — not a decode cell)
```

The pre-change tree was equally exact (every one of these is a pinned
byte-exact gate on `main`, and the strip fixture was re-decoded pre-change
here: f0..f4 all EXACT). So the gathered votes cancel at every reached unit:
the routing is **correct but output-inert on the committed corpus**, exactly
the disposition the same file already gives the IntraBC twin
(`the_intrabc_128rect_chroma_chunk_walk_stays_per_axis`, "that fixture's own
votes do not cancel … no committed cell discriminates").

Because no pixel gate can see it, the change is pinned by a **source-scan
routing gate**, `the_422_block_128rect_chroma_chunk_gather_stays_per_axis`,
which additionally carries a **reachability arm** (decodes both pinned strips
and asserts exactly 72 gathered units, so the routing can never be a no-op the
scan cannot see — class `gate-blind-to-feature`).

### Red-before (mutation), measured

Two mutations, each built and run:

1. gather reverted to the bare square call (guard removed) →
   `the ss (1, 0) guard over the per-chunk chroma gather must appear exactly
   once in decode_block_128rect, found 0` — RED.
2. guard kept, pair rule replaced by the square gather in the `if` arm →
   `neighbours.around_mi_422_chroma(cu_mi, luma_span, luma_span_h) must appear
   exactly once … found 0 -- one arm has been dropped, so the gather has
   fallen back to the square per-mi rule at ss (1, 0)` — RED.

Reverted; green again.

## 2/3. Sites 2 & 3 — the `chroma_444`-scoped gathers (closed)

- `decode_intra_sub8_leaf` (49582): the `let ca = around_mi_rect(lmi, bw, bh)`
  that feeds `read_chroma` sits in the `else` of `if lossless(fctx)` inside
  `else if chroma_444 && bw != bh`. `chroma_444` is `ss_x==0 && ss_y==0`
  (`decode.rs` 49359).
- `decode_inter_sub8_rect2` (50434): the `let ca = around_mi_rect(lmi, bw, bh)`
  that feeds `read_inter_plane_rect` for U/V sits in the `else` of
  `if lossless(fctx)` inside the `if chroma_444` block (`decode.rs` 49796).

Both were probed at ss (1,0) across the entire committed 4:2:2 corpus:
**0 hits each** (matching the static guard). They are 4:4:4-only; at 4:4:4 the
pair rule is inapplicable (`ss_x == ss_y == 0`, one luma mi per chroma cell).
`decode_inter_sub8_rect2`'s 4:2:2 per-piece arm already reads through
`around_mi_422_chroma`. Both comments now record the guard so a future
mechanical enumeration does not re-list them.

## 4. Also corrected (same function, stale prose)

`decode_block_128rect`'s `SB128RECT_REPLAY_SPAN_MISMATCH_HITS` comment still
read "4:2:2 … is refused by name at the sequence header, so the arm is
unreachable there". 4:2:2 is admitted (`lane-av1422lift`) and the arm fires
(mismatch 9 on the two strip cells, byte-exact). The comment now says so and
states that nothing asserts the mismatch is zero.

## 5. Evidence summary

- Site 1 fix: `decode.rs` 24946–24950 (guard + `around_mi_422_chroma`).
- Site 2/3: comments only (`decode.rs` 49578, 50430) — no code change, closed
  as unreachable.
- Gate: `stream.rs::the_422_block_128rect_chroma_chunk_gather_stays_per_axis`
  (source scan + 72-unit reachability arm), mutation-proven red twice.
- Pinned fixtures: `422_intrabc_sb128_strip.obu` (1672 B, sha256 prefix
  `f92000db86df7acc`), `422_intrabc_sb128_strip_notxsearch.obu` (1675 B,
  `80dd0d4e93fd6c3d`).

## 6. Repro

```text
git worktree add -b lane/av1422gather3 ~/.cache/wt/av1422gather3 90778367
cd ~/.cache/wt/av1422gather3
CARGO_TARGET_DIR=~/.cache/tgt-av1422gather3 cargo build -p ec-av1 --example dump_yuv
B=~/.cache/tgt-av1422gather3/debug/examples
$B/dump_yuv crates/ec-av1/fixtures/422_intrabc_sb128_strip.obu /tmp/ours   # 384x320 yuv422p
ffmpeg -v error -i crates/ec-av1/fixtures/422_intrabc_sb128_strip.obu -pix_fmt yuv422p \
  -f rawvideo /tmp/ref.yuv
TMPDIR=/dev/shm CARGO_TARGET_DIR=~/.cache/tgt-av1422gather3 \
  cargo test -p ec-av1 --lib -- the_422_block_128rect_chroma_chunk_gather_stays_per_axis
```
