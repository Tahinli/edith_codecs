# lane-maskcomp r2 charter — port the wedge/diffwtd masked-compound BLEND

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-maskcomp2, branch lane-maskcomp2 @ 5e25f76.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-maskcomp2 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, `nice -n 19`, `CARGO_BUILD_JOBS=4`. Never push; never touch other worktrees; fixtures/ is a symlink — only new pinned .obu streams go in.

## Current state (r1 landed in 5e25f76 — do not re-derive)
- comp_group_idx==1 syntax is entropy-exact at BOTH refusal sites (decode.rs ~5176
  leaf16/32, ~6558 leaf8): compound_type, wedge_idx (16-ary), wedge_sign (literal),
  mask_type (literal) are consumed, then the blend refuses by name. Read
  lanes/maskcomp-r1.report.md first.
- Gate a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact exists; its own
  masked-compound refusal is currently EXPECTED and a zero-hit run SOFT-SKIPs.
- Interintra blend (interintra_blend, decode.rs ~4410) is the shape to mirror for a
  64-scale mask blend: (mask*p0 + (64-mask)*p1 + 32) >> 6.

## Scope r2: the blend
1. Wedge masks: port libaom's wedge codebook for the bsizes this decoder reaches
   (8x8..32x32 squares; wedge is only used where av1_is_wedge_used(bsize)) —
   reconinter.c av1_wedge_params_lookup + init_wedge_masks/init_wedge_master_masks
   (the 64x64 master masks are built from wedge_master_oblique/vertical tables then
   cropped/flipped per code + sign). Build them at first use or as consts; verify a
   handful of mask rows against a C-side dump (patch a printf into
   /tmp/libaom-src reconinter.c, rebuild /tmp/libaom-build with ninja) — do NOT
   trust a hand transcription unchecked (class shared-oracle-blindness).
2. DIFFWTD masks: build_compound_diffwtd_mask (reconinter.c): per-pixel
   mask = clamp(38 + (|p0-p1|) >> shift, 0, 64) for DIFFWTD_38, inverted for _INV
   — port exactly, mind the bit-depth shift chain (class mirror-the-decoder-shift).
3. Chroma: av1_get_compound_type_mask + the subsampling path (chroma reads the luma
   mask subsampled by 2x2 averaging? read av1_build_masked_compound /
   aom_blend_a64_d16-vs-8bit paths carefully; this decoder is 8-bit).
4. Flip the gate: masked-compound refusals FORBIDDEN; zero-hit SOFT-SKIP becomes a
   hard assert (hits > 0) with EC_MASKCOMP_GATE_ATTEMPTS=80 default kept. Hammer 8x
   with EC_AV1_GATE_DUMP self-pin (/tmp/claude-1000/mc-flake-N.obu).
5. Any mismatch: pin to fixtures/, recon-dump (EC_AV1_PREFILT_DUMP) → range ladder
   (exact ladder = prediction-side, i.e., your mask; ours[k]=aom[k+1]) → for mask
   bugs dump the mask array both sides (add an env-gated eprintln; patch aomdec's
   av1_make_masked_inter_predictor likewise).

## Done criteria
1. Gate green with hard hits assert, 8/8 hammer; 11 pins byte-exact; lib suite green.
2. Committed to lane-maskcomp2; REPORT lanes/maskcomp-r2.report.md, verdict FIRST.
Commit + report BEFORE budget runs out. If the wedge codebook cannot be finished
verified, land DIFFWTD alone (flip only the diffwtd refusal) — partial capability
with exact naming is mergeable; an unverified wedge table is not.
