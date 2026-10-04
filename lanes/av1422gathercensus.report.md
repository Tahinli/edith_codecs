# lane-av1422gathercensus — every chroma coefficient-context gather in
# `decode.rs` that can run at ss (1, 0) without `around_mi_422_chroma`

Base `main` @ `2ca9e4e9` (branch `lane/av1422gathercensus`).

The class: a gather that feeds a CHROMA plane's coefficient context
(`read_plane` / `read_inter_plane` / `read_inter_plane_rect` /
`read_rect_chroma_unit` / `read_coeffs*` plane 1|2 → `dc_sign_ctx`), spelled
with a LUMA extent, at ss (1, 0). One chroma 4-px cell spans TWO luma mi
columns, so a per-luma-mi sum counts each chroma column twice — libaom's
`get_txb_ctx_general` reads one vote per CHROMA cell
(`Neighbours::around_mi_422_chroma` is the pair rule). The prior sweep
(`lanes/av1422gather3.report.md`, `lanes/av1muchunk422sym.report.md` §6)
enumerated `around_mi_rect` call sites only; two bare `around_mi` instances
were fixed after (`decode_block_128rect` 4:2:2 mu-chunk gather; the
intra-in-inter side>64 gather at ~46834). This lane closes the enumeration.

## Method

A full mechanical pass over EVERY `neighbours.around(` / `around_mi(` /
`around_mi_rect(` / `around_rect(` call site in `crates/ec-av1/src/decode.rs`
(98 call sites, plus the 5 internal wrappers in the `Neighbours` impl), each classified by its enclosing function, whether the
result's plane 1|2 is consumed (the chroma indicator), the guard it sits
under, and the span passed. Guards confirmed by reading the branch
structure, not by the call's spelling.

## Per-site verdicts (the class = chroma consumer, non-trivial luma span)

| site (main line) | function | guard / span | reached at ss (1,0)? | verdict |
|---|---|---|---|---|
| **18022 (U), 18061 (V)** | `decode_intrabc_owned_rect` | none; above span = block LUMA `bw` | structurally yes; **0 hits** on the committed 4:2:2 corpus | **STOP** — unwitnessed (this report) |
| 28256, 28309 | `decode_leaf_split4` | `else` of `chroma_444`/`chroma_422` → 4:2:0 group | **no** (branch requires `!chroma_422`) | closed — 4:2:0-only |
| 29444, 29498 | `decode_leaf_rect8` | same 4:2:0 group arm | **no** | closed — 4:2:0-only |
| 48830 | `decode_inter_sub8_split4` | `if !chroma_444 && !chroma_422` group arm | **no** | closed — 4:2:0-only |
| 50860 | `decode_inter_sub8_rect2` | 4:2:0 group arm | **no** | closed — 4:2:0-only |
| 44249, 46085, 47149 | `decode_inter_block` | `if ss_x == 0 && ss_y == 0 && chroma_side > chroma_tx` | **no** | closed — 4:4:4-only |
| 26832, 27053 | `sub8_leaf_chroma444` | function is the 4:4:4 arm | **no** | closed — 4:4:4-only |
| 49546, 49612 | `decode_intra_sub8_leaf` | inside `if chroma_444` | **no** | closed — 4:4:4-only (prior lane, re-confirmed) |
| 49665 | `decode_intra_sub8_leaf` | `if chroma_444 {..} else {group}` | **no** | closed — 4:2:0/4:4:4 |

Already routed through the pair rule (unchanged, listed for completeness):
13170, 13304, 13507, 14765, 14903, 15442, 16136, 16711, 17322, 18268,
18800, 18872, 19777, 19820, 20497, 23860, 23927, 24285, 24957, 25668,
25928, 27458, 31992, 43713, 43732, 43981, 45503, 45513, 45568, 45811,
46835, 47219, 47270, 47412, 48622, 50652, 52005, 52969, 53609.

Provably equivalent to the pair rule (single chroma cell — `step_by(2)` over
a 1-cell extent is the same cell): 19588, 39663, 39851, 48434 (all
`around_mi(pos, MI|B4)`). No change needed at any ss.

Luma-only (`[0]` / plane 0), out of class: the remaining ~60 call sites.

## The one remaining site — measured

`decode_intrabc_owned_rect` (main 17800) gathers its chroma context with
`around_mi_rect((mi_r, mi_c), bw, bh)` — the block's LUMA `bw`/`bh` — feeding
`read_inter_plane_rect` planes 1 and 2 (main 18022 / 18061). It runs only in
the `else` of `if cw.min(32) != cw || ch.min(32) != ch`, i.e. for a
non-split chroma plane block (`cw, ch <= 32`), and only when `!skip` and
`leaves.is_none()` (non-lossless) — the `if skip` and lossless arms take
other paths. So at ss (1, 0) a non-skipped lossy intrabc-owned 64x32 / 32x64
(or smaller) rect strip reaches it with a double-counted above vote.

**Reachability, measured (temporary env-gated probe, removed before commit).**
`EC_IBC422GATHER` printed at that gather for every call; `EC_HALVSWEEP`
(same function, pre-existing) printed at function entry.

- Over the whole committed 4:2:2 gate set — all 24 tests whose name contains
  `422` (`cargo test -p ec-av1 --lib 422`, 21 decoding cells incl. both
  `422_intrabc_sb128_strip*.obu`, plus the two named refusal cells and the
  two oracle-helper refusal tests) — `EC_IBC422GATHER` fired **0 times**.
  `EC_HALVSWEEP` also fired **0 times**: no committed 4:2:2 fixture enters
  `decode_intrabc_owned_rect` AT ALL (the 4:2:2 intraBC strips are
  128-root, handled by `decode_block_128rect`'s own intrabc arm, not this
  function).
- Probe liveness (that the instrument can fire): `EC_HALVSWEEP` printed 5
  entries on the committed 4:4:4 owner-rect cell `r512.obu`
  (`a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes`),
  so the function is entered by a committed fixture at other subsamplings.
  Across the same `EC_IBC422GATHER` runs (the 24 `422`-named tests plus every
  test whose name contains `intrabc`) no non-split entry fired: the
  4:2:0/4:4:4 intrabc rect fixtures that hit this function keep `skip = true`
  (`a_16x4_intrabc_pair_strip_decodes_pixel_exact`: `ibc_owned bw=64 bh=32
  skip=true`), and the lossless r512 entries are all in the split arm.

No encode was needed (and none was attempted): the branch requires a
non-skipped lossy intrabc rect strip, which none of the committed 4:2:2
cells codes.

## Disposition

- **STOP.** The site is structurally reachable at ss (1, 0) but UNWITNESSED:
  0 of the 21 committed 4:2:2 decoding cells enters it. Per the ticket, no
  route and no pixel gate are added — a routing with no witness cannot be
  discriminated by any pixel compare (the same disposition the sibling
  sites took, e.g. `lanes/av1422gather3.report.md` §2/3 for the
  `chroma_444`-scoped pair).
- A dated comment at the site records the verdict so a future mechanical
  enumeration does not re-list it as new work, and names the fix
  (`around_mi_422_chroma` under the ss (1, 0) guard) required if a future
  fixture enters it.
- No behavioural change: `decode.rs` differs only by that comment.
  `cargo check -p ec-av1` → 0 errors (1 pre-existing `dead_code` warning in
  `dumpio.rs::pin_reporting`, unrelated).

## Evidence summary

- Class sites: 1 unwitnessed (`decode_intrabc_owned_rect`, 18022/18061);
  9 guarded/closed rows (4:2:0 only, 4:4:4 only); 4 single-cell-equivalent
  rows; 39 already-routed rows.
- Probe counts: `EC_IBC422GATHER` 0 / `EC_HALVSWEEP` 0 over 24 `422`-named
  tests (21 decoding cells); `EC_HALVSWEEP` 5 on `r512.obu` (4:4:4) proving
  the instrument fires.
- `decode.rs`: comment only at 18022; probe removed.
- Report + comment committed on `lane/av1422gathercensus` (no push).

## Repro

```text
git worktree add -b lane/av1422gathercensus ~/.cache/wt/av1422gathercensus 2ca9e4e9
cd ~/.cache/wt/av1422gathercensus
# re-add the probe at decode.rs 18022 (`eprintln!("EC_IBC422GATHER ...")` under
# an env_flag) and run:
CARGO_TARGET_DIR=~/.cache/tgt-av1422gathercensus EC_IBC422GATHER=1 EC_HALVSWEEP=1 \
  cargo test -p ec-av1 --lib 422 -- --nocapture --test-threads=1   # 0 probe lines
CARGO_TARGET_DIR=~/.cache/tgt-av1422gathercensus EC_HALVSWEEP=1 \
  cargo test -p ec-av1 --lib a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes \
  -- --nocapture                                     # 5 EC_HALV lines (liveness)
```
