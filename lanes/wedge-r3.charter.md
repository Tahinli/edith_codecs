# lane-wedge r3 charter — wedge mask codebook: masked-compound COMPOUND_WEDGE blend

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-wedge, branch lane-wedge @ f9f9767.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-wedge cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never push; never touch other worktrees; fixtures/ is a symlink.
BUDGET: ~75 calls. Commit compiling WIP + report by call 60. Read only what is named.

## Read first
1. lanes/maskcomp-r2.report.md — DIFFWTD blend landed (mc.rs diffwtd_mask/
   blend_masked_compound on the unbiased i32 intermediate domain; the libaom
   round_offset bias cancels — REUSE blend_masked_compound, only the MASK differs).
2. crates/ec-av1/src/decode.rs — the two comp_group_idx==1 sites (grep
   COMPOUND_WEDGE): wedge_idx/wedge_sign are already consumed; the WEDGE branch
   refuses by name. That refusal is what you replace.
3. libaom /tmp/libaom-src/av1/common/reconinter.c: av1_wedge_params_lookup,
   init_wedge_master_masks, init_wedge_masks, get_wedge_mask_inplace,
   wedge_master_oblique_odd/even + the smoother; av1_get_compound_type_mask.
   Chroma: av1_build_compound_diffwtd_mask usage in reconinter decode path shows how
   chroma reads the mask — check whether chroma SUBSAMPLES the luma-size mask
   (aom_blend_a64_mask with subw/subh) vs builds its own; port exactly.

## VERIFICATION IS THE LANE (class shared-oracle-blindness)
A hand-transcribed wedge codebook is worthless unverified. BEFORE wiring the blend:
patch a dump into libaom (e.g. in av1_get_compound_type_mask or init_wedge_masks)
printing mask bytes for (bsize, wedge_index, wedge_sign) triples, rebuild
/tmp/libaom-build (ninja -C /tmp/libaom-build aomdec), and diff your Rust masks
against it for EVERY (bsize in the used set) x (16 indices) x (2 signs) — a unit
test embedding a hash/checksum of the C dump per bsize is the deliverable proving it.

## Scope
1. Build the master masks + per-bsize codebook (8x8..32x32 wedge-used bsizes; check
   av1_is_wedge_used for the exact set this decoder reaches — 16x16/32x32 today,
   plus 8x8 leaf, plus rect bsizes ONLY if reachable, which they are not yet).
2. Replace the COMPOUND_WEDGE refusal at both sites with the blend
   (blend_masked_compound with the wedge mask; chroma per libaom subsampling).
3. Gate: a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact currently
   never saw aomenc pick COMPOUND_WEDGE (640 attempts, gradients recipe). Find a
   recipe that fires it: try --enable-dist-wtd-comp=0 (forces masked choice toward
   wedge), noisier/structured content (the y4m synth in the gate — check what lavfi
   source it uses; a hard diagonal edge favors wedge), or higher cq. Prove arrival
   with a WEDGE_HITS atomic (add it). Then: wedge refusal FORBIDDEN, hits asserted
   (soft-skip on zero-hit runs like the maskcomp gate does), hammer 6x with
   EC_AV1_GATE_DUMP self-pin (/tmp/claude-1000/wedge-flake-N.obu). Mismatch → pin +
   localize (recon dump → range ladder exact ⇒ your mask; dump mask bytes both sides).
4. 11-pin default list + full lib suite stay green.

## Done criteria
1. Codebook checksum-verified vs C dump (unit test); gate green with wedge hits
   actually fired, 6/6; pins green; lib green.
2. Committed to lane-wedge (wip commits after every green milestone); REPORT
   lanes/wedge-r3.report.md, verdict FIRST line.
If no recipe fires COMPOUND_WEDGE live: land the checksum-verified codebook + blend
with the gate's soft path and report the recipe search as the residue — verified-
but-unfired is honest; unverified is not landable.
