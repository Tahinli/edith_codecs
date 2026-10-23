# lane h264encpool: persistent band workers for ec-h264

Base sha: dd323832f54c519b8bdbf75806ee6cb6180a6afe (main). Branch lane/h264encpool.

## Goal
Remove the per-picture thread spawn and join in the ec-h264 encoder (7366 clone3 calls for 600
frames in the edith 1080p export profile), and look for memset/memcpy in the encoder's inner loops.
Output must stay byte-identical.

## Changes
- `crates/ec-h264/src/enc/pool.rs` (new): `BandPool`, one std thread per band, fed over mpsc
  channels, joined in `Drop`. No new crates, no unsafe.
- `crates/ec-h264/src/enc/mod.rs`: the picture's shared inputs (sps, cfg, rc, source, reference,
  band table, slice type, targets) move into one `Arc<FrameShared>` per picture. Each band job
  moves its worker `Picture` in and back out (`BandMsg` / `BandOut`). The pool is created lazily
  on the first multi-band picture. `threads == 1` runs `run_band` inline with no pool. The source
  and reference come back out of the Arc once every band has answered. Band split, per-band
  state and stitching are unchanged.
- `crates/ec-h264/src/enc/mb.rs`: `#[derive(Default)]` on `Source` (needed for `mem::take`).
- `crates/ec-h264/examples/h264_clip_encode.rs`: `H264_CAVLC=1` selects CAVLC for the identity
  matrix. Default (CABAC) is unchanged.

## Byte identity
Matrix: two clips, 60 frames each, 1920x1080, threads 1, 4, 6, CABAC and CAVLC. Constant QP 26,
plus 4 Mb/s rate control at 4 threads. Clips: testsrc2 with temporal noise (2 s), and the first 60
frames of `fixtures/video/h264-1080p-23.976-8bit.mp4`. md5 of the Annex-B stream and of the
encoder reconstruction. Base is main's encoder code with only the example switch added. The
identity run happened on host c/a/b (auto) on the server.

| clip | rate | threads | mode | stream md5 (base = new) | recon md5 (base = new) |
|---|---|---|---|---|---|
| tsrc | 0 | 1 | cabac | 388f3dae3ebfeea0445c0700b7314c80 | c1704e50ff0d6eb1435c589cacb0afde |
| tsrc | 0 | 1 | cavlc | be5fecb43dcce9c2e7a98efe2385bc46 | cd53f96c1c24bd097620fac72a3f1b31 |
| tsrc | 0 | 4 | cabac | 6b75735bc593a6d2f05285e88b175923 | fa01907f117897c3719e2126c90bf28a |
| tsrc | 0 | 4 | cavlc | d86540cc61c879842b5d8c66f8b31328 | a6c27343a9d9808ee15478e4a796ad49 |
| tsrc | 0 | 6 | cabac | 8429e19094fbedce5952632884208601 | 228e1110a737031eab318cd35db5232b |
| tsrc | 0 | 6 | cavlc | 8dbafb5d8c23bce3fd519569f9f500cd | bcb849b54b92cdb9400d6f14da9ec1e9 |
| nat | 0 | 1 | cabac | b523cc295ba9401064caac2a1841cc2d | d8bc1103bd15b170fdb35daaa5aa8896 |
| nat | 0 | 1 | cavlc | 800c63953c7652563c6e409c612a2658 | d5b08f3f53eb94b6229ced01fefd211a |
| nat | 0 | 4 | cabac | 16803c0caef0a5463d422a6a6f237e79 | fe65125b9af8a3345dc3ea68d5c2d966 |
| nat | 0 | 4 | cavlc | b518357eb8db8f2d2673568254216968 | bbc901b4cc3f9d86423a918b59addef0 |
| nat | 0 | 6 | cabac | 9a51e5c06982b1f68d2b98392cc476ac | 381fadd9d1c442ebcec3d96ea8b2e078 |
| nat | 0 | 6 | cavlc | 790a29991270411179874d8de0b8a0f2 | 99ff19693bd11a3e3d7352c136a7ae6c |
| tsrc | 4000000 | 4 | cabac | cac2173e3971188d9d9525f3d037bc56 | 5173f89a89ff1f4e9c78d6b522d32bd9 |
| tsrc | 4000000 | 4 | cavlc | 7de8a214322277ee7b7c8ab73f7ce346 | c9cbcc233304d1b991f4b9c5363abcf7 |
| nat | 4000000 | 4 | cabac | bf26719721c4c7897b85b9aea7e1aa92 | 863d7a9f3724139cc6ec55916f37ee38 |
| nat | 4000000 | 4 | cavlc | 93bd085fafb1dd84e2db9176319ff84e | 50049ac695d3c495361b51ba3debcd63 |

All 16 rows: base and new identical (`diff` of the two md5 files is empty).
Local check: 8-frame testsrc2 at 1, 4 and 6 threads, base and new streams identical.

## Speed (local, 12-core, 4 s noisy testsrc2 at 4 Mb/s, 120 pictures, `h264_clip_encode`)
Host load average was 7 to 32 during the runs, so the figures are noisy. Three interleaved rounds
per row, median reported.

| build | threads | run 1 | run 2 | run 3 | median fps |
|---|---|---|---|---|---|
| main (before) | 1 | 12.54 | 11.25 | 12.86 | 12.54 |
| lane (after) | 1 | 12.89 | 10.51 | 12.00 | 12.00 |
| main (before) | 6 | 36.10 | 40.78 | 41.51 | 40.78 |
| lane (after) | 6 | 44.65 | 41.38 | 38.69 | 41.38 |

An earlier, unpaired base run measured 12.78 fps at 1 thread and 42.33 fps at 6 threads. Within
this noise the pool gives no measurable change. Its removed cost is per frame and small: 6 spawns
and joins per picture is tens of microseconds against about 40 ms per picture. The wall time is
dominated by MB coding, so the pool is a structural cleanup, not a speed-up.

## memset/memcpy hot spots (callgrind, 8 frames, 1 thread, main build)
Instruction share, from `callgrind_annotate`: memset 1.9%, memcpy 1.9% of 21.0 G instructions.
- `encode_inter_mb`: `Levels::default()` (about 2.8 KB zeroed) and 2 to 3 `Levels` clones per
  inter macroblock (`mb.rs` ~3274-3307). The largest source of memset/memcpy in the encoder.
- `code_block_8x8` (300k calls): three 256-byte `[i32; 64]` locals per call (`mb.rs` ~2224-2250).
- `w.clone()` in the inter/intra candidate loop (`mb.rs` ~634, 646): copies the whole band
  bitstream so far for every candidate, so the cost grows with bitrate. This is the most
  significant remaining memcpy and needs a rollback design, not a hoist.
- `inter::mc_luma` (662k calls): its temporary buffers are zeroed. That lives in the shared
  decoder inter module, so it is out of scope for this lane.

Not hoisted in this lane. Each site is a few percent at most, and the cheap-looking ones need
proof that every field is fully rewritten before reuse, which is risky inside the 3700-line
mb.rs. The `w.clone()` fix needs a writer rollback that handles CABAC carry propagation into
already-written bytes. Both are left for a follow-up.

## Gates (server, host c/a/b auto)
- `cargo clippy -p ec-h264 --all-targets -- -D warnings`: exit 0.
- `cargo test --release -p ec-h264`: exit 0. Totals 81, 22 (1 ignored), 15 (1 ignored), 4, 1, 11, 1 passed; 0 failed.
- `cargo test --release -p ec-h264-syntax`: exit 0. 14 passed, 0 failed.
- Largest file: `crates/ec-h264/src/enc/mb.rs` at 3747 lines (limit 5000). `enc/mod.rs` is ~1250.

## Left
- Writer rollback instead of `w.clone()` (above).
- Zeroed scratch for `Levels` in `encode_inter_mb`.
- Scratch for `code_block_8x8` locals.
