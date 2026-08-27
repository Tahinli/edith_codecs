# AV1 inter-frame package — foreman decomposition (2026-08-26)
Goal: ec-av1 inter frames -> edith_replica drops rav1e. User-seat gate: AV1 export from
edith_replica plays, ffmpeg decodes frame-for-frame, `cargo tree -p engine` has no rav1e.

Lanes (I1..I13). Wave 1 armed 2026-08-26: I1 cdf / I2 hdr / I3 mvstack / I4 mc, worktrees
edith_codecs-av1-{cdf,hdr,mvstack,mc}. Then I5 motion (needs I4); wave 2 I6 inter tile
writer (needs I1+I3) then I10 q-contexts (cdf.rs conflict — sequential after I6);
sequential tail I7 encode loop+ref buffer -> I8 ffmpeg multiframe VERIFIER gate ->
I9 pad-to-32/render-size crop -> I11 streaming facade+colour config -> I12 replica swap ->
I13 entry-surface + rav1e parity VERIFIER.

Swap blockers beyond inter itself:
- Q_CTX_2 61..=120 only coeff q context (tile.rs:84,600) -> I10 adds the other 3, range 0..=255.
- Picture::check refuses non-multiple-of-32 sizes (encode.rs:109-119) -> I9 pad+render crop
  (frame.rs:361 render_differs already writes crop syntax).
- Colour: encode.rs:187-199 hardcodes unspecified; rav1e seat sets Limited + BT709/BT601
  (replica export.rs:3494-3507) -> I11 config fields, else visible colour shift.

Replica rav1e surface (all in edith_replica/crates/engine/): Cargo.toml:190-197 dep;
export.rs:3345 Enc::Av1Sw, :3461 open_av1, :3609 encode arm, :3710 drain arm,
:3793-3817 collect_av1/pop_av1 (delete — ec-av1 facade is one-in-one-out like Enc::Hevc,
export.rs:3624 "Nothing is ever held back"); name strings export.rs:697, caps.rs:32,:137.
I12 gates: cargo tree -p engine no rav1e; cargo test -p engine --test av1 --test backends.

I5: crates/ec-av1/src/motion.rs, diamond search around PredMv + half/quarter-pel refine via
mc.rs; cost SAD/SSE + lambda*mv_bits priced by interval narrowing (never per-entry prob);
constants swept before landing. I6: tile.rs sb_coeff_inter_frame_tile — per block skip,
intra_inter, single-ref chain, inter_mode/drl/mv_joint+components; intra-in-inter uses
Default_Y_Mode_Cdf NOT KF_Y_MODE; extend Neighbours with per-mi is_inter+mv.
I7: encode_sequence(&[Picture]) keeps decoded recon as LAST ref, one slot; RD set =
intra ∪ {NEARESTMV skip, NEARESTMV coded, NEWMV}; gate = sample-exact ffmpeg equality
key+4 inter. I8 verifier: EC_AV1_CLIP multi-frame, per-frame PSNR floor, inter frames
smaller than key on low-motion + inter-block share printed (gate-blind-to-feature).
I11: EncoderConfig{w,h,q|bitrate,gop,colour}, encode_planes one-in-one-out, flush none;
30-frame gop-15 test = 2 key + 28 inter; colour fields round-trip via ec-av1-syntax.
I13 verifier: entry-surface export from replica settings row on a real ~/Videos clip,
label/progress/error-path captured; parity table vs rav1e (bytes, PSNR, wall-clock);
>2x bytes at equal PSNR = numeric REPORT not fail.

Rejected: feature-depth-first (compound/switchable/TX_SELECT first) — triples CDF surface
before first ffmpeg-decoded inter frame.

Charter boilerplate every lane: own worktree + CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1-<lane>;
no new tests/*.rs (ec-av1 tests inline); never validate tables against rav1e layout (transposed);
gates cargo test -p ec-av1 (+ --release for ffmpeg roundtrips), clippy, fmt --check; long runs
under systemd-run MemoryMax=10G; merge into main at close, branch+worktree die same batch.
