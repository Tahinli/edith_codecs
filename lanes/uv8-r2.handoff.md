# lane-uv8 r2 — HANDOFF (merge done, root cause NARROWED to one block, gate still RED)

## Merged into lane-uv8 (tip f5c8dbf)
- `a9b4468` main aa83400 (clean).
- `5dc9707` lane-interp3 `10b801a` (clean).
- `c3ead8c`/prev `af47324` lane-intersub8 — CONFLICTS resolved:
  * `decode_inter_block8` compound-leaf interp read: kept **interp3's** copy (it adds the
    `gm_nontrans_c || skip_mode` suppression and the COMPOUND8_FILTER_HITS counter); dropped
    intersub8's duplicate read (two reads = one extra symbol).
  * `resolve_interp_filter` tail: kept BOTH (interp3's DUAL_FILTER_DIFF_HITS + intersub8's EC_IF trace).
  * `refusal_inventory.rs`: union of both lists, then deleted
    "an inter partition below 8x8 (...lane-sub8 scoped to intra)" — intersub8 landed that capability
    (the `the_decode_path_refuses_exactly_the_listed_cases` test named it).
- `6561ec4` lane-cdef `8c6065e` (clean).
- `c3ead8c` lane-sqdrift `7d498c0` — stream.rs conflict was two whole appended test fns interleaved;
  resolved by `git checkout --ours stream.rs` + `patch -p1` of `git diff 85887c7 7d498c0 -- stream.rs`
  (both new tests present: `..._reads_the_no_cfl_uv_alphabet`, `..._square_only_inter_sequence...`).
- `crates/ec-av1/src/cdf.rs` byte-identical to main (`git diff --stat main -- cdf.rs` empty).
- `f5c8dbf` adds EC_TRACE_MODE_STEP ladder prints in the 8x8 intra-in-inter arm
  (`EC_ISTEP ... name=use_filter_intra` in the oracle's exact format, plus `EC_ISTEP8 name=modes`).

## Gate result on the merged tree (RED, same shape as r1)
`uv8-gate-r2.log` (unit uv8-gate-r2-1788332583, before the trace commit): all three arms
30/30 named refusals, counted-exact=0, uncounted-exact=0. Re-armed after the merge as unit
**uv8-gate-r2b** → `$HOME/.cache/uv8-gate-r2.log`, then the full suite in the SAME unit →
`$HOME/.cache/uv8-suite-r2.log`.
Refusal histogram over the 90 attempts: 31x inter 16x16-level 1:4, 21x nonzero angle delta on an
8x8 intra leaf, 13x inter 8x8 HORZ, 10x non-skip rect strip, 5x intra 1:4 rect strip, 3x inter SB AB,
3x 128x128 SB HORZ/VERT/AB, 2x intra 4x4 in sub-8x8 split, 1x 32x64 split TU, 1x intra 8x4/4x8.
All still class [[refusal-from-own-desync]] — proven below.

## ROOT CAUSE LOCALISED (this is the round's real result)
Stream `~/.cache/uv8-tmp/a0.obu` (regenerate: `~/.cache/uv8-tmp/g2.sh 42 12 a0.obu`, exact gate
recipe, 8-bit, seed 42, cq 12; note it is a **128x128 superblock** stream — the gate recipe has no
`--sb-size=64`).
1. `EC_TRACE=1 aomdec` partition histogram (`ref_part.txt`): the reference codes **only** NONE and
   SPLIT (bsize 15/12/9 value=3, bsize 6 value 0|3, bsize 3 value 0). There is no rect/AB/1:4
   partition anywhere in the stream → every refusal above is our own desync.
2. Partition range ladder (ours `EC_TRACE_PART=1` vs ref, ignoring the ctx print convention:
   ref prints `4*bsl + ctx`): decode-order frame 1 matches 138/138; frame 2 first range divergence
   at element 119 — ref `(0,36,bsize=6,rng=48648)` vs ours `rng=38664`. The previous element,
   the 8x8 partition at **mi (2,34)**, still matches (rng 44040).
3. `EC_MODE` subsequence: ref's entries are consumed in order up to `(2,32)`; ours' next block
   `(0,36)` starts at rng 57984 vs ref 32884. Ref prints no EC_MODE for `(2,34)` → that block is an
   **INTRA 8x8 leaf inside inter frame 2**.
4. `EC_TRACE_MODE_STEP` ladder at that block: ours reads `use_filter_intra val=1 rng=64248` and
   `filter_intra_mode val=0 rng=43964` — **bit-identical to the oracle**. Our decoded modes there are
   `y=DC_PRED, uv=13 (UV_CFL_PRED)` (`EC_ISTEP8` line).
   => the whole mode-info half of this leaf, including the new non-DC/CfL uv path, is IN SYNC.
   The desync is in what follows: `read_block_tx_size` or the leaf's COEFFICIENTS.

## EXACT NEXT STEP
Both coefficient traces are already dumped: `~/.cache/uv8-tmp/ref_co.txt` (761992 lines) and
`ours_co.txt` (260655). Formats: ref `EC_COEFF plane=.. row=.. col=.. tx_size=.. rng=..` +
`EC_COEFF_STEP tag=all_zero|tx_type|base|br ...`; ours prints the STEP lines only (no EC_COEFF
header with plane/row/col). Align the two `EC_COEFF_STEP` streams and find the first `rng`
divergence — it must be inside the block at mi (2,34) of decode-order frame 2, i.e. the FIRST
filter-intra + CfL 8x8 intra leaf of an inter frame. Prime suspects, in order:
  (a) the CfL AC / chroma `TxbSet::Chroma4` txb ctx on a leaf whose luma is filter-intra;
  (b) `read_block_tx_size` for an intra block in an inter frame at 8x8 (`--enable-tx-size-search=0`
      here, so check whether a symbol should be read at all);
  (c) the luma `intra_ext_tx` row: `read_plane` gets `filter_intra` and applies `fi_tx_row`, but the
      CHROMA calls pass `mode` (raw luma mode) as the tx-row arg with `filter_intra=None` — verify
      against libaom `av1_read_tx_type` (chroma tx type is inferred, so this should be inert; prove it).
Do NOT re-do steps 1-4; they are pinned above with their artifacts.
