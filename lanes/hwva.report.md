# lane-hwva: engine-hw on the cros-libva ec-va

## Commits
- edith (lane-hwva): bd09079b "♻️ engine-hw: hold the VA display in Rc, open it with ec_va::open, import PRIME via ec_va::import_prime"; 2c65f9f1 "🔧 Cargo.lock: resolve ec-va on cros-libva". Files: crates/engine-hw/src/enc_ec.rs, crates/engine-hw/src/lib.rs, Cargo.lock. No engine/ or app/ changes.
- edith_codecs (lane/safeva): 0a41f318 "lane/safeva: re-export surface::import_prime at the crate root". File: crates/ec-va/src/lib.rs (one line).

## API changes
- `ec_va::Display::open()` cannot return Result: `Display` is a type alias of `cros_libva::Display`, whose inherent `open()` returns Option and shadows any ec-va method. Fix used: `ec_va::open()` (crate root, already `Result<Rc<Display>, Error>`), engine-hw calls `ec_va::open().ok()`.
- `Surface::import_prime` cannot be an inherent method on the foreign `cros_libva::Surface`. The free fn `ec_va::surface::import_prime` (already in safeva) is now re-exported as `ec_va::import_prime(&Rc<Display>, &PrimeSurface) -> Result<cros_libva::Surface<PrimeImport>>`. engine-hw wraps it in `Rc::new(..)` for `ec_hw::Encoder::encode_surface`, which takes `&Rc<Surface<D>>`.
- engine-hw holds `Rc<ec_va::Display>` (session struct, `open_decode_display`, enc session). No `unsafe impl Send` added. No statics in engine-hw; sessions are `Box::into_raw`'d to the C ABI.

## Threading findings (Rc-holding sessions)
- Every `vh_open_at` / `vh_enc_*open` creates its own `Rc<Display>`; no Rc is shared between sessions, and `query_caps` opens and drops its own display. No Rc crosses threads inside the plugin.
- Engine side (crates/engine/src/hw.rs, hwproc.rs) carries `unsafe impl Send` for `HwSession` (hw.rs:539), `LocalEncoder` (hw.rs:870) and `Remote` (hwproc.rs:675). These now cover Rc-holding state the compiler cannot see.
- Experiment (reverted, not committed): removing the three impls makes `cargo check` fail at exactly one site, crates/engine/src/decode.rs:1478, where the decode worker thread is spawned with `opened: Source`. `Source` contains `Option<HwSession>`. In practice that field is always None at hand-off (`Source::demuxed` and `Source::open` both set hw: None); the session is opened on the worker itself (decode.rs:1638, 1748, 3697). So no session is actually moved. Export: decode_stream runs on a thread spawned at export.rs:9533 and opens its own HwSession; HW encoders are opened inside the export thread (export.rs:1221 via 3481/8015/8107/8231). Hardware sessions are never parked (only the SW decoders are, export.rs:9283).
- Recommendation (engine owner's call): spawn with the demuxer and meta and build Source on the worker, then drop the three `unsafe impl Send` lines so the compiler proves thread-locality.
- Sessions are used by one thread at a time, and Drop (vh_close) runs on the thread that drops the session, which is the owning worker thread.

## Test results
- ec-va + ec-hw (safeva, `cargo test --release -p ec-va -p ec-hw`): 97 passed / 0 failed / 0 ignored. Clippy `-D warnings`: clean.
- edith `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- engine-hw unit tests: 7 passed / 0 failed (not separately measured on the baseline).
- engine hardware tests with `--include-ignored`, LD_LIBRARY_PATH=target/release, real AMD gfx1200 VA-API: hw_decode 13 passed, hw_encode 6 passed, hw_midgop_seek 2 passed, hw_fallback 1 passed.
- Full engine suite `cargo test --release --no-fail-fast -p engine -- --include-ignored`:
  - Baseline (edith main fa829b15 + edith_codecs main): 1872 passed, 5 failed.
  - New (lane-hwva): 1871 passed, 6 failed.
  - Failed on both: ao_output x3 (muting, plays_and_tracks_position, no_daemon_opens_nothing); hevc_export::a_1080p_hardware_export_leaves_a_decoder_with_nothing_to_say (planned video is SW, not HW, on this box); mpegts::dts_and_truehd_tracks_are_refused_by_name_and_the_picture_still_opens.
  - New only: dependency_guard::no_forbidden_crate_is_in_the_dependency_graph. Message: "cros-libva is in the dependency graph: engine-hw -> ec-va -> cros-libva". The test (crates/engine/tests/dependency_guard.rs:23) forbids cros-libva, so this is a policy conflict. Left untouched: the decision is whether engine-hw may depend on cros-libva. Fix needs either removing "cros-libva" from FORBIDDEN or allowing it only under engine-hw.

## Environment
- Baseline layout was ~/.cache/hwva-base/edith (detached worktree of main) with edith_codecs symlinked to the main checkout. Its assets/ were symlinked from the main checkout, because the ignored media files were missing. Baseline worktree removed afterwards. The main checkout and its assets are intact.
- Logs: ~/.cache/hwva/engine_full_new.txt, ~/.cache/hwva/engine_full_base.txt, ~/.cache/hwva/hw_new.txt, ~/.cache/hwva/ecva_test.txt.
