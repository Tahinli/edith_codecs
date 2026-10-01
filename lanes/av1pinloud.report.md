# lane av1pinloud — loud self-pins in `stream.rs`

## Defect

Every self-pin site in `crates/ec-av1/src/stream.rs` wrote with

```rust
let _ = std::fs::write(&pin, &stream);
```

and then named the path in the very next statement's failure message
("stream pinned at {}"). A partial write (`ENOSPC`, `RLIMIT_FSIZE`) therefore left a
**truncated stream at exactly the path the reader was told to replay** — the same
silent-truncation class that produced two false defect rows on the dump path
(`lanes/unwritten-dep.report.md`), one hop downstream. `fs::write`'s `Result` was
discarded, so nothing said so.

Nine sites: `stream.rs` 14191, 18201, 23170, 23190, 23589, 23611, 27904, 28896,
29106 — plus the two `EC_AV1_GATE_DUMP` writes beside 28896/29106
(`stream.rs` 28898, 29108), which checked the write but never checked the resulting
length.

## Change

`crates/ec-av1/src/dumpio.rs` gains two wrappers over the existing `LoudDump`, plus a
`failure_line` helper so the fatal and reporting shapes cannot drift:

- `pin(site, path, bytes)` — fatal. Same shape as `write_planes` for a single blob:
  create, `write_all`, then stat the file and compare against `bytes.len()`.
  Success path is byte-identical to the old `fs::write`: same bytes, same path,
  truncate-then-write.
- `pin_reporting(site, path, bytes) -> Result<(), String>` — non-fatal, not silent.
  Returns the **same** failure line `pin` would have panicked with
  (`ec-av1 dump FAILED [EC_AV1_PIN] <path>: wrote N of M bytes: <why>`).

All nine pin sites now call `pin("EC_AV1_PIN", ...)`; both `EC_AV1_GATE_DUMP` writes
call `pin("EC_AV1_GATE_DUMP", ...)` (previously `fs::write(...).expect(...)`, loud on a
write error but blind to a short file on disk).

One more in-crate site of the same shape: `msac.rs:1140` (`EC_AV1_SYMTRACE` flush) had a
hand-rolled panic that compared the buffer length but never stat'ed the file. Folded
into `pin` so it gets the on-disk check.

### `stream.rs:28143` — the warns-by-design site: decision

Routed through `pin_reporting`, keeping it non-fatal. The two shapes differ for a
stated reason, not by accident:

- the nine sites run **inside the arm that is already panicking** — the pin exists to
  feed that panic, so dying on a short pin replaces a message with a strictly better
  one (same panic, plus the byte counts).
- 28143's real failure is the `assert_eq!` three lines below (luma/U/V vs ffmpeg).
  Panicking on the write would **mask that assert** with an unrelated filesystem
  error — the exact failure its comment refuses. So it must not be fatal.

What changed is what it *prints*: the old `could not pin to {path}: {e}` warning
became `cdfflake: ec-av1 dump FAILED [EC_AV1_PIN] {path}: wrote N of M bytes: {why}` —
same severity (a warning, then the asserts run), now carrying expected-vs-on-disk
counts. A truncated pin there was previously indistinguishable from a good one.

## Proof

`dumpio::tests::a_pin_under_a_file_size_cap_fails_loudly` is the real reproduction: a
child under `ulimit -f 8` (8 KiB) pins 32 KiB, leaving a fragment and dying on the
counts, not carrying on.

```
$ EC_AV1_PINLOUD_CHILD=/tmp/pincap2 bash -c 'ulimit -f 8; trap "" XFSZ; exec "$0" "$@"' \
    <testbin> --exact dumpio::tests::pin_child_under_cap --nocapture
ec-av1 dump FAILED [EC_AV1_PIN] /tmp/pincap2/cap-pinned.obu: wrote 8192 of 32768 bytes:
  write failed with 0 bytes confirmed in hand: File too large (os error 27)
test dumpio::tests::pin_child_under_cap ... FAILED
$ ls -l /tmp/pincap2
-rw-r--r-- 1 tahinli tahinli 8192 cap-pinned.obu
```

8192 B on disk where 32768 B was owed — bytes-expected vs bytes-on-disk, and the
reported count is the file's real length, not what `write_all` acknowledged.

Also: `pin_success_path_is_byte_identical` (bytes on disk equal the bytes handed in),
`pin_to_a_full_device_is_loud`, `pin_reporting_is_loud_but_not_fatal` (the same line,
returned rather than panicked, plus a healthy pin returning `Ok`).

Scoped runs (`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1pinloud`):

```
cargo test -p ec-av1 --lib dumpio::                                  12 passed; 0 failed
cargo test -p ec-av1 --lib -- --exact \
  stream::tests::a_444_intra_in_inter_128root_muchroma_pinned_stream_decodes_pixel_exact
                                                                    1 passed; 0 failed
cargo check -p ec-av1 --tests                                        clean
```

## Class sweep, other crates

`grep` for `let _ = std::fs::write` / `let _ = …write_all` / discarded `fs::write` across
every `ec-*` crate:

| Site | Shape | Disposition |
|---|---|---|
| `ec-vorbis/tests/oracle.rs:1447,1452` | `let _ = fs::write` of sweep tables into `lanes/*.txt` | **Reported, not fixed.** Not the same shape: the path is not printed in a failure message, the file is a human-read report rather than a replay artefact, and the asserts that follow (`rows > 0`, `failures.is_empty()`) do not depend on it. Sibling lane material. |
| `ec-ac3/examples/ac3dec.rs:89,96` | `let _ = out.write_all(..)` to raw stdout of an example decoder | **Reported, not fixed.** No path is named; a short write there is the encoder of a raw file a downstream `cmp` measures. Different class (unverified output, not a mis-pointed replay path). |
| `ec-h264/tests/encode.rs:475,1068`, `ec-h265/tests/encode.rs:289,341` | `std::fs::write(..).ok()?` feeding an `ffmpeg` subprocess | **Reported, not fixed.** `.ok()?` propagates the failure as `None` — the test helper bails rather than decoding a truncated file, so there is no silent-truncation path to close. |

No sibling site has the "discard the error, then hand the reader that exact path"
shape. Fixed only `ec-av1`.

## Notes for the reviewer

- Success path is byte-identical; the only behavioural delta is that a short pin is
  now fatal (9 sites + 2 gate dumps + the symtrace flush) or loud-with-counts (28143).
- The `EC_AV1_PIN` site label is a new failure-line prefix; grep it when triaging.