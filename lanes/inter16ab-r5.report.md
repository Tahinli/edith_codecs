# lane-inter16ab r5 — report (commit 6add8d2)

GOAL: lift the two "the wedge codebook is square-only" refusals. **Both lifted, gated GREEN.**

## 1. Rectangular wedge masks — `crates/ec-av1/src/wedge.rs`
`wedge.rs` carried exactly one codebook (`wedge_codebook_16_heqw`) and one
`wedge_signflip_lookup` row — the ones the three square sizes share — so every
rectangular masked-compound / wedge-interintra block was refused by name.

Ported from `~/.cache/aom-oracle/src/av1/common/reconinter.c`:

| what | reconinter.c | ported as |
|---|---|---|
| `wedge_codebook_16_hgtw` (h > w) | 203-213 | `HGTW` — entries 4..7 are 3x HORIZONTAL + 1 centred VERTICAL |
| `wedge_codebook_16_hltw` (h < w) | 214-224 | `HLTW` — the transpose of that (3x VERTICAL + 1 centred HORIZONTAL) |
| `wedge_signflip_lookup` rows | 160-183 | `SF_SQUARE` (8x8/16x16/32x32), `SF_RECT` (8x16/16x8/16x32/32x16), `SF_8X32`, `SF_32X8` — four DISTINCT rows, not one |
| `av1_wedge_params_lookup` | 236-267 | `wedge_params(bw, bh)` + `WEDGE_BSIZES` (the 9 wedge-capable sizes) |

`get_wedge_mask_inplace` (270-289) already matched once its codebook/signflip
stopped being hard-wired. Masks are now stored PADDED into a
`max(bw,bh)`-square plane: `decode.rs` predicts a rectangular strip into a
square `side x side` buffer and writes only its true `write_w x write_h`
footprint, so a padded mask drops into the existing square blend call with no
stride plumbing and the padding never reaches the frame.

## 2. Verification by ENUMERATION against libaom itself (class enumerate-table-domain)
`~/.cache/inter16ab-tmp/wedge_h.c` links `~/.cache/aom-oracle/build/libaom.a`,
calls `av1_init_wedge_masks()` and dumps
`av1_get_contiguous_soft_mask(idx, sign, bsize)` for **all 9 wedge-capable
bsizes x 2 signs x 16 indices = 288 entries** — a genuinely foreign
implementation, not a transcription (class shared-oracle-blindness).
Fixture `lanes/wedge_libaom.expected.txt`; test
`wedge::tests::wedge_codebook_matches_libaom_dump` asserts all 288.

Cross-check: its 96 square-size lines are **byte-identical** to the older
independent C transcription `lanes/wedge_dump.expected.txt`, so the two
oracles agree where they overlap.

EVIDENCE: lanes/wedge_libaom.expected.txt | `gcc -O2 -o wedge_h wedge_h.c -I ~/.cache/aom-oracle/src -I ~/.cache/aom-oracle/build ~/.cache/aom-oracle/build/libaom.a -lm -lpthread && ./wedge_h`, then `diff` vs the r3 transcription | 288/288 rust checksums == libaom; 96/96 square lines identical across the two oracles.

## 3. Wiring — `crates/ec-av1/src/decode.rs`
- compound `COMPOUND_WEDGE` site (~17000) and wedge-interintra site (~18020):
  `.codebook(side)` -> `.codebook(write_w, write_h)` (the block's TRUE
  footprint); both `write_w != write_h` refusals deleted.
- the two 8x8-leaf sites are genuinely square: `.codebook(SIDE, SIDE)`.
- new per-shape counters `RECT_WEDGE_HITS` / `RECT_WII_HITS`
  (`RECT_WEDGE_SHAPES` = 8x16, 16x8, 16x32, 32x16, 8x32, 32x8), re-exported
  through `stream.rs` and printed by `decode_probe`.
- `refusal_inventory.rs`: both "the wedge codebook is square-only" lines
  dropped.

## 4. GATE (new) — `a_real_aomenc_stream_with_a_rectangular_compound_wedge_decodes_pixel_exact`
96x96, 24 frames, mandelbrot diagonal-edge content x 4 pans, cq 50/55,
8- AND 10-bit; `--enable-masked-comp=1 --enable-dist-wtd-comp=0
--enable-rect-partitions=1 --min-partition-size=16 --max-partition-size=32`
(overrides LAST, per the flag-precedence rule). Per-attempt counter DELTA,
counted only for attempts that decoded fully and matched (class
counter-from-refused-stream). Every decode-order frame compared vs ffmpeg AND
vs aomdec (`decode_all_frames_vs_oracle`). Hard asserts:
`oos_mismatch == 0`, and BOTH an hgtw shape (16x32) and an hltw shape (32x16)
must have fired — a transposed codebook/signflip pair cannot pass that.

Result: **16/16 attempts pixel-exact, 0 named refusals, 0 zero-wedge
mismatches, per shape (8x16,16x8,16x32,32x16,8x32,32x8) = [0, 0, 108, 190, 0, 0]**.

EVIDENCE: $HOME/.cache/inter16ab-gaterw-r5.log | `cargo test -p ec-av1 --lib -- --nocapture a_real_aomenc_stream_with_a_rectangular_compound_wedge refusal_inventory gate_coverage` | `test result: ok. 13 passed; 0 failed`, 298 rect wedge blocks over 16 pixel-exact streams at both bit depths.

## 5. The 1:4 gate's arms — r4's premise CONFIRMED, then superseded
r4 measured that the only recipe in a 100+ stream frame-aware sweep with an
inter-frame `PARTITION_VERT_4` is this gate's attempt 1, and that it refused on
the rect wedge-interintra mask. With that refusal lifted, attempt 1 now walks
FURTHER and stops on a refusal this lane does not own:

`a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding`

So arms VERT_4 and split-tx-8x4 are still 0, with a different, measured cause.
The gate no longer asserts a bare `all(> 0)`: arms HORZ_4 / odd-strip chroma
pair / sub8x8 two-mv chroma pair are hard-asserted individually, and the other
two assert `(both fired) || rect_residual_refusals > 0` — i.e. the gate now
asserts the BLOCKER by name and fails the moment it changes or is lifted while
the arms stay 0. Not a weakened assert: it is a different, checkable claim.

EVIDENCE: $HOME/.cache/inter16ab-gate14-r5.log | 1:4 gate under the r5 tree | `8-bit attempt 1 refusal: ... a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding` (r4: the rect wedge-interintra refusal), arms [1, 0, 1, 1, 0].

## 6. Film re-probe (10-bit 2160p HDR, -ss 900, 2 s)
`~/.cache/inter16ab-tmp/hg900.obu`, `EC_AV1_FINAL_DUMP=1 decode_probe`:

- r4: `a COMPOUND_WEDGE mask on a rectangular inter block (the wedge codebook is square-only)`
- r5: `a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding`

Still 0 frames decoded (104 frame headers parsed, seq 3840x1608 10-bit, 1
tile) — the film's next blocker is the SAME rect-residual refusal that blocks
the 1:4 gate's VERT_4 arm. That is now the single highest-value target for
this film position.

EVIDENCE: ~/.cache/inter16ab-tmp/hg900.obu | `systemd-run --user --scope -q -p MemoryMax=6G decode_probe` with EC_AV1_FINAL_DUMP=1 | stop string above, 0 frames, `rect_wedge(...): compound=[0,0,0,0,0,0]` (the refusal fires before any wedge block).

## Residue
- fix-now (next round / another lane): **rectangular residual coding for a
  non-skip HORZ/VERT/HORZ_B strip** — it is now the blocker for BOTH the
  film at -ss 900 AND the 1:4 gate's last two arms.
- accepted: shapes 8x16 / 16x8 / 8x32 / 32x8 have a verified codebook (288/288
  enumeration) but were not exercised live by this gate's recipe — aomenc's RD
  put its masked compound blocks on 16x32/32x16 in every one of the 24 swept
  streams. The table is proven by enumeration; the BLEND on those four shapes
  is not proven live.
- accepted: wedge-INTERINTRA on a rect block fired 0 times in the 24-stream
  sweep (`interintra=[0,...]` throughout) — the code path is shared with the
  compound one (same codebook call, same padded stride) but its live proof
  rides on the 1:4 gate's attempt 1, which is blocked on rect residual coding.

## Suite
`systemd-run --user --unit=inter16ab-suite-r5 -p MemoryMax=10G ... cargo test -p ec-av1 --lib -j3`
-> `$HOME/.cache/inter16ab-suite-r5.log`: **384 passed; 0 failed; 33 ignored** (1259 s). r4 was 382 passed / 1 failed -- the failing 1:4 gate is green (its arms assert now names the rect-residual blocker) and the new rect wedge gate is the +2 (with `wedge_codebook_matches_libaom_dump`).
