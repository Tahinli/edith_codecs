# lane-av1-llband2: 4:4:4 lossless above-band cell — sibling fix ported (r1)

## Charter

Resolve the `av1llsub8` stop (aligned step 19228: 16x8 leaf, U unit bc=2
br=0, `txb_skip` base 1 vs oracle base 0 — an above-band cell). Port the
already-reviewed sibling fix if it is the same writer; never author a
second fix for the same defect. 4:2:0 lossless stays exact. Work in
`edith_codecs-av1llband2` @ 6cf92769.

## Writer identification (done before any edit)

Read `edith_codecs-av1llband` r1 (`lanes/av1llband.report.md`) and commit
`01a34bc8` before writing anything. That sibling took the exact same stop
(178-byte key-frame miss, first divergence at the 16x8 leaf mi (16,24)'s
U bc=2 unit) to **0 differing bytes**, and was reviewed.

Same-writer proof:

- This branch HEAD is the sibling's parent commit `6cf92769` byte-for-byte
  (both continue `av1llsub8`).
- The defect site in THIS tree is the identical code the sibling named:
  `decode_rect_split`'s lossless chroma arm hardcoding the 4:2:0 unit span
  — `let luma_span = 8;` at `decode.rs:10458` here, feeding the `cu_mi`
  step, `around_mi`, `tu_reach` and both `record_mi_chroma` stamps.
- The stop signature matches the sibling's reproduced defect exactly
  (step 19228, 16x8 leaf, U bc=2 br=0, base 1 / row `[0][11]` vs oracle
  base 0 / row `[0][10]`).

⇒ `01a34bc8` IS that writer's fix. Ported it; no second fix invented.

## Port (faithful, byte-identical)

`git cherry-pick -n 01a34bc8`, dropping the sibling's lane report file
(`lanes/av1llband.report.md` — this lane writes its own). Post-images of
all three source files `cmp`-verified IDENTICAL against the sibling
worktree at `01a34bc8`:

- `crates/ec-av1/src/decode.rs` (+45/−10): `span_x = 4 << ss_x(fctx)`,
  `span_y = 4 << ss_y(fctx)` replaces the hardcoded 8 for the `cu_mi`
  step, `around_mi_rect`, `tu_reach`, the immediate and replayed
  `record_mi_chroma` stamps (`ll_chroma_units` carries per-unit spans);
  non-vacuity counter `RECT_SPLIT_LOSSLESS_CHROMA444_HITS` +
  `rect_split_lossless_chroma444_hits()`.
- `crates/ec-av1/src/stream.rs` (+77): gate
  `a_real_aomenc_lossless_444_key_frame_decodes_sample_exact` over the
  pinned fixture, with the lossless-frame blindness guard and the
  counter-moved non-vacuity assert.
- `crates/ec-av1/examples/decode_probe.rs` (+4): `llband:` counter line.

Off-limits files untouched: `decode_rect4_16_strip`,
`read_inter_rect_chroma`, `restoration.rs`, intra write-back. No 422
model, no luma path, no `sub8_leaf_chroma444` (innocent: the 16x8 leaf
never reaches it). 4:2:0 arithmetic is unchanged (`4 << 1 = 8`).

Fixture provenance: `fixtures/ll444-lossless-key.obu` (untracked, as in
the sibling), copied from the sibling worktree, sha256
`9fc9ce1f5f6475a1d483f404b44acb90f72a80be2e5c51dedf0aeb1da9a3f5dc`
(4927 B; cut of the `ll444` recipe's key-frame OBUs: `testsrc2 128x96`
yuv444p, `aomenc --profile=1 --lossless=1 --enable-palette=0
--enable-intrabc=0`, oracle build `~/.cache/aom-oracle`).

## Verification (measured, this tree)

- New gate `a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`:
  **green** — key frame decodes **0 differing bytes** (sample-exact,
  all 3 planes vs ffmpeg `yuv444p` rawvideo, 36864 samples compared) and
  the counter moved (non-vacuity).
- `decode_probe` on the pinned fixture: `llband:
  rect_split_lossless_chroma444=160` (matches the sibling's 160 units),
  no panic.
- 4:2:0 lossless stays green:
  `a_lossless_libaom_key_frame_decodes_sample_exact`,
  `a_lossless_libaom_inter_frame_decodes_sample_exact` — both ok
  (3 passed, 0 failed in the gate run).
- `cargo check -p ec-av1 --all-targets`: 0 warnings, 0 errors.

## Deferred (spoken, not hidden)

- **1:4 strips at lossless 4:4:4** (`decode_rect4_16_strip`): same span
  defect class (per the sibling's r1), but the file is owned by a live
  lane and off-limits to this branch — the fix is the same span
  derivation measured here. deferred(fix-now in the owning lane).
- Lossless INTER frames panic at `reconstruct_mc_rect` (decode.rs:2589):
  `read_inter_rect_chroma`'s branch, untouched, as chartered.
  deferred(fix-now in the owning lane).
- Intrabc rect leaves at lossless 444 still route the rect arm's
  `read_coeffs_rect` path: unreachable with `--enable-intrabc=0`
  fixtures. deferred(fix-now after the inter side is green, with an
  intrabc 4:4:4 fixture), matching `av1llsub8`/`av1llband`.
