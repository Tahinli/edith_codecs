# lane/h26xpinloud — the h26x test pins fail loudly on an Ok-but-short write

Base: `main` = `b8385c93`.

`ec-av1` already owns this class: `crates/ec-av1/src/dumpio.rs` (`LoudDump`, and
`pin` / `pin_reporting` for the test-pin shape) declares the expected byte count,
writes, then stats the file's own length on disk and fails loudly on any
shortfall. The two h26x crates had no shared helper, so the four sites called
directly and the fix is per crate.

## The four named sites

| crate | site (pre-fix) | what it wrote | who read it |
|---|---|---|---|
| `ec-h264` | `tests/encode.rs:475` in `ffmpeg_decode` | our Annex B stream | `ffmpeg -i <path>` |
| `ec-h264` | `tests/encode.rs:1068` in `x264_encode_qp` | raw I420 for x264 | `ffmpeg -i <raw>` (x264 encode) |
| `ec-h265` | `tests/encode.rs:289` in `ffmpeg_decode` | our Annex B stream | `ffmpeg -i <path>` |
| `ec-h265` | `tests/encode.rs:341` in `x265_encode_qp` | raw I420 for x265 | `ffmpeg -i <raw>` (x265 encode) |

All four were `std::fs::write(&path, bytes).ok()?`. The `Err` path was closed
(bail via `?`); the **Ok-but-short** path was not, and that is the one that
produces an artefact another tool then trusts.

## Sweep of the two crates

Every write-ish call in `crates/ec-h264` and `crates/ec-h265` (source + all test
binaries), filtered to the ones whose path an external reader takes:

**Fixed — same shape:**

- `ec-h264/tests/encode.rs:475`, `:1068` (the two named sites).
- `ec-h265/tests/encode.rs:289`, `:341` (the two named sites).
- `ec-h265/tests/conformance.rs:50` `write_au` — `File::create` +
  `write_all(&picture.au).expect("write bitstream")`, whose `path` is passed
  straight to `ffmpeg_decode` → `ffmpeg -i`. `write_all` returning `Ok` means the
  bytes were accepted by the write, not that the file holds them; the same
  on-disk stat is missing. **Not in the ticket's four, but the identical shape
  and the same crate, so it is fixed too.** It is the fifth site and the only
  one that fed a path written by `File::create`.
- `ec-h264/src/enc/mod.rs:747` `rd_trace_write` — `let _ = writeln!(out, ..)`
  into `$EC_H264_RD_TRACE` (feature `rd-ablation`). This one is an append-mode
  text table with **no knowable total**, so no length check is possible (the
  ec-av1 `append_unknown` case: `expected: None`). But discarding the write
  result leaves a silently short trace that an RD-attribution reader cannot
  distinguish from a complete one. Fixed as far as the shape allows: the write
  error is now fatal (`.expect("write EC_H264_RD_TRACE")`). The length check is
  deliberately absent, and the comment says why.

**Swept and NOT the same shape (with the reason):**

- `ec-h264/tests/conformance.rs` `x264_encode` / `x264_encode_gop` (and
  `extract_annexb`, ~1765) — these do not write the file themselves; **ffmpeg**
  writes the stream (`-f h264 <path>`, `h264_mp4toannexb`) and the harness reads
  it back with `fs::read`. There is no discarded write result of ours to check:
  ffmpeg's exit status is the only signal, and `run(...)` already discards
  everything but that boolean. Not this class.
- `ec-h264/tests/conformance.rs` `compare_sequence_streamed` (1693),
  `compare_container_streamed` (1815), `ffmpeg_all_frames` (226),
  `ffmpeg_first_frame` (144) — read-only paths into ffmpeg; no write of ours.
- `ec-h264/tests/conformance.rs` `scratch_dir` (694) `let _ = create_dir_all` and
  the many `let _ = remove_dir_all` — directory lifecycle, no bytes, no length to
  owe.
- `ec-h265/tests/common/mod.rs`, `decode.rs`, `perf.rs`, `rdoq_cache_hash.rs` —
  no file writes at all.
- `ec-h265/src/encoder.rs:769` `header.write(&mut writer, ..)` — an in-memory
  `BitWriter`, not a file.

So: **every write in these two crates whose path is handed to ffmpeg now goes
through the loud pin** (five sites), and the one remaining write with a discarded
result (the append-mode RD trace) has its error made fatal because no length is
knowable.

## Shape chosen: a per-crate helper, not four inline checks

`pin_loud` (plus `short_line`, `pin_or_bail`, `pin_or_panic`), replicating
ec-av1's `dumpio` idea per crate — no cross-crate dependency, per contract.

Why a helper and not inline checks: the four (now five) sites are the *same*
shape — write `bytes` to a `Path`, then hand `Path` to ffmpeg — so inline checks
would duplicate the failure-line format five times and let it drift, which is
precisely the ec-av1 hazard its shared `failure_line` exists to prevent. The
helper returns the **failure line as `Err(String)`** rather than panicking,
because three of the five sites `return None` on write failure today and
`:690` in ec-h264 turns `None` into a **SKIP**. Making it panic would have
changed a skip into a failure; instead the shortfall is printed in full and the
existing bail/skip semantics are preserved. Only `write_au` panicked before, so
only it gets `pin_or_panic` (the line *is* the panic message).

Placement: `ec-h265` has a shared `tests/common/mod.rs` used by four test
binaries, and `write_au` lives in a different binary from `ffmpeg_decode`, so the
helper lives there. `ec-h264` has no `tests/common`; both of its sites are in
`tests/encode.rs`, so the helper is local to that file.

## Proof the check bites

Both crates carry two new non-vacuity tests.

**1. Genuine Ok-but-short (`pin_loud_reds_on_an_ok_but_short_write`).** A
`/dev/null` symlink in a scratch dir is the shape: it *looks* like
`.../ok-but-short.264`, it accepts every byte, it stores none. Measured on this
box with a standalone `rustc` probe:

```
fs::write -> Ok(())  |  on-disk len = 0    |  owed = 1234     # symlink to /dev/null
fs::write -> Ok(())  |  on-disk len = 1234 |  owed = 1234     # real file
```

The test asserts the returned line contains the path and `file holds 0 of 1234
bytes`, then asserts the control — a real file of 1234 bytes succeeds and reads
back byte-identical.

**Red-before proof (mutation).** Replacing the length check's short arm with
`Ok(())` (i.e. deleting the check) turns the test RED:

```
---- pin_loud_reds_on_an_ok_but_short_write stdout ----
panicked at crates/ec-h264/tests/encode.rs:548:61:
called `Result::unwrap_err()` on an `Ok` value: ()
test result: FAILED. 0 passed; 1 failed
```

Restored afterwards; `git diff --stat` confirms the helper is back.

**2. The Err arm, driven for real (`pin_loud_reports_what_landed_under_a_size_cap`).**
A child process runs under `ulimit -f 1` (a 1 KiB block cap) with `SIGXFSZ`
ignored (`trap '' XFSZ`, which survives the exec) and pins 4096 bytes. The kernel
stores up to the cap and *then* returns `EFBIG`, so the acknowledged count is
zero while 512 bytes are on disk. Actual output from both binaries:

```
ec-h264 encode pin SHORT [capped] /tmp/ec-h264-pinloud-cap-*/capped.264: file holds 512 of 4096 bytes: the write failed: File too large (os error 27)
ec-h265 test pin SHORT  [capped] /tmp/ec-h265-pinloud-cap-*/capped.265: file holds 512 of 4096 bytes: the write failed: File too large (os error 27)
```

The count on the line is the file's **real** length, not the writer's confirmed
count — which is the part the acked count gets wrong.

## Scoped runs

```
cargo check -p ec-h264 -p ec-h265 --tests        # clean
cargo check -p ec-h264 --features rd-ablation    # clean
cargo test -p ec-h264 --test encode pin_loud     # 3 passed
cargo test -p ec-h265 --test encode pin_loud     # 3 passed
```

(The full `ec-av1` suite runs on the VPS; per contract these two crates were
checked and run locally, scoped.)

## What did not change

- The success path is byte-identical to the old `fs::write`: same bytes, same
  path, truncate-then-write. The `control` half of each test reads the file back
  and compares bytes.
- No assertion was weakened and no gate's printed output changed on the success
  path. The only new output is on the shortfall path, which previously printed
  nothing at all.
- No decode/encode logic touched: `ec-h264/src/enc/mod.rs` changed only inside the
  `rd-ablation`-gated trace writer's discarded result.