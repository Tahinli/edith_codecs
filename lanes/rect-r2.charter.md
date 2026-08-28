# lane-rect r2 charter — rectangular mvstack/context threading; HORZ/VERT decode for real

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-rect2, branch lane-rect2 @ f9f9767.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rect2 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never push; never touch other worktrees; fixtures/ is a symlink.
BUDGET DISCIPLINE (you have ~75 tool calls, two predecessors died at the cap): read ONLY the files/lines named here; commit compiling WIP + write the report by call 60, whatever state you are in.

## Read first (in this order, nothing else)
1. lanes/partitions-r1.report.md — the defect you are fixing, fully localized.
2. git show 4782e57 -- crates/ec-av1/src/decode.rs — the strip-decode implementation
   to resurrect (HORZ/VERT arms with write_w/write_h; the arms currently refuse at
   decode.rs ~8212/~8232 with the finding in a comment).
3. crates/ec-av1/src/mvstack.rs — find_mv_stack's signature and its row/col scan +
   weight math (it takes a square bw4 today).
4. libaom /tmp/libaom-src/av1/common/mvref_common.c setup_ref_mv_list + scan_row_mbmi
   /scan_col_mbmi/scan_blk_mbmi: everywhere xd->width vs xd->height (n4_w/n4_h)
   appear asymmetrically — row scans use n4_w, col scans n4_h, weights use
   AOMMIN(2, ...) of each, max_row/col_offset derivation, has_top_right.

## The defect (pinned, deterministic)
fixtures/rect-flake-1.obu, decode frame 16, block (0,8) = PARTITION_HORZ:
strip 2 (32x16 at mi 4,8) must build its mvstack as a 32x16 block (n4_w=8, n4_h=4);
our square-context decode picked the wrong NEARMV DRL entry (got strip 1's (0,-16),
aom gets (0,0)). Range ladder diverges inside strip 2.

## Scope
1. Give find_mv_stack (and its ctx outputs new_mv_ctx/ref_mv_ctx + drl weighting)
   separate bw4/bh4 params; audit inside it every use of the current square bw4 and
   split per libaom (row scan lengths/weights vs col scan lengths/weights,
   max offsets, the extra-search + has_top_right rules). Square callers pass
   (bw4, bw4) — zero behavior change there (the lib suite proves it).
2. Thread bw4/bh4 through decode_inter_block's other side-derived SYNTAX surfaces
   for a rect block: is_inter/skip/ref ctx gathers over the true mi span (the _rect
   record methods from r1 already write the true span — the READ side must match),
   warp num_proj_ref/find_samples (bw4 vs bh4 in its row/col walks), obmc overlap
   eligibility (above row uses width, left col uses height). grep `bw4` inside
   decode_inter_block and adjudicate each use: width, height, or both.
3. Resurrect the HORZ/VERT arms from 4782e57 on top of this (side still drives CDF
   size-class selection — bsize 32x16 shares the 32-class tables? NO: check libaom
   size lookups: partition ctx bsize, tx size TX_32X16, size_group_lookup,
   skip/mode CDFs are indexed by bsize where 32x16 != 32x32 — verify EACH indexed
   table against libaom's bsize enum for BLOCK_32X16/16X32 and use the right rows;
   class cdf-row-held-constant).
4. GATE LADDER (in order): (a) rect-flake-1 pin must decode byte-exact:
   `EC_AV1_GATE_DUMP_PIN=/home/tahinli/Documents/Code/Rust/edith_codecs/fixtures/rect-flake-1.obu cargo test -p ec-av1 --release --lib pinned_warp_stream_decodes_pixel_exact -- --ignored --nocapture`
   (b) existing 11-pin default list stays green (c) un-clamp the free-partition gate
   recipe (stream.rs a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact:
   set --enable-rect-partitions=1 and --min-partition-size=16; the comment there
   documents this) and hammer 6x with EC_AV1_GATE_DUMP self-pin
   (/tmp/claude-1000/rect-flake-N.obu) — new mismatches: pin + localize via the
   report's ladder (recon dump → range ladder ours[k]=aom[k+1] → EC_AV1_TELL).
   (d) full lib suite.
5. Sweep HORZ_B's top strip (same class, latent): its arm should build the strip's
   stack as 32x16 too once find_mv_stack is rect-capable — flip it and confirm the
   3 warp pins + ii pins stay byte-exact (they contain HORZ_B).
6. Add rect-flake-1 to the pinned test's default fixture list ONLY once it passes.

## Done criteria
1. rect-flake-1 byte-exact; free gate un-clamped green 6/6 (or new pins + localization
   for anything beyond HORZ/VERT scope, refusing by name is fine for HORZ_A etc.);
   12-fixture pin list green; lib suite green.
2. Committed to lane-rect2 (wip commits as you go — commit after EVERY green
   milestone); REPORT lanes/rect-r2.report.md, verdict FIRST line.
If the full ctx audit cannot finish: land find_mv_stack rect-capability + the pin
green + arms still refusing the residue you did not audit, report what is left.
