# lane-av1chromarect-r3 — the `r512.obu` chroma residual is already 0 on main

Worktree `~/.cache/wt/chromarect-r3`, branch `lane-av1chromarect-r3`, base
`70ed5601`. **No decode-path change is committed on this branch**: the ticket's
first measurement came back at zero, so there is nothing left to fix. The
working tree at commit time is byte-identical to main; the only file added is
this report.

Result shape: the same as `lane-av1loss444` — a re-measurement that closes the
ticket with numbers instead of a patch.

## 1. First measurement: per (frame, plane) WRONG-SAMPLE COUNT on current main

Not the gate's own verdict — a temporary probe test that decoded
`crates/ec-av1/fixtures/r512.obu` (6948 B, 512x512 8-bit 4:4:4, three frames),
dumped both sides and COUNTED mismatching samples per (frame, plane) instead of
panicking on the first one. Compare method, named in full:

* oracle: the instrumented libaom `aomdec` at `~/.cache/aom-oracle/build/aomdec`,
  `aomdec --codec=av1` with `EC_AV1_FINAL_DUMP=<prefix>`, per-frame dumps
  `<prefix>.f0/.f1/.f2`;
* ours: this crate's `decode_stream` with `set_final_dump_prefix(<prefix>)`, the
  same rung, so both sides are the SHOWN 8-bit planes in decode order;
* unit of comparison: one sample, 262144 per plane, `3 * 3 * 262144 = 2359296`
  samples total; luma exactness is checked, not assumed.

With `EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_FIXTURES=1`
and a private `CARGO_TARGET_DIR`, on main `70ed5601` unmodified:

    ibc_rect_hits=5  skip_window_hits=24
    f0 Y: 0 wrong from 262144      f0 U: 0 wrong from 262144      f0 V: 0 wrong from 262144
    f1 Y: 0 wrong from 262144      f1 U: 0 wrong from 262144      f1 V: 0 wrong from 262144
    f2 Y: 0 wrong from 262144      f2 U: 0 wrong from 262144      f2 V: 0 wrong from 262144
    TOTAL 0 wrong from 2359296

Both engagement counters fired on the same decode (`IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS
= 5`, matching the five `EC_HALV` strips the earlier rounds counted, and
`SKIP_CHROMA_OVERRIDE_WINDOW_HITS = 24`), so the 0 is measured on a decode that
reached the arms, not on a witness that stopped exercising them. The probe was
removed before the commit; it is not in the diff.

**The ticket's residual is 0. 79969 → 0 happened on main, not in this lane.**

## 2. Who closed it, and by what mechanism

`2c0fd2b9` — "Merge lane-av1chromadc: the skipped-block chroma override is
windowed per unit (79969 → 0)", whose report is
`lanes/av1chromadc.report.md`. It is in main's history and predates this lane's
base.

Mechanism, restated because the ticket asks for it and r2's `NOT DONE` left it
open: at 4:4:4 a skipped 64x64 square block has a **64x64 chroma plane block**,
which libaom walks as a 2x2 grid of `TX_32X32` units
(`get_vartx_max_txsize` → `av1_get_adjusted_tx_size(TX_64X64) = TX_32X32`; the
64-point `av1_get_max_eob` truncation is a luma rule, and the chroma walk steps
by `tx_size_*_unit[max_tx_size]`, decodeframe.c:994-1004). r2 read the dirty map
correctly and stopped one step short: the defect is **not** a per-unit DC
predictor. It is that the block's chroma prediction is not edge-derived at all —
it is the intra-BC / UV-palette **override** carried whole on the `PALETTE_PRED`
slot (`decode_block`'s `intrabc_bufs` / `palette_uv_bufs` →
`set_palette_pred` → `PlaneBuf::reconstruct`'s `pred`), and `reconstruct` uses
the buffer it is handed as the UNIT's own `side x side` prediction. The pre-fix
call handed every unit the whole buffer, so at 4:4:4 each unit predicted from
the block's top-left unit. At 4:2:0 the chroma plane block of a 64x64 block is
one 32x32 unit, so the whole buffer IS that unit's window and the defect is
structurally invisible — which is why no 4:2:0 gate ever went red.

The site is `decode_block`'s skip-arm chroma unit loop (both planes),
`crates/ec-av1/src/decode.rs:21938` — the `chroma_window` closure — and it is
outside `decode_intrabc_owned_rect`, which is why the strip arm's own counters
(`IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS`) are unchanged by the fix.

## 3. Non-vacuity: the 0 is not a blind compare

A compare that cannot fail proves nothing, so the closing arm was put back the
way it was before the fix and the SAME probe re-run. Mutation: `chroma_window`'s
`if cn_cols == 1 && cn_rows == 1` early return changed to `if true`, i.e. every
unit gets the whole buffer — the pre-fix shape, in one line.

    f0 Y: 0 wrong      f0 U: 7488 wrong from 262144, first=229728 (x=352, y=448)
                        f0 V: 8128 wrong from 262144, first=229728 (x=352, y=448)
    f1 Y: 0 wrong      f1 U: 22571 wrong from 262144, first=65704 (x=168, y=128)
                        f1 V: 41782 wrong from 262144, first=65704 (x=168, y=128)
    f2: all three planes 0
    TOTAL 79969 wrong from 2359296

Those are the ticket's numbers, the r2 numbers and the r1 numbers, digit for
digit, including both first-divergence coordinates. So the compare reaches the
seed (`x=352 = 320 + 32`, the 2x2 chroma grid boundary of the skipped 64x64 at
`px=(320,448)`) and the f1 column (`x=168, y=128`, inside the skipped 64x64 at
`px=(128,128)`), and the green 0 on the unmutated tree is a real measurement.

Both COMMITTED gates also red under the same one-line mutation, and green
without it:

| gate | mutation | unmutated |
| --- | --- | --- |
| `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit` | **FAILED** — `frame 0 plane U … first divergence at sample 229728 of 262144 (x=352, y=448), 7488 samples wrong` | ok |
| `a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes` | **FAILED** — `frame 0 plane U diverges at sample 229728 of 262144 (x=352, y=448) -- the exact prefix this gate pins is 262144` | ok |

The second gate is the one whose floors the ticket names. Its `EXACT_PREFIX` is
`[[262144; 3]; 3]` — FULL byte-exactness on all three planes of all three
frames, raised from `[[262144, 229728, 229728], [262144, 65704, 65704],
[262144, 262144, 262144]]`. No floor was lowered by this lane, because this
lane changed no floor. The mutation message quoting `the exact prefix this gate
pins is 262144` is the proof that the floor is already at the top.

## 4. Corroborating gate families, clean tree, no change

    $ EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_FIXTURES=1 \
        cargo test -p ec-av1 --lib -- a_444 lossless_444 intrabc_rect 444_lossless
    test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 736 filtered out

(`a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes` and
`a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit` are
both in that set.) The wide family, same env, same filter string:

    $ EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_FIXTURES=1 \
        cargo test -p ec-av1 --lib -- rect intrabc ibc chroma 444 420 subsampl
    test result: ok. 145 passed; 0 failed; 5 ignored; 0 measured; 623 filtered out; 246.13s

The 5 `#[ignore]`d tests are main's (two superblock-size globals, a directional
sweep, an intrabc probe needing the real-library manifest, the rectx recipe
sweep). This branch is byte-identical to main outside this report, so there is
no set-difference against main to run.

`cargo check -p ec-av1 --all-targets` clean, 0 errors 0 warnings, on the private
`CARGO_TARGET_DIR`, and `git status --porcelain` empty after the probe was
removed — a green gate next to an unbuilt tree would have been a stale binary.

## 5. The two refuted premises, confirmed as refuted

Both were named in the ticket as already refuted and were not re-derived. For
the record, what the measurement says about each:

* **"ours 2688 vs oracle 2395 reads"** — not reachable from this ticket: no
  coefficient read is involved in a zero-residual skipped block, and the
  pixel-level compare above reproduces the residual to the digit without any
  symbol accounting.
* **"the skip-arm footprint"** (lane-av1ibcfork, `e7152d26`) — that fix landed
  and is correct on its own witness (`444_intrabc_rect4_witness.obu`, full
  exactness, gate `a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint`),
  but it is **not** what closed THIS residual. Restoring the pre-`chromadc`
  windowing shape on top of both fixes reproduces 79969 exactly, which places
  the whole residual in `chroma_window` and leaves nothing for the footprint
  correction to explain on this stream.

## 6. Not done

1. The **UV-palette arm** of the class (`palette_uv_bufs` on a non-intrabc
   multi-unit chroma walk) still has no witness. It shares the fixed line with
   the intra-BC arm, but the measurement is inherited, not observed. Unblock: a
   4:4:4 encode with `--enable-palette=1` and a SKIPPED block of side 64 or
   above, `--min-partition-size=64`, and a gate reading
   `SKIP_CHROMA_OVERRIDE_WINDOW_HITS` with `src=palette` distinguished from
   `src=intrabc`. That encode was not run here.
2. `decode_intrabc_rect` (the non-`owned` sibling) and the other rect chroma
   paths were not re-verified against a multi-unit override; they window per
   unit already and this lane changed nothing. Unblock: a witness per path, or
   one sweep of all override-carrying rect arms.
3. No heavy encode, no VPS fleet run. The measurement is fixture-driven against
   the local instrumented oracle; this lane requested no fleet capacity.
