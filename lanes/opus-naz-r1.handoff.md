## Exact next steps

1. Add one dump loop after the top-8-windows block (~line 3488, before `# first encoder frames`): print `bands_s/bands_o/bands_r` ln-energies for metric windows xi 12–20 (index `(xi*NBANDS+bi)*CHANNELS+ci`) — shows WHICH bands carry the 1e19 (HF noise-fill vs broadband). `cargo check` first; the edit tool dropped braces twice this lane.
2. If ours >> source in specific bands at +40ms: inspect decoder energy reconstruction after `old_band_e=-28` (fine/coarse energy prediction on intra frame 2, noise fill, anti-collapse) in `crates/ec-opus/src/celt.rs`; if silence windows: our silence decode path isn't emitting exact zeros.
3. One lever max; acceptance = naz err_ratio drops AND corr/dropout ≤.001 regression; gate: `SWEEP_ONLY=naz,dl8a,sadie cargo test -p ec-opus --release --test conformance encoder_library_gate_vs_libopus -- --ignored --nocapture`; `git checkout -- lanes/opus-gate-r1.sweep.txt` before commit.
4. Write `lanes/opus-naz-r1.report.md` verdict-first (cause: four startup windows — silence-decode leak + first-attack-frame error after digital silence; bands: TBD from step 1; lever tried/none; err_ratio 23.87 before → after).
5. Commit all on `lane-opus-naz`; no merge, no push.

HANDOFF: diagnostic narrowed to 4 startup metric windows (digital-silence intro + first attack frame of naz); test instrumentation added & verified (top-8 windows, first-10 frames dumps in lanes/opus-naz-r1.bands.txt); remaining: per-window per-band energy dump → pick single lever (silence-decode leak or attack-frame energy reconstruction after old_band_e=-28) → gate → report → commit on lane-opus-naz.
DELTA: none
