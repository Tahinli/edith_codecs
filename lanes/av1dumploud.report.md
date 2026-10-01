# lane/av1dumploud — every dump site fails loudly on a short write

**Outcome in one line:** all 11 dump write sites in `crates/ec-av1` now account
for their own byte count and abort the run with the file, the bytes on disk, the
bytes owed and the OS error; one real decode under a real `RLIMIT_FSIZE` is
reproduced as a FAILING run; the `hg_rect64_intra16x4_witness` hash sweep is
byte-identical to `lanes/unwritten-dep.report.md` §2 with the loud writer in
place; the campaign value `0ee1edf403` is **still not reproduced** and stays open
with the mechanism class now provably closed going forward.

Base: `f01e9738`. Branch: `lane/av1dumploud`. Worktree:
`/home/tahinli/.cache/wt/av1dumploud`. Target dir:
`/home/tahinli/.cache/target-av1dumploud`.

The class this lane closes is named in `lanes/unwritten-dep.report.md` §9 as an
open item: *"The dump sites still swallow their write errors, so a short dump
remains possible — the artefact behind §3."* That is now closed for every site.

---

## 1. Every dump write site, and what a FULL write means for it

New module: `crates/ec-av1/src/dumpio.rs`. `LoudDump::create(site, path,
expected)` opens the file, `write(chunk)` hands it bytes, `finish()` flushes and
then compares the file's **own length on disk** against `expected`. Any shortfall,
and any failure to create the file at all, panics with one fixed line:

```text
ec-av1 dump FAILED [<site>] <path>: wrote <on-disk bytes> of <owed> bytes: <reason>
```

| # | Site (file:fn) | Env var | File | Dump shape | FULL write = |
|---|---|---|---|---|---|
| 1 | `decode.rs:21147` `dump_stage16` | `EC_AV1_PREFILT_DUMP16`, `EC_AV1_POSTDEBLOCK_DUMP16`, `EC_AV1_POSTCDEF_DUMP16` | `<prefix>.f<N>` | luma + 2 chroma, u16 LE, cropped | `(fw*fh + 2*cw*ch) * 2`, `cw = round_ss(fw, ss_x)`, `ch = round_ss(fh, ss_y)` — the same per-axis extents the writer slices |
| 2 | `decode.rs:21235` `dump_stage` | `EC_AV1_POSTDEBLOCK_DUMP`, `EC_AV1_POSTCDEF_DUMP` | `<prefix>.f<N>` | whole padded plane, u8 | `y.data.len() + u.data.len() + v.data.len()` |
| 3 | `decode.rs:21259` `dump_prefilter_wide` | `EC_AV1_PREFILT_WIDE_DUMP` | `<prefix>.f<N>` | mi-aligned crop, u8 | `Σ true_width * true_height` over the three planes |
| 4 | `decode.rs:38237` (key-frame twin) | `EC_AV1_PREFILT_DUMP` | `<prefix>.f<N>` | whole padded plane, u8 | `Σ p.data.len()` |
| 5 | `decode.rs:56608` (inter-frame twin) | `EC_AV1_PREFILT_DUMP` | `<prefix>.f<N>` | whole padded plane, u8 | `Σ p.data.len()` |
| 6 | `decode.rs:38471` margin dump | `EC_AV1_MARGIN_DUMP` | `<prefix>.f<N>` | `true_width x true_height` luma crop, u8 | `true_width * true_height` |
| 7 | `stream.rs:2205` | `EC_AV1_DECODE_ORDER_DUMP` | `<prefix>.f<N>` | post-deblock, u8-narrowed | `y.len() + u.len() + v.len()` (narrowing is 1 B/sample) |
| 8 | `stream.rs:2252` | `EC_AV1_FINAL_DUMP` | `<prefix>.f<N>` | post-filter/superres, bit-depth correct | `(y.len() + u.len() + v.len()) * sample_width`, `sample_width = 1` at 8-bit else 2 — computed from the PLANES, not from the buffer, so a wrong buffer is still caught |
| 9 | `stream.rs:2176` | `EC_AV1_DUMP_TABLES` | `<path>` (APPEND) | 9 text lines per frame, one file for the whole run | **CANNOT BE KNOWN** — the file spans every frame, so no total length exists to declare. `append_unknown` says `of an unknown number of bytes` on the failure line instead of inventing one. Per-line write errors are still fatal. |
| 10 | `msac.rs:1031` `symtrace::ecdump` | `EC_AV1_ECDUMP` | `<dir>/ecdump-<n>.txt` | one text line per symbol, streamed for a whole run | **CANNOT BE KNOWN** — same reason; the file grows per symbol. Write errors are fatal and the line is named (the path now travels with the handle). |
| 11 | `msac.rs:1130` `symtrace::flush` | symtrace dir | `<dir>/<side>-<n>.txt` | one coder's buffered lines | `buf.len()` — `std::fs::write` returned a `Result` that was discarded; now fatal. **Result-only**: see the note under the table. |

**Row 11 is the one site verified by `Result` alone.** `std::fs::write` returns
only a `Result`, so unlike the other ten there is no handle to keep, no
`finish()`, and no second look at the file's own length on disk. It is still
loud on a short write — `Err` is fatal — but its check is the `Result` and
nothing more. Do not read the table's "FULL write" column as identical
verification for all eleven: rows 1–10 are checked twice (bytes handed in, then
the file on disk), row 11 once.

**Out of scope, named rather than skipped.** Three shapes:

* **Nine `#[cfg(test)]` stream-pin sites** — `stream.rs` ≈ 14191, 18201, 23170,
  23190, 23589, 23611, 27904, 28896, 29106. Each does `let _ = std::fs::write(
  &pin, &stream)` and then `panic!`s on the next line naming the pin. **A
  truncated pin is a live residue here, and it is not fixed by this lane**: the
  write result is discarded, so an `ENOSPC` or an `RLIMIT_FSIZE` hit partway
  through a multi-megabyte `.obu` leaves a SHORT stream at exactly the path the
  following `panic!` hands the reader ("stream pinned at {}"). "No decode
  measurement is built on the file" is true of the test's VERDICT — which comes
  from the assert, not the artefact — and is not true of the artefact. Named, not
  fixed: these are test-side pins, and making them loud would change what a
  failing gate prints rather than what a failing gate decides.
* **`stream.rs:28143`**, the tenth pin site and a different shape: it checks the
  `Result`, but **warns and continues** — `eprintln!("cdfflake: could not pin to
  {}: {e}", …)` — by explicit design, because its own comment says a failed
  write must not mask the assert below it. So neither the "they panic on the next
  line" reason above nor the "discards the result" one applies to it. It is
  excluded on the same ground as the nine (a `#[cfg(test)]` pin whose verdict
  comes from the assert), with the extra note that its own failure path is
  already deliberate and non-fatal.
* **`probe.rs:158`**: a stdin pipe to a child process, swallowed on purpose so
  the child's own exit status and stderr carry the diagnosis (the documented
  `aomenc-stdin` deadlock fix). Not a file at all.

### Success path

Byte-identical, by construction: same bytes, same order, same file names, same
per-var index (`dump_stage_idx` is still only called when the var is set, and
still once per frame). `dumpio::tests::success_path_is_byte_identical_to_the_old_write_all_loop`
pins the three-plane concatenation, and §4's sweep pins the real geometry: every
frame of both 10-bit witnesses is exactly 18 524 160 B, unchanged.

### The one behaviour change visible without a disk error

**A dump that cannot be CREATED is now fatal too** (missing parent directory, a
path a sweep reused as a directory). It is the same silent hole as a short dump
— the reader sees no file, or an empty one — and it is the more common of the
two. This only fires when the operator has explicitly set the env var.

---

## 2. The reproduced short write

`RLIMIT_FSIZE` is the shape `lanes/unwritten-dep.report.md` §3 actually measured
(frames 0..4 complete plus 13 320 192 B of frame 5). `SIGXFSZ` is **ignored**
(`trap "" XFSZ`) so the limit surfaces as `write`'s `EFBIG` — exactly what a full
filesystem's `ENOSPC` does — instead of killing the child. Committed gate:
`dumpio::tests::a_real_decode_under_a_file_size_cap_fails_loudly`, which drives a
real decode of the pinned `420_mixll_256x128_6f.obu` in a child process under a
4 KiB cap and asserts the run FAILS.

Captured output (same child, three caps; one 256×128 4:2:0 8-bit frame owes
49 152 B):

```
=== ulimit -f 8  (4096 B) ===
ec-av1 dump FAILED [EC_AV1_FINAL_DUMP] …/ours.f0: wrote 4096 of 49152 bytes: write failed with 0 bytes confirmed in hand: File too large (os error 27)
child exit=101        ours.f0 = 4096 B

=== ulimit -f 20 (10240 B) ===
ec-av1 dump FAILED [EC_AV1_FINAL_DUMP] …/ours.f0: wrote 10240 of 49152 bytes: write failed with 0 bytes confirmed in hand: File too large (os error 27)
child exit=101        ours.f0 = 10240 B

=== ulimit -f 60 (30720 B) ===
ec-av1 dump FAILED [EC_AV1_FINAL_DUMP] …/ours.f0: wrote 30720 of 49152 bytes: write failed with 0 bytes confirmed in hand: File too large (os error 27)
child exit=101        ours.f0 = 30720 B

=== uncapped ===
child exit=0          ours.f0 … ours.f6 = 49152 B each
```

Two things this measurement corrected, and would have got wrong by assumption:

1. **The kernel stores up to the cap and THEN returns `EFBIG`** — `write_all`
   confirms *none* of it. The naive "bytes acknowledged" count would have printed
   `wrote 0` while 30 720 B sat on disk. The failure line therefore reports the
   file's **measured length on disk**, and carries the acknowledged count
   separately ("0 bytes confirmed in hand"). The gate asserts the reported number
   EQUALS the fragment's real length, so the line cannot drift from the artefact.
2. The empty-file variant of the incident ("an empty dump read as
   instability") is the `wrote 0` end of the same sweep, not a separate case.

Supporting gates (`crates/ec-av1/src/dumpio.rs`): `/dev/full` (hermetic `ENOSPC`,
no bytes stored), an uncreatable path, a writer that under-declares its own
expected length, and the append-unknown case asserting the "unknown number of"
wording.

---

## 3. `0ee1edf403` — still not reproduced, and now bounded

Re-attempted on `hg_rect64_intra16x4_witness.obu` (23 472 B, sha256
`c9e721088766163b…`) with the loud writer in place.

```text
--- A. plain, 3 processes ---
  rc=0 sha=a26438168c3c53a0 frames=34   ×3, identical
--- B. sentinel (EC_AV1_PLANE_SENTINEL=1), 3 processes ---
  rc=0 sha=a26438168c3c53a0 frames=34   ×3, identical
--- C. two decodes CONCURRENTLY into one reused directory ---
  rc=0/0 sha=a26438168c3c53a0 frames=34
--- D. RLIMIT_FSIZE sweep ---
  cap=32768B    rc=101  wrote 65536 of 18524160 bytes      sha=2d3745b17d665bed frames=1
  cap=65536B    rc=101  wrote 131072 of 18524160 bytes     sha=1fd52689ef20fb22 frames=1
  cap=262144B   rc=101  wrote 524288 of 18524160 bytes      sha=5d13ba5d22458efb frames=1
  cap=1048576B  rc=101  wrote 2097152 of 18524160 bytes     sha=5f745dcff3520f78 frames=1
  cap=2097152B  rc=101  wrote 4194304 of 18524160 bytes     sha=0aafa68194719c7c frames=1
  cap=4194304B  rc=101  wrote 8388608 of 18524160 bytes     sha=7cab950993a6fcc7 frames=1
  cap=8388608B  rc=101  wrote 16777216 of 18524160 bytes:
                 write failed with 15436800 bytes confirmed in hand
                                                            sha=1bf8cc62449548a5 frames=1
  cap=16777216B rc=0    <no loud failure>  sha=a26438168c3c53a0 frames=34
--- E. truncated-prefix arithmetic sweep ---
  34 frames of 18524160 B, full stream 629821440 B
  shapes checked: whole stream + 34 whole-frame prefixes
                  + 34 × {1/4, 1/512, 1/1024, 1/4096, 1/65536} partial cuts  →  matches: 0
```

**What the new evidence RULES OUT (mechanism class, not the value):** any dump
truncated by a filesystem-full, quota or `RLIMIT_FSIZE` failure. Seven caps were
swept; every truncating one now ABORTS with the bytes-vs-expected line instead of
leaving an artefact, and none of the seven artefacts that did get written on the
way down hashes to `0ee1edf403`. The `cap=8388608B` row is the campaign's own
reported shape — complete frames plus a partial tail — now with the confirmed
prefix count in the line. The concurrent-reuse shape is likewise closed here:
both processes exited 0 with every frame at full length.

**What it does NOT rule out, stated plainly:**

* **A killed process.** `SIGKILL`/`SIGTERM` between two frame writes runs no code,
  so no failure line can be emitted and the artefact is exactly the prefix. The
  loud writer cannot see this; nothing in this lane can. A timeout-killed campaign
  run remains a live explanation.
* **A different build or host.** `0ee1edf403` never reproduced on *any* tree
  measured, plain or sentinel; the campaign's own PLAIN value reproduces here,
  which means the divergence was already present in its plain runs too.
* **The value itself.** `0ee1edf403` remains an unreproduced 10-hex artefact
  after 34 whole-frame prefixes, 170 partial cuts, 3 plain and 3 sentinel
  processes, and a concurrent reuse. **Not closed, and not closed by assertion.**

**Disposition: OPEN (instrument hygiene).** The decode remains invariant — plain
and sentinel agree, 3/3 and 3/3, in 34 frames of exactly 18 524 160 B — so nothing
here makes it a decoder defect. What changed is that one whole family of
explanations (silent truncation) is now closed at the instrument, so the next
attempt does not have to re-sweep it.

---

## 4. The sweep whose hashes must not move

`hg_rect64_intra16x4_witness` and `hg_arf_witness`, one decode per PROCESS,
decode-order `cat frame.f*` hashed, every frame's size read back:

```text
hg_rect64_intra16x4_witness run1 rc=0 frames=34 sizes=[18524160] sha=a26438168c3c53a0
hg_rect64_intra16x4_witness run2 rc=0 frames=34 sizes=[18524160] sha=a26438168c3c53a0
hg_rect64_intra16x4_witness run3 rc=0 frames=34 sizes=[18524160] sha=a26438168c3c53a0
hg_arf_witness              run1 rc=0 frames=40 sizes=[18524160] sha=8d7a43bc5fcf840a
hg_arf_witness              run2 rc=0 frames=40 sizes=[18524160] sha=8d7a43bc5fcf840a
hg_arf_witness              run3 rc=0 frames=40 sizes=[18524160] sha=8d7a43bc5fcf840a
```

Against `lanes/unwritten-dep.report.md` §2: `a26438168c` and `8d7a43bc5f` —
**unchanged, prefix for prefix, with the loud writer in place.** The frame size
18524160 is the one §2/§10 assert by hand; it is now asserted by the writer on
every single frame, so a sweep can no longer "pass" a short one.

---

## 5. Gates run

```
cargo test -p ec-av1 --lib -- dump
  dumpio::tests::a_real_decode_under_a_file_size_cap_fails_loudly ... ok
  dumpio::tests::full_device_short_write_is_loud ... ok
  dumpio::tests::success_path_is_byte_identical_to_the_old_write_all_loop ... ok
  dumpio::tests::append_unknown_takes_no_length_claim ... ok
  dumpio::tests::uncreatable_dump_is_loud ... ok
  dumpio::tests::under_declared_expected_length_is_loud ... ok
  stream::tests::pinned_lr_sgr_stream_call_unique_dump ... ok
  wedge::tests::wedge_codebook_matches_libaom_dump ... ok
  intra::tests::rect_predictors_match_c_dump ... ok
  test result: ok. 10 passed; 0 failed; 0 ignored
```
cargo test -p ec-av1 --lib -- the_frame_the_caller_receives_carries_no_unwritten_plane_sample
  test result: ok. 1 passed; 0 failed   (71 s)
```

`dumpio::tests::dumpio_child_decode` is the child half of the RLIMIT gate. It is
not a gate: with no `EC_AV1_DUMPLOUD_CHILD` guard it decodes nothing and asserts
nothing, so a plain `cargo test` run is unaffected.

Full-crate validation is Main's job and was not run here (sibling lanes are
editing concurrently).

---

## 6. What this lane did not do

* Did not close `0ee1edf403`. See §3 for what is and is not ruled out.
* Did not touch the reserved 4:2:0 group-tail chroma SKIP arm, `film_grain.rs`,
  or any decode semantics. The only behavioural changes are: a dump that cannot
  be written now ends the run, and a dump that cannot be created now ends the run.
* Did not change what any dump CONTAINS, or its file naming or index.
* Did not add `dumpio` to the public API (`mod dumpio`, private) — it is an
  instrument, not a surface.
* Did not sweep the pinned `440_request_is_422` cell: `main` = `f01e9738`
  refuses it, same as `e45cc748` did in §10.
* **Did not make the ten `#[cfg(test)]` stream pins loud** (§1, "Out of
  scope"). A short write to one of them leaves a truncated `.obu` at exactly the
  path the following `panic!` hands the reader. Named residue, deliberately not
  fixed here: these are test-side pins whose verdict comes from the assert, not
  from the file, and the one site that already handles its own failure
  (`stream.rs:28143`) does so by design. If a lane wants them closed, the shape
  is `LoudDump::create(site, path, stream.len())` — the same three lines as the
  eleven — but it is a change to what a FAILING GATE PRINTS, and that is a
  decision for the owner of the gates, not for an instrument lane.

## 7. Coordination

Peer lane `av1uballoc` owns `fresh_plane` in `decode.rs` (~21820–21860, the
`plane_sentinel_on()` allocation path). Untouched by this lane.

Lines this lane changed in `crates/ec-av1/src/decode.rs`: **21156–21215**
(`dump_stage16`), **21217–21250** (`dump_stage`), **21250–21284**
(`dump_prefilter_wide`), **38237–38252** (key-frame `EC_AV1_PREFILT_DUMP`),
**38471–38488** (`EC_AV1_MARGIN_DUMP`), **56608–56623** (inter-frame
`EC_AV1_PREFILT_DUMP`). Plus `stream.rs` 2176–2194, 2205–2219, 2252–2274;
`msac.rs` 1007–1009, 1031–1052, 1135–1146; `lib.rs` 33; new file
`src/dumpio.rs`.