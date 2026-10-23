# lane h264encclone: no per-candidate writer clone in the ec-h264 mode decision

Base sha: 2a1f421d (lane base, equal to main at lane start). Branch lane/h264encclone.

## Goal
`crates/ec-h264/src/enc/mb.rs` copied the whole band bitstream (`w.clone()`) for every inter
candidate and for the intra candidate of each macroblock. The copy grows with the bytes written so
far. Remove it, keeping output byte-identical.

## Design
Mark and rollback was rejected: CABAC state (low, range, outstanding, contexts) would need a
snapshot anyway, and the winner's bits would have to be saved and restored. The chosen design is a
probe-and-adopt pair, which reuses the existing `probe_copy` (state without the output buffer,
written for the costing probes at `mb.rs` `entropy_bits`):

- Each candidate is coded into `w.probe_copy()`: the same coder state as `w`, plus an empty writer
  at the same bit alignment (`aligned_empty`, so PCM alignment inside the trial is unchanged).
  `bit_len` differences are the same as before.
- The winning trial is handed to `EncEntropy::adopt(trial)`. For CABAC, `CabacEnc::adopt`
  destructures the trial exhaustively (a new field fails to compile), copies every state field and
  appends the trial's bits after `w`'s bits. For CAVLC, `skip_run` is copied and the bits appended.
- `entropy::append_bits(dst, src, from)` appends `src`'s bits from `from` (the alignment the probe
  started at) to its end, bit by bit. No bit already written to `w` is changed. The CABAC engine
  writes bits through `outstanding` bits and never back-patches earlier bytes, so there is no carry
  into written bytes to handle.

Why this is exact: during a candidate, every write depends only on the coder state and the bit
alignment, both of which the probe carries. So the bits and final state of a trial equal what
writing to `w` directly would produce. `w` is not written between the probe and the adopt.

## Changes
- `crates/ec-h264/src/enc/entropy.rs`: `EncEntropy::adopt`, `append_bits`.
- `crates/ec-h264/src/enc/cabac_enc.rs`: `CabacEnc::adopt`.
- `crates/ec-h264/src/enc/mb.rs`: the two `w.clone()` sites (inter candidates, intra candidate)
  become `w.probe_copy()`; the two `*w = ...` assignments become `w.adopt(...)`. 3748 lines before and after (limit 5000; `mb.rs` is the largest file in the crate).
- `lanes/bench.md`: `## lane h264encclone` rows.

## Byte identity
Matrix: two 1920x1080 clips, 60 pictures each (`tsrc`: 2 s of testsrc2 with temporal noise, 30 fps,
generated with `ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30:duration=2 -vf noise=alls=20:allf=t`;
`nat`: the first 60 frames of `fixtures/video/h264-1080p-23.976-8bit.mp4`). Each run writes the
Annex-B stream and the encoder reconstruction through `h264_clip_encode`. Runs: QP 26, CABAC and
CAVLC, threads 1, 4 and 6; 4 Mb/s rate control at 4 threads, CABAC and CAVLC; QP 12, CABAC, 1
thread. 18 runs, 36 md5s.

Baseline md5s were recorded on the untouched lane base before any code change (server host b). The
changed tree was run on server host c. Both matrices used the same clips. Result: all 36 md5s are
identical (`diff` of the two lists, stream and recon columns, is empty).

| run | stream md5 (base, new) | recon md5 (base, new) |
|---|---|---|
| tsrc-q26-cabac-t1 | 4d5ec8103c894e9567bac531bcaa0ea5 4d5ec8103c894e9567bac531bcaa0ea5 | b3453b85ab2e48e71135a2d8e1315a60 b3453b85ab2e48e71135a2d8e1315a60 |
| tsrc-q26-cabac-t4 | 5397aae64b26036ef420483d81eca9d3 5397aae64b26036ef420483d81eca9d3 | eb0e2b8b5184a97895b27af9b6d76904 eb0e2b8b5184a97895b27af9b6d76904 |
| tsrc-q26-cabac-t6 | c73a63ed9aedaf0f0a964888524e88d8 c73a63ed9aedaf0f0a964888524e88d8 | 0c31bb059df20cd0bc436fe76383d13c 0c31bb059df20cd0bc436fe76383d13c |
| tsrc-rc4m-cabac-t4 | ec76276afebc3a086cdd6a57a284481c ec76276afebc3a086cdd6a57a284481c | 5bb17940326039dcb9bf2eaf18f12fdd 5bb17940326039dcb9bf2eaf18f12fdd |
| tsrc-q26-cavlc-t1 | 8d43513764ec73b55ca9943b46ba7017 8d43513764ec73b55ca9943b46ba7017 | 7e98dd2f0b495b1c9d84521ff735d292 7e98dd2f0b495b1c9d84521ff735d292 |
| tsrc-q26-cavlc-t4 | 6876848fe7d4cdcf3ea84394b6b43b6c 6876848fe7d4cdcf3ea84394b6b43b6c | a0e87081c2480bfb1bdac5eafc36bee8 a0e87081c2480bfb1bdac5eafc36bee8 |
| tsrc-q26-cavlc-t6 | 010fad4dd7d3693a17454a37ce143bef 010fad4dd7d3693a17454a37ce143bef | 2fa1f749f11cb70f65778ef85fd49a7d 2fa1f749f11cb70f65778ef85fd49a7d |
| tsrc-rc4m-cavlc-t4 | c231e72914aaac11e99f5af428d3100f c231e72914aaac11e99f5af428d3100f | cee284a2997533d20a73e548e04936ff cee284a2997533d20a73e548e04936ff |
| tsrc-q12-cabac-t1 | 5da1bdebf931245aad3c7badb3fdb234 5da1bdebf931245aad3c7badb3fdb234 | 61cd1363fb9989808d99a899f691bb72 61cd1363fb9989808d99a899f691bb72 |
| nat-q26-cabac-t1 | ee44f82e221dea4e92cc40fdf8d21640 ee44f82e221dea4e92cc40fdf8d21640 | d8bc1103bd15b170fdb35daaa5aa8896 d8bc1103bd15b170fdb35daaa5aa8896 |
| nat-q26-cabac-t4 | a58067568ddf3290db9ac630837d35fd a58067568ddf3290db9ac630837d35fd | fe65125b9af8a3345dc3ea68d5c2d966 fe65125b9af8a3345dc3ea68d5c2d966 |
| nat-q26-cabac-t6 | 62333565620226be62aae0b07cf9dfc7 62333565620226be62aae0b07cf9dfc7 | 381fadd9d1c442ebcec3d96ea8b2e078 381fadd9d1c442ebcec3d96ea8b2e078 |
| nat-rc4m-cabac-t4 | a949036103cde89fb9db050af1833a24 a949036103cde89fb9db050af1833a24 | 63175062929f399d889db14f28bbab62 63175062929f399d889db14f28bbab62 |
| nat-q26-cavlc-t1 | b8f77b383545706bcba0a4fe7727f02b b8f77b383545706bcba0a4fe7727f02b | d5b08f3f53eb94b6229ced01fefd211a d5b08f3f53eb94b6229ced01fefd211a |
| nat-q26-cavlc-t4 | 9f8a2e1ab491994aa54a36059ad75635 9f8a2e1ab491994aa54a36059ad75635 | bbc901b4cc3f9d86423a918b59addef0 bbc901b4cc3f9d86423a918b59addef0 |
| nat-q26-cavlc-t6 | cc4f054683ebad42f5703cabee31fe04 cc4f054683ebad42f5703cabee31fe04 | 99ff19693bd11a3e3d7352c136a7ae6c 99ff19693bd11a3e3d7352c136a7ae6c |
| nat-rc4m-cavlc-t4 | baa9f60c843fdd678a59b60b6ad39b55 baa9f60c843fdd678a59b60b6ad39b55 | eb60380b3dd48cb4e8c3a35aa9813f87 eb60380b3dd48cb4e8c3a35aa9813f87 |
| nat-q12-cabac-t1 | 660b63ef29a11f623f5223d0e58f7ef5 660b63ef29a11f623f5223d0e58f7ef5 | b08c03ec08edb5a4ea94c935663b16d6 b08c03ec08edb5a4ea94c935663b16d6 |

Local bench runs of the base and changed binaries also produced the same byte counts in every run
(1995923 and 2002474 bytes at 4 Mb/s, 218673353 and 218797417 bytes at QP 12).

## Gates (server host b, `scripts/remote-run.sh h264encclone --host auto`)
- `cargo clippy -p ec-h264 --all-targets -- -D warnings`: exit 0, no warnings.
- `cargo test --release -p ec-h264`: exit 0. Totals 81, 22 (1 ignored), 15 (1 ignored), 4, 1, 11, 1
  passed; 0 failed.
- `cargo test --release -p ec-h264-syntax`: exit 0. 14 passed, 0 failed.

## Speed
Wall-clock fps is not usable here: the machine was shared (load average 12 to 72). The fps rows
are in `lanes/bench.md` under `## lane h264encclone`. Medians of 3 (before, after): 4 Mb/s 1 thread
13.28, 13.66; 4 Mb/s 6 threads 43.37, 34.71; QP 12 1 thread 1.21, 0.90; QP 12 6 threads 4.19, 2.28.
They are noise-dominated and are not evidence for or against the change.

Cycle counts (`perf stat -e cycles:u,instructions:u`, 60 pictures, QP 12, 1 thread, two alternating
rounds): cycles before 275.9 G and 293.6 G, after 267.5 G and 260.8 G. Instructions 832.8 G before
and 832.7 G after. `perf record` at the same setting: `memmove` 2.2% of cycles before, 1.6% after.
The copy was a small share, so the gain is a few percent at high bitrate. Cycle counts are less
affected by the load than wall time, but the sample is two rounds.

## Left
- Speed claim needs a quiet machine and more rounds at QP 12. Not measured on the server.
- The bit-by-bit `append_bits` costs a little per macroblock at high bitrate. A bulk path would
  need a `BitWriter` method in `ec-core`, which is outside this lane.
- The `Levels` zeroing and `code_block_8x8` locals noted in the pool report are untouched.
