# Lane gateaudit: DONE.md gate matrix against the evidence

Base: `f5eb855b` (main), branch `lane/gateaudit`, worktree `~/.cache/wt/gateaudit`.
Read-only audit. No DONE.md criteria were edited. The only compute was one workspace clippy
run on a server (see G1). Evidence is cited as `lanes/<name>.report.md` (or `bench.md`/`bench.log`)
with a section, table row or line, or as a commit on main.

Status legend: **MET** (evidence found), **NOT MET** (evidence says no, or the item is missing),
**UNKNOWN** (no evidence; the stated command would settle it). Where a row says "integ", the
evidence is the edith test on branch `codecs-integration` (worktree `~/.cache/wt/edith-integ`,
head `d12ba5b`). That branch is not on edith main yet, so the edith-side part of any gate is
MET only on that branch.

## Shared gates (apply to every crate; per-crate tables below repeat the result)

| Gate | Result | Evidence |
|---|---|---|
| G1 clippy `-D warnings` | **Pending server run.** Per-crate status below is "MET on lane evidence" until the run is in. | Main has `e88ea83e` (alacclippy, workspace clean), `4dc330d3` (av1clippy3, ec-av1 clean on all targets). Run: `scripts/remote-run.sh gateaudit -- cargo clippy --workspace --all-targets -- -D warnings`, log in scratchpad `gateaudit-clippy.log`. It was still queued for a server slot at the time of writing. |
| G2 fuzz, 1 CPU-hour per target | Mostly MET. Table per crate. | `lanes/fuzz1hvideo.report.md` (table, 3,600 s each, 6 targets clean), `lanes/fuzz1haudio.report.md` (14 targets, 3,601 s), `lanes/fuzz1hcontainers.report.md` (36 of 37 clean), `lanes/fuzzh265av1.report.md:117-125` (4 targets), `lanes/fuzzmkv.report.md` (7 targets). |
| G3 no panic on malformed input | Mostly MET by fuzz runs plus replay tests. Open: ec-av1 decode_stream (items 17, 18 in `fuzzh265av1.report.md:129-130`), ec-image decode timeouts. |
| G4 external correctness | Per crate. |
| G5 edith integration test | Per crate. Edith tests exist on `codecs-integration` (`crates/engine/tests/*`). |
| G6 bench row | Rows in `lanes/bench.md` and `lanes/bench.log`. Note: DONE.md names `lanes/bench.log`, but the speed tables are in `lanes/bench.md` (242 lines). Both are used here. |
| G7 no source file over 5,000 lines | **MET everywhere.** Largest file per crate from `find crates/<c>/src`: ec-av1 `decode/rect.rs` 4,848; ec-aac `sbr_bands/vectors/part0.rs` 4,305; ec-h265 `enc_p.rs` 2,400; ec-h264 `enc/mb.rs` 3,748; ec-opus `celt.rs` 3,178. Splits: `lanes/av1splitdec`, `av1splitstream`, `av1splitencode`, `aacgates2` (`e51e3a34`). |

Process note: DONE.md's `ec-truehd` and `ec-ac3` "DONE 2026-10-17" lines are already in the file. Section 3 below finds that the ec-ac3 line is ahead of its gate 6 evidence.

## Per-crate matrix: P0 and P1 crates

### ec-h264 decode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| JVT progressive 8-bit 4:2:0 bit-exact | MET | `lanes/h264refusals.report.md:63-67`: 26 bit-exact, 9 refused by name, 0 failed (35 streams, `fetch-vectors.sh` h264-jvt set). |
| Out-of-scope refuse by name, with a test | MET | `crates/ec-h264/tests/refusals.rs:209-308`: interlaced/PAFF/MBAFF, FMO, ASO, SP, SI, data partitioning, High10, 4:2:2, 4:4:4, separate colour planes, each asserting `Unsupported` with the feature name. |
| real_library.toml bit-exact | MET | `lanes/h264reallib.report.md:26`: 15 decoded bit-exact, 0 refused, 0 failures. |
| 1080p30 >= realtime | MET, threads not recorded | `lanes/bench.log:145`: `ec-h264 decode 1920x1080 film, 302 frames`, 2.61x. The thread count is not in the row. UNKNOWN which thread count gave 2.61x (command: rerun `h264_clip_decode` with `EC_THREADS` recorded). |
| `dpb.rs:875` expect becomes an error | MET | `lanes/h264gates.report.md` item 1: `Dpb::store` returns `Error::corrupt`; test `dpb::tests::reference_picture_without_marking_is_corrupt_not_a_panic`. |
| G1 clippy | pending | `lanes/h264gates.report.md` (clippy clean on the lane). Workspace run pending. |
| G2 fuzz 1 h x 2 targets | MET | `lanes/fuzz1hvideo.report.md` table: decode_annexb 831,002 runs; decode_avcc 340,196 runs, 0 crashes, EXIT 0, code `dcc64530`/`ee94d667` (harness cap on avcc only). |
| G3 no panic | MET | `ca5230f2`, `201fa706`, `d29395a0`, `5e7b7583` (overflow fixes with replay tests). |
| G5 edith test | MET on integ | `crates/engine/tests/sw_backends.rs` and `export.rs` reference ec_h264 (integ). |
| G6 bench row | MET | `bench.log:145`. |

Verdict: **MET** on lane evidence (pending G1). Thread count of the 2.61x row is the one caveat.

### ec-h264 encode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| Output decodes in ffmpeg identical to encoder recon | MET | `lanes/h264encrc.report.md:5-7` ("28/28 md5 matches"). |
| Rate control within ±5% over 60 s | MET | `lanes/h264encrc.report.md:63` (60 s, 30 fps, 6/6 targets), `lanes/h264encspeed2.report.md:17` (rate within 5%). Edge: 1080p 1 Mb/s at +4.83%. |
| BD-rate vs x264 veryfast <= +20% | MET | `lanes/h264encspeed2.report.md:15`: +12.46% (limit 20). |
| 1080p30 encode >= realtime with threads | MET, local | `lanes/bench.md:91-96`: 41.72 fps at 6 threads, 1.39x, 12-core local (noisy testsrc2). Host a shows 0.285x (`bench.md:90`), so the speed bar depends on the machine (D7). |
| Shared gates | G1 pending; G2 N/A (no untrusted entry point); G5 MET on integ (`export.rs`). | |

Verdict: **MET** on lane evidence (pending G1). Proposed line in section 4.

### ec-h265 decode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| JCT-VC HEVC_v1 Main/Main 10 bit-exact vs ffmpeg/HM | MET | `lanes/hevcvps.report.md:42`: 9203 pictures, 9203 exact, 147/147 accepted. `KNOWN_INEXACT` is empty (`crates/ec-h265/tests/conformance_intra.rs:539`). Three streams' reference is the package MD5 (ffmpeg was wrong for them, `hevcvps.report.md:5-16`). |
| Out-of-scope refused by name, with test | MET | `crates/ec-h265/tests/refusals.rs`: 15 `*_is_refused_by_name` tests (RExt, SCC, multi-layer, 3D, monochrome, 12-bit). |
| real_library HEVC incl. phone clips bit-exact | **NOT MET** (external blocker) | 26 HEVC library files bit-exact (`3f7b0190`, per the gate notes); no HEVC phone footage on this machine. Test `real_library_hevc_streams_match_ffmpeg` is ready. |
| 1080p30 >= 2x realtime, 4K30 >= realtime, <= 6 threads | MET, local | `lanes/bench.md:127-148` (`hevcdecbench`): 1080p 6 thr 5.18x; 4K 6 thr 1.52x. |
| Dolby Vision 8.x base layer decodes | UNKNOWN | Command: a test that decodes a DV profile 8 stream (none located; `crates/ec-h265/tests/dec_meta.rs` has a Dolby mention only). |
| HDR SEI to tonemap | MET | `lanes/hevcsei.report.md`; integ `hevc.rs`. |
| Edith side: needs_plugin HEVC refusal removed | MET on integ | `crates/engine/src/decode/hevc.rs` (integ), not on main. |
| G1 clippy | pending | `lanes/hevcclippy.report.md`, commit `e05ce582`. |
| G2 fuzz | MET | `lanes/fuzzh265av1.report.md:119-121`: decode_annexb 425,673 runs, parse_params_slice 8,107,128 runs, both clean exit 0 on the final tree. |
| G3 no panic | MET | 18 crash fixes with replay tests (`fuzzh265av1.report.md`). |
| G5 edith test | MET on integ | `crates/engine/tests/hevc.rs`, `hevc_sw.rs`, `hevc_mkv.rs`. |
| G6 bench row | MET | `bench.md:127-148`. |

Verdict: **NOT MET**. Blocked by the phone-clip criterion (external footage). DV base-layer test is UNKNOWN.

### ec-h265 encode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| Output decodes in ffmpeg identical to recon (intra and inter) | MET | `lanes/hevcencrc.report.md:53-60` (`ec-h265 decode == recon`, `ffmpeg decode == recon`); `hevcencrcpar.report.md:82` (`hevc-gop-roundtrip.sh`, six rows exact). |
| Rate control ±5% over 60 s | MET | `lanes/hevcencrc.report.md:6, 28`; `hevcencrcpar.report.md:29`. |
| BD-rate vs x265 veryfast <= +25% | MET | `lanes/hevcenctools.report.md:8` (-5.31 / +1.5 / +6.9%); later `hevcencrcpar` +21%. |
| Intra-only BD-PSNR >= -1.0 dB vs x265 intra | UNKNOWN | No x265-intra BD-PSNR row in `lanes/hevc*.report.md`. Command: `scripts/hevc-bdrate.sh` with an intra-only clip against x265 `-preset veryfast` intra. |
| 1080p30 >= realtime on <= 6 threads | MET, local | `lanes/bench.md:182-215`: 40.16 fps at 6 thr (1.34x) (`hevcencspeed5`, merge `8fb9fb6c`). |
| G2 fuzz | N/A | No untrusted entry point (bitstream writer). |
| G5 edith test | MET on integ | `hevc_export.rs`. |

Verdict: **NOT MET** only on the intra-only BD-PSNR criterion (UNKNOWN). One command settles it.

### ec-av1 decode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| AOM conformance profile 0 8/10-bit bit-exact vs libaom/dav1d | **UNKNOWN** | No conformance test in `crates/ec-av1/tests/` (only `real_library.rs`, `fuzz_regressions.rs`, `data/`). Command: run the AOM profile 0 conformance set through the decoder and compare md5 to libaom. |
| real_library AV1 entries bit-exact | MET | `crates/ec-av1/tests/real_library.rs`; 2 AV1 library files bit-exact at `3f7b0190` (scratchpad gate notes, not a lane report). |
| 1080p30 8-bit >= realtime with EC_AV1_THREADS > 1 | MET, local | `lanes/bench.md:162-175` (`av1decbench`): 3.12x at 2 threads, 3.41x at 4 threads (md5 = ffmpeg). 6 threads reads 1.00x in that run (noisy). Default is min(4, 6) = 4 (`bench.md:75`). |
| `decode.rs` and `stream.rs` split to <= 5k | MET | Largest now 4,848 (`decode/rect.rs`) and 4,594 (`stream/tests/t03.rs`). Lanes `av1splitdec`, `av1splitstream`. |
| clippy uninit_vec (decode.rs:22362, film_grain.rs:1299), mut_from_ref (par.rs:192) | MET | `4d8ca6f3` (av1clippy merge), `4dc330d3` (av1clippy3: crate clippy exits 0 on all targets). |
| Edith: non-4:2:0 AV1 refused at demux with a named reason | MET on integ | `crates/engine/src/demux.rs` (integ, lane `edith-av1420`). Not on main. |
| G1 clippy | MET on lane evidence (`4dc330d3`), pending workspace run | |
| G2 fuzz | **NOT MET** | `lanes/fuzzh265av1.report.md:125`: parse_obu 8,597,687 runs clean (1 h). decode_stream: the earlier clean 1 h run "is superseded" and no 1 h run exists on the final tree (`:125`). |
| G3 no panic | **NOT MET** | Items 17 and 18 in `fuzzh265av1.report.md:129-130`: "root cause open". A gate-level flake also: `stream::tests::t03` failed once on main `ca5230f2` (gate notes). |
| G5 edith test | MET on integ | `crates/engine/tests/av1.rs`, `av1_software_export.rs`. |
| G6 bench row | MET | `bench.md:162-175`. |
| G7 | MET | Max 4,848 lines. |

Verdict: **NOT MET**. Conformance UNKNOWN, decode_stream 1 h not run on the final tree, and open panic root causes.

### ec-vp9 decode (P1)

| Criterion | Status | Evidence |
|---|---|---|
| edith uses ec-vp9 as software backend; needs_plugin VP9 refusal removed | MET on integ | `crates/engine/src/decode/vp9.rs` (integ). Main still has the `vp9_needs_420_for_vaapi` predicate (gate notes). |
| Profile 0 and profile 2 libvpx vectors bit-exact | MET | `lanes/vp9threads.report.md:9`: 305 of 305 vectors clean, `KNOWN_FAILING` empty. Merge `1add6ec7`. |
| 1080p30 profile 0 >= realtime (tile threads allowed) | MET on host b; local UNKNOWN | `lanes/bench.md:36-56`: tile1080.ivf 1.33x at 2 threads, 1.65x at 4; libvpx 1080p clip 1.95x at 1 thread (host b). No local row, but D7 says the speed bar is measured locally. |
| Dead code removed (read_partition, TM_PRED, gen_tables allow) | MET | `lanes/vp9gates.report.md` (gate notes). |
| Stale reports superseded (vp9inter, vp9hbd) | MET | Headers say SUPERSEDED 2026-10-15 (gate notes). |
| G1 clippy | MET on lane evidence (`vp9threads`), pending workspace run | |
| G2 fuzz 1 h x 2 | MET | `lanes/fuzz1hvideo.report.md` table: decode_frame 1,017,368 runs; decode_stream 4,266,085 runs; 0 crashes. |
| G3 | MET | `ca5230f2`, `66483de4`, `b63402c3` (OOM and overflow fixes). |
| G5 edith test | MET on integ | `crates/engine/tests/vp9.rs`. |
| G6 bench | MET (host b) | `bench.md:36-56`. |

Verdict: **PARTIAL**. Only the local speed row is missing (S). Note: `vp9gates` reports 12 failing tests, but those are fixture-missing `NotFound` on host a (`vp9gates.report.md:53-62`). `vp9threads` replaces this state with the fixtures present and `EC_REQUIRE_FIXTURES`. Not a criterion, but the tests should panic, not fail, without fixtures.

### ec-vp8 decode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| edith calls ec-vp8 from engine directly | MET on integ | `crates/engine/src/decode/vp8.rs` (integ), `tests/vp8.rs`. |
| 12/12 IVF fixtures bit-exact | MET | `lanes/vp8.report.md` (byte-exact vs ffmpeg), `edith-vp8engine`. |
| Release guards at loopfilter.rs:458/584; panics at intra.rs:114/365 become errors | MET | `lanes/vp8guards.report.md`, `vp8fuzzfix1`. |
| G1 clippy | MET on lane evidence, pending workspace run | |
| G2 fuzz 1 h x 2 | MET | `lanes/fuzz1hvideo.report.md`: decode_frame 2,265,883 runs; decode_ivf 1,917,513 runs, both capped harness, 0 crashes. |
| G6 bench row | **NOT MET** | No `ec-vp8` row in `bench.md` or `bench.log` (grep count 0). Command: one `ec-bench` vp8 decode row. |

Verdict: **NOT MET**, only the bench row (S).

### ec-aac decode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| All AAC routes through ec-aac, mono and stereo | PARTIAL | integ `crates/engine/tests/audio_aac.rs` and `audio.rs` use ec_aac. Edith's HE-AAC v2 refusal in mp4 is not confirmed either way on integ (grep found none in `demux.rs`): UNKNOWN. |
| AAC-LC and HE-AAC v1, mono to 7.1, raw and ADTS, corr >= 0.999 per channel | PARTIAL | PS (HE-AAC v2) fixtures: all four >= 0.9997 (`lanes/aacpstone.report.md:57-70`). PS in mp4 corr 0.999977 (`lanes/aacgates2.report.md:52`). Mono, 5.1 and 7.1 vs ffmpeg not evidenced. Command: a per-channel correlation over the mono/5.1/7.1 fixtures against ffmpeg. |
| sbr_hf.rs unwraps become errors | MET | `lanes/aacgates.report.md:8-12`. |
| HE-AAC v2 PS decode (D4) | MET in ec-aac | `aacpstone` (sine fixtures corr 0.708 to 1.000); `aacgates2` item 5: no PS refusal in ec-aac or ec-mp4. |
| G1 clippy | MET on lane evidence (`aacgates2`, `aacpstone`), pending workspace run | |
| G2 fuzz 1 h x 2 | MET | `lanes/fuzz1haudio.report.md:18-19`: decode_adts 668,182 runs; decode_raw 384,441 runs, EXIT 0. |
| G3 | MET | `aacgates.report.md:29-36` (truncation and corruption never panic). |
| G6 bench row | MET (encode row) | `bench.log:194`: ec-aac encode 26.99x. Decode: via ec-probe. |
| G7 | MET | Largest 4,305 (`sbr_bands/vectors/part0.rs`). |

Verdict: **NOT MET**. Multi-channel and mono correlation vs ffmpeg is not evidenced, and the edith side is UNKNOWN.

### ec-aac encode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| LC CBR 1-6 and 8 channels pass INCUMBENT_BAR | **UNKNOWN** | Only stereo and 5.1 were checked (`lanes/aacgates2.report.md:24-26`, 83-86). Command: `cargo test -p ec-aac --test oracle` for each channel count. |
| Objective score vs ffmpeg native AAC at 128 kbps stereo recorded | MET | `lanes/aacgates2.report.md:60-67`: ffmpeg 16.66 to 32.14 dB segSNR vs ec-aac 7.01 to 20.42 dB (`scripts/aac-encode-score.sh`). |
| Rate within ±5% | MET | `lanes/aacgates2.report.md:83-86`: stereo 64 +4.4%, stereo 128 +2.3%, stereo 256 +1.3%, 5.1 384 +0.9%. |

Verdict: **NOT MET** (INCUMBENT_BAR at 1-6 and 8 channels UNKNOWN).

### ec-opus decode and encode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| 12 RFC 6716 vectors pass, recorded | MET | `f48f1d10`: RFC 6716 12/12 and RFC 8251 `.dec` 12/12. `crates/ec-opus/tests/conformance.rs`. |
| RFC 8251 vectors added and pass | MET | `f48f1d10`. |
| celt unwraps become errors | MET | `68d4d2fd` (CELT unwraps to errors). |
| Encode: SILK packet budget enforced | MET | `f48f1d10`: 0 over on 15 rate rows, rate +0.00%. |
| Encode: rate within ±5% | MET | `f48f1d10` (same). |
| No FAIL rows in dropout gate (reelm@64k) | MET | `b87177d9`: reelm@64k gate passes. |
| G1 clippy | MET on lane evidence, pending workspace run | |
| G2 fuzz 1 h x 2 | MET | `lanes/fuzz1haudio.report.md:20-21`: decode_packet 598,606 runs (fixed code, EXIT 0); parse_toc 1,139,827,568 runs. |
| G5 edith test | MET on integ | `opus_export.rs`, `audio_decode.rs`, `audio_seek.rs`; `audio.rs:1780` drives MultistreamDecoder. |
| G6 bench row | MET | `bench.log`: ec-opus encode row; decode through ec-probe. |
| G7 | MET | Largest 3,178 (`celt.rs`). |

Verdict: **MET** on lane evidence (pending G1). Proposed line in section 4.

### ec-vorbis (P0)

| Criterion | Status | Evidence |
|---|---|---|
| Shared gates | G1 pending; G2 MET; G3 MET (fuzz); G5 MET on integ; G6 MET; G7 MET | `fuzz1haudio.report.md:22-23`: decode (capped) 117,703 runs, setup 1,461,429 runs; `audio_vorbis.rs` (integ); `bench.log` Vorbis rows (ec-probe) and encode 510x. |
| Quality: 14/14 PASS at equal size vs libvorbis | MET | `lanes/vorbis-rate-r1.report.md`. |

Verdict: **MET** on lane evidence (pending G1). Proposed line in section 4.

### ec-mp3 encode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| Shared gates | G1 pending; G2 N/A (encode only); G4 MET; G5 MET on integ; G6 MET; G7 MET | `lanes/mp3lame-r1.report.md`: CBR matches or beats LAME on 14 rows, hein@192 0.2 dB behind (accepted). `crates/engine/tests/audio_export.rs` (32 mp3 references, integ). `bench.log`: ec-mp3 encode 3.0 s row. |

Verdict: **MET** on lane evidence (pending G1). Proposed line in section 4.

### ec-ac3 decode (P0)

| Criterion | Status | Evidence |
|---|---|---|
| AC-3 and E-AC-3 match ffmpeg on real_library through 5.1 | MET | `lanes/truehdexact.report.md`; `crates/ec-ac3/tests/real_library.rs`: corr >= 0.999 per channel on 2 entries. Output is not bit-exact (dither). |
| E-AC-3 7.1 dependent substreams (D5) | MET | Merge `29227519`; `crates/ec-ac3/tests/dependent.rs`. |
| G2 fuzz 1 h x 2 | MET | `fuzz1haudio.report.md:24-25`: decode_stream 1,370,296 runs, parse_header 230,658,709 runs. |
| G5 edith test | MET on integ | `audio_ac3_film.rs`, `audio_decode.rs`. |
| G6 bench row (decode) | **NOT MET** | `bench.log:218` is an encode row only (55.29x). No AC-3 decode row. |

Verdict: **NOT MET**. The DONE line in DONE.md (2026-10-17) is ahead of gate 6. Either add the decode row (S) or withdraw the line.

### ec-truehd decode (P1)

| Criterion | Status | Evidence |
|---|---|---|
| 2/6/8-channel substreams bit-exact vs ffmpeg | MET (7.1 by local remux only) | `lanes/truehdexact.report.md:13-16`: all five committed fixtures bit-exact (`fixture_exact.rs`); 7.1 from a local remux via `EC_TRUEHD_71_REMUX` (not committed, not reproducible from the repo). |
| Edith decodes TrueHD through ec-truehd from MKV | MET on integ | `crates/engine/tests/audio_truehd.rs`. |
| G2 fuzz 1 h x 2 | MET | `fuzz1haudio.report.md:26-27`: decode_stream 559,795 runs, parse_sync 654,187,150 runs. |
| G6 bench row | MET | `bench.log`: ec-truehd decode row ("own encoded stream, stereo"). |
| G1 clippy | pending | |

Verdict: **MET** on lane evidence (pending G1). The DONE line already exists. The 7.1 proof is not reproducible from the repo (caveat).

### Dependency-replacement crates (P0 once swapped; DONE.md "Done for each swap")

| Crate (swap) | Status | Gap |
|---|---|---|
| ec-inflate (miniz_oxide, wave 1) | NOT MET | G5 UNKNOWN: no edith test references it (1 src use). G2 MET (4 targets 1 h, `fuzz1hcontainers`). G6 MET (`bench.log:241`). |
| ec-riff (hound, wave 1) | NOT MET | G6: no bench row (`grep riff` 0). G2 MET (3 targets 1 h). G5 MET (`speed.rs`, `audio_export.rs`). RF64 MET (`swaphound`). |
| ec-flac (flacenc, wave 1) | NOT MET | flacenc level and bit-depth parity UNKNOWN (`swapflacenc`). G2 MET (4 targets). G6 MET (encode 168x). |
| ec-mp4 (mp4, wave 2) | PARTIAL | G2 MET (3 targets 1 h). G4 MET (`mp4demux`/`mp4mux`). Sample entries av01, ac-3, Opus, alac: `mux_entries.rs` exists; alac not confirmed (UNKNOWN). G6 MET (187 MB demux row). |
| ec-hw, ec-va (cros/gbm, wave 2) | NOT MET | Local fixes as ec-hw regression tests: partly ported (`hwgaps`, `hwfixes`). Fuzz requirement for VA-API builders is not stated in DONE.md (UNKNOWN). G5 MET on integ (`hw_encode.rs`). |
| ec-subs, ec-ass, ec-pgs (wave 3) | NOT MET | G2 MET (11 targets 1 h, `fuzz1hcontainers`). G5 NOT MET (no edith test references). G6 NOT MET (no bench row). G4 PARTIAL (`swapsubs`). |
| ec-ogg (wave 3) | NOT MET | G4 partial (Vorbis parity only; no Opus-in-Ogg writer, which edith does not use). G5 NOT MET (no test). G6 NOT MET (no row). G2 MET (5 targets 1 h). |
| ec-image (wave 3) | NOT MET | G2 NOT MET: `ec-image decode` target aborts on libFuzzer timeouts, OPEN (`fuzz1hcontainers.report.md:4, 62-76`). G6 MET (decode rows). G5 MET on integ (`images.rs`). |
| ec-matroska (optional consolidation) | not required | G2 MET (7 targets 1 h, `fuzzmkv`). No edith reference (src 0). Not in the DONE P0 list. |

### Out of scope for this matrix (no DONE entry)

ec-av1 encode (frozen), ec-alac, ec-probe, ec-core, ec-dsp, ec-*-syntax crates.

## 3. Summary counts

- P0/P1 crates in DONE.md: 11 (ec-h264, ec-av1, ec-h265, ec-vp9, ec-vp8, ec-aac, ec-opus, ec-vorbis, ec-mp3, ec-ac3, ec-truehd).
- **Fully met on lane evidence (pending G1): 5.** ec-h264 (decode and encode), ec-opus, ec-vorbis, ec-mp3, ec-truehd. ec-truehd already has its DONE line.
- Partial (one or two items): ec-vp9 (local speed row), ec-vp8 (bench row), ec-ac3 (decode bench row; DONE line already applied, see below), ec-h265 encode (intra BD-PSNR UNKNOWN).
- Not met: ec-av1 decode, ec-h265 decode, ec-aac (decode and encode).

Note on ec-ac3: its DONE line (2026-10-17) is in DONE.md ahead of its gate 6. Add the decode bench row or withdraw the line.

## 4. Proposed "DONE 2026-10-17" lines

Apply only after the G1 workspace clippy run is clean for these crates.

- DONE 2026-10-17 ec-h264 decode and encode: JVT 26 bit-exact, 9 refused by name with tests (`h264refusals`, `refusals.rs`); real_library 15/15 (`h264reallib`); encode recon 28/28 md5 vs ffmpeg, rate within 5%, BD-rate +12.46% vs x264 veryfast (`h264encrc`, `h264encspeed2`). Edith tests are on codecs-integration.
- DONE 2026-10-17 ec-opus decode and encode: RFC 6716 12/12 and RFC 8251 12/12, SILK packet budget enforced, rate +0.00%, reelm@64k dropout gate met (`opusgate`).
- DONE 2026-10-17 ec-vorbis: 14/14 PASS vs libvorbis at equal size (`vorbis-rate-r1`); fuzz 1 h clean (`fuzz1haudio`).
- DONE 2026-10-17 ec-mp3 encode: CBR matches or beats LAME on 14 rows, hein@192 0.2 dB behind accepted (`mp3lame-r1`).

ec-truehd already has its line; keep it.

## 5. Remaining work, ordered by tier (P0 first)

Sizes: S = under about 1 h of server time or a single code/doc edit; M = a lane of a day or less; L = multi-day or research.

### P0

1. **ec-av1 decode: 1 h decode_stream on the final tree.** S (server, 1 h).
2. **ec-av1 decode: root-cause the open panic items 17 and 18** (`fuzzh265av1.report.md:129-130`, palette/intrabc prediction size and CfL). M.
3. **ec-av1 decode: AOM conformance profile 0 8/10-bit bit-exact vs libaom/dav1d.** No evidence exists. Needs the conformance set and a harness. L.
4. **ec-h265 decode: phone HEVC clip bit-exact vs ffmpeg** (iPhone and Android 8/10-bit HDR). Blocked on footage, which the machine does not have. External. S once footage exists.
5. **ec-h265 decode: Dolby Vision profile 8 base-layer decode test.** S.
6. **ec-h265 encode: intra-only BD-PSNR >= -1.0 dB vs x265 intra.** S (`scripts/hevc-bdrate.sh`, intra clip).
7. **ec-aac decode: mono, 5.1 and 7.1 raw and ADTS correlation >= 0.999 per channel vs ffmpeg.** M.
8. **ec-aac decode: confirm edith no longer refuses HE-AAC v2 in mp4 (integ).** S.
9. **ec-aac encode: INCUMBENT_BAR at 1-6 and 8 channels.** M.
10. **ec-vp8 decode: one bench row (`ec-bench` vp8 decode).** S.
11. **ec-vp9 decode: local 1080p profile 0 bench row** (D7 says local). S.
12. **ec-ac3 decode: AC-3 decode bench row, or withdraw the DONE 2026-10-17 line.** S.
13. **ec-h264 decode: record the thread count for the 2.61x row** (`bench.log:145`). S.
14. **Workspace clippy G1 for the 11 P0/P1 crates.** S (run in progress; if it fails, fix each crate).

### P1 and dependency crates (P0 once swapped)

15. **ec-image decode: close the libFuzzer timeouts (1 h clean).** M.
16. **ec-hw and ec-va: state whether a fuzz target is required; port the local cros/gbm fixes as ec-hw regression tests.** M.
17. **ec-flac: flacenc compression level and bit-depth parity.** M.
18. **ec-subs, ec-ass, ec-pgs: an edith test and a bench row each.** M.
19. **ec-ogg: an edith test and a bench row.** S to M.
20. **ec-riff: a bench row; ec-inflate: an edith test.** S each.
21. **ec-mp4: confirm the alac sample entry (and Opus/av01/ac-3 are covered by `mux_entries.rs`).** S.
22. **edith side: merge `codecs-integration` onto edith main.** The gate notes say this is the user's decision, so it is listed for visibility and is not a lane task.

Top 10 for the summary: items 1, 2, 3, 4 (external), 6, 7, 9, 10, 12, 14.

## Verify

- Read: `DONE.md` (all), `lanes/LANE-PREAMBLE.md`, scratchpad `gate-matrix.md` (read only), `git log --oneline main | head -400` (saved to the scratchpad as `gateaudit-log.txt`), the lane reports cited above, `lanes/bench.md`, `lanes/bench.log`, and the crate test and fuzz directories.
- `scripts/link-fixtures.sh` run in the worktree: exit 0.
- `scripts/remote-run.sh gateaudit --host auto -- cargo clippy --workspace --all-targets -- -D warnings`: started in the background. Result pending at report time (see G1).
- No DONE.md criteria were changed. No code was changed.
