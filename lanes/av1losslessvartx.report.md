# lane-av1llvartx — the two lossless var-tx publishes: REACHABLE, and why no cell can measure them

Base `main` = `cc9f2668`. Worktree `/home/tahinli/.cache/wt/av1llvartx`, target
dir `/home/tahinli/.cache/tgt/av1llvartx`, `EC_NOMEMGUARD=1`. Oracle:
`~/.cache/aom-oracle/build/aomdec` / `aomenc`, libaom source at
`~/.cache/aom-oracle/src`.

## 0. Verdict

**The item's stated reason is REFUTED. The two arms are reachable, and eight
committed fixtures reach one of them. What is true is narrower and more
specific: the publishes are reachable and correct, but every cell that makes
them *observable* is a cell this decoder already gets wrong for an unrelated
reason.** So the coverage claim is not closeable as a revert-red pin, and this
report closes it as **ACCEPTED-UNCOVERED with the reason corrected** — see §5
for the precise predicate and §4 for the measurement that replaces the
report's guess.

Every number below is from a measurement in this session. All instruments are
removed; `git status --porcelain` is empty and `decode.rs` is byte-identical to
`cc9f2668`.

## 1. The instrument

Two `thread_local!` counters, one per arm, incremented on the line *after* each
`txfm_partition_update_rect`:

* `LL_VAR_TX_RECT_HITS` — `read_block_tx_size_rect`'s lossless arm
  (`decode.rs:30173` on main).
* `LL_VAR_TX_INTRABC_HITS` — `decode_rect4_16_intrabc`'s lossless arm
  (`decode.rs:18198` on main).

Each fixture decoded on its own thread so the thread-locals are read from the
thread that decoded. A third throwaway instrument (§3) marked the published
cells in a shadow grid.

## 2. The counter fires — 10 committed fixtures reach these arms

Full sweep of all 105 files in `crates/ec-av1/fixtures/`:

| fixture | rect arm | intrabc arm |
|---|---:|---:|
| `ll444_c_altref_leaf8_warp.obu` | **598** | 0 |
| `420_lossless_arf_1to4_320x240_5f.obu` | **29** | 0 |
| `ll444_minp8_inter.obu` | **30** | 0 |
| `ll444_defaultp_inter_strip.obu` | **48** | 0 |
| `ll444_sb64_1to4_lossless.obu` | 3 | **1** |
| `ll444_intrabc_rect_l2.obu` | 1 | **1** |
| `lossless_sb128_rect_kf.obu` | 3 | 0 |
| `420_lossless_tallinter_8x16.obu` | 1 | 0 |

**8 of 105 fixtures reach the rect arm; 2 of 105 reach the intrabc arm; 95
reach neither.** (The remaining 13 are 4:2:2 or otherwise refused.)

This directly contradicts the report's §4.3 concession, which read the arms as
possibly unreachable. They are not merely reachable — `ll444_c_altref_leaf8_warp`
enters the rect arm **598 times**.

## 3. Why the committed fixtures do not measure them: the publish is a no-op there

The arms publish `4` into libaom's `above_txfm_context`/`left_txfm_context`
grid (`av1_common_int.h:1641` `set_txfm_ctxs`). A var-tx context read compares a
neighbour cell against the block's own transform width
(`decode.rs:29151-29152`, libaom `txfm_partition_context`):

```rust
let above = usize::from(usize::from(above_px) < tx_w);
let left  = usize::from(usize::from(left_px)  < tx_h);
```

A cell the arm published holds `4`; a cell nothing wrote holds
`TXFM_CTX_INIT = 64` (`decode.rs:29294`, libaom `tx_size_wide[TX_SIZES_LARGEST]`).
**For every block that can read the grid, `tx_w >= 4`, so `4 < tx_w` and
`64 < tx_w` agree except at `tx_w == 4` exactly.** The publish is therefore
observationally equivalent to leaving the cell alone for the whole grid domain
except the 4-wide corner, and on a fully-lossless frame *every* block is
lossless, so nothing reads a var-tx symbol at all.

Reverting only the two publishes and re-hashing all eight reaching fixtures:
**8/8 byte-identical output.** The report's "94 comparable, zero red" is
reproduced and explained.

## 4. A cell that makes them observable exists — and is already red

`--end-usage=q --cq-level=0 --aq-mode=1` is aomenc's one path to a **mixed**
frame (some segments `qindex == 0` = lossless, others not); variance AQ is the
only writer of `SEG_LVL_ALT_Q` with no clamp at base 0
(`aq_variance.c:80-90`, the two AQ modes that clamp are `aq_complexity.c:110`
and the `av1_vaq_frame_setup` guard itself). This is the shape the crate's own
`a_real_aomenc_mixed_lossless_segment_frame_decodes_sample_exact` gate uses.

On such cells the two builds separate. Over 188 generated cells:

* **93 of 188 change output** when the two publishes are removed
  (`hash_nopub` vs `hash_true`, per-frame FNV over all three planes).
* Substituting a wrong value (`33`) for the published `4` also changes output on
  **93 of 188** — so the value is genuinely read, not merely the write's
  existence.

So the arms are load-bearing, and the load-bearing cells are constructible.

**The blocker is this decoder's own state, not the encoder.** Of the 93
sensitive cells, **0 are byte-exact**; of the cells that *are* byte-exact
against `aomdec`, **0 are sensitive**. The split is total, with no overlap:

| | byte-exact vs `aomdec` | revert-sensitive |
|---|---:|---:|
| reaching cells | 15 | 93 |
| of those exact | 15 | **0** |

Every revert-sensitive cell already decodes wrong with the publish in place
(typically 12 000–46 000 wrong bytes of 294 912), i.e. it is red for a
*different* defect. Pinning one would make a gate that is red before the fix it
is supposed to witness, and its redness would say nothing about these two
publishes.

The exactness ceiling is a separate, pre-existing defect in this decoder, not a
property of the mixed class: any stream of **5+ frames** decodes more frames
than the oracle emits (a 5-frame encode yields 6 decoded frames; a plain
`--cq-level=20 --lossless=0` 5-frame stream does the same). 4-frame and shorter
mixed cells are exact. That is out of this lane's scope and is **not** a
defect introduced here — it is recorded so the next census does not re-derive
it.

## 5. The libaom predicate, quoted

The candidate reason on file is **correct as a statement about the var-tx
search and wrong as a statement about these two arms.** libaom
`av1/decoder/decodeframe.c:1226-1245`:

```c
  int inter_block_tx = is_inter_block(mbmi) || is_intrabc_block(mbmi);
  if (cm->features.tx_mode == TX_MODE_SELECT && block_signals_txsize(bsize) &&
      !mbmi->skip_txfm && inter_block_tx && !xd->lossless[mbmi->segment_id]) {
    /* var-tx: read_tx_size_vartx per unit */
  } else {
    mbmi->tx_size = read_tx_size(xd, cm->features.tx_mode, inter_block_tx,
                                 !mbmi->skip_txfm, r);
    if (inter_block_tx)
      memset(mbmi->inter_tx_size, mbmi->tx_size, sizeof(mbmi->inter_tx_size));
    set_txfm_ctxs(mbmi->tx_size, xd->width, xd->height,
                  mbmi->skip_txfm && is_inter_block(mbmi), xd);
  }
```

The exact predicate is **`!xd->lossless[mbmi->segment_id]`** on line 1228, where
`xd->lossless[]` is computed once per frame at `decodeframe.c:5194-5199` as
`qindex == 0 && y_dc == u_dc == u_ac == v_ac == v_dc == 0`. So:

* A **lossless** block always takes the `else` branch, where `read_tx_size`
  returns `TX_4X4` immediately (`decodeframe.c:1197`) and `set_txfm_ctxs`
  publishes it over the block's own `xd->width`/`xd->height`. **That is exactly
  what the two arms do** — they are the port of the `else` branch, not dead
  code, and they are correct.
* A **lossy** block takes the var-tx branch and *reads* the grid.

The blocker for a *witness* is therefore: **the publishing block and the reading
block must be in different segments, and the reader's grid cell must be one the
lossless arm wrote.** Both are constructible (`--cq-level=0 --aq-mode=1`), and
§4 shows the resulting cells are observable. The blocker for a *pin* is that
those cells are red for another reason.

## 6. Close

**ACCEPTED-UNCOVERED, reason corrected.** The next census should rely on:

1. The arms are **reachable and correct**; 8 committed fixtures enter the rect
   arm and 2 the intrabc arm. Do not re-derive "possibly unreachable".
2. They are **not measurable by a revert** on any cell this decoder currently
   decodes correctly, because `4` and `TXFM_CTX_INIT` (`64`) agree on the
   `neighbour < tx_w` comparison for every `tx_w >= 8` a reading block can have.
3. The cells where they *are* load-bearing are constructible
   (`--end-usage=q --cq-level=0 --aq-mode=1`, 4 frames) and **are already red
   for an unrelated defect**. Closing the mixed-frame exactness ceiling first
   would unblock this pin; that ceiling is a 5+-frame frame-count divergence
   affecting lossy streams too, and is filed here as the prerequisite, not
   fixed.

## 7. Regression

```
cargo test -p ec-av1 --lib -- lossless 444 420 var-tx \
  --skip bitrate_target_lands_within_5_percent_over_48_frames
test result: ok. 71 passed; 0 failed; 0 ignored; 0 measured; 731 filtered out;
             finished in 27.99s
```

## 8. Not done

* The mixed-frame frame-count ceiling (5+ frames decode extra frames; lossy
  streams too) is **not diagnosed here** — out of scope, named as the
  prerequisite in §6.
* The intrabc arm's observability was not pushed past the two committed 4:4:4
  cells that reach it. The screen-content recipe
  (`--tune-content=screen --enable-palette=1 --enable-intrabc=1` at
  `--cq-level=0 --aq-mode=1`) produced 4:2:0 cells reaching the **rect** arm
  only; no intrabc-reaching mixed cell was found in 8 screen arms.
* No instruments committed; no fixture pinned; no push, no merge, no rustfmt.
