//! Loud dump writing: a dump that cannot hold every byte it claims to hold is
//! a failure, not a diagnostic artefact.
//!
//! lane-av1dumploud: every dump site in this crate wrote through
//! `let _ = f.write_all(..)`. A full filesystem, an `RLIMIT_FSIZE`, or any other
//! `ENOSPC`/`EFBIG` therefore left a SHORT file on disk and nothing said so.
//! Measured consequence, twice in one day: a sentinel-vs-plain hash difference
//! that read as decoder nondeterminism reproduced bit-for-bit as a truncated
//! dump (`lanes/unwritten-dep.report.md`: frames 0..4 complete plus 13 320 192
//! bytes of frame 5, with `EC_AV1_FINAL_DUMP`'s discarded write error), and an
//! empty dump read as instability. That artefact started a whole defect class,
//! and a silently wrong diagnostic is worse than a loud one, so every dump site
//! goes through [`LoudDump`]: the caller states the expected byte count from the
//! geometry it already has, the writer checks that count twice -- once against
//! the bytes it was handed, once against the file's own length on disk -- and
//! any shortfall aborts the decode with the file, the counts, and the OS error.
//!
//! The success path is byte-identical to the old `write_all` loop: same bytes,
//! same order, same file names.
//!
//! The failure path is a panic, not a returned `Result`: the dump sites sit in
//! the middle of `fn decode_*` functions with no error channel for them, and a
//! dump that failed IS a decode failure (the run's measurement is void). This
//! is a debug instrument -- it only runs when its env var is set -- so the
//! panic cannot affect a normal decode.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One dump file, with the byte count it owes accounted for as it goes.
pub struct LoudDump {
    /// Human-readable label for the failure line: the env var, or
    /// `VAR.suffix` when the file names something the var alone does not.
    site: String,
    path: PathBuf,
    file: Option<File>,
    written: usize,
    /// `None` only for a stream whose total length is genuinely unknowable at
    /// open time (an append-mode text table). Short-write detection still
    /// applies -- `write_all` reports a short write as `Err` -- but the failure
    /// line then carries no expected count, because inventing one would be a
    /// guess.
    expected: Option<usize>,
}

impl LoudDump {
    /// Open `path` for `site`, expecting exactly `expected` bytes in total.
    ///
    /// Failing to CREATE is loud too: a requested dump that does not exist is
    /// the same silent hole as one that is short (the reader sees no file, or
    /// an empty one), and it is the more common of the two (a missing parent
    /// directory, a path a sweep reused as a directory).
    pub fn create(site: &str, path: impl AsRef<Path>, expected: usize) -> Self {
        let path = path.as_ref().to_path_buf();
        let file = File::create(&path).unwrap_or_else(|e| {
            abort(
                &site,
                &path,
                0,
                Some(expected),
                &format!("could not create the dump file: {e}"),
            )
        });
        Self {
            site: site.to_string(),
            path,
            file: Some(file),
            written: 0,
            expected: Some(expected),
        }
    }

    /// Open `path` for `site` in APPEND mode with no knowable total length --
    /// the CDF-table text dump, whose file spans every frame of a run. Write
    /// errors are still fatal; the length check is simply absent (see
    /// [`LoudDump::expected`]).
    pub fn append_unknown(site: &str, path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| {
                abort(
                    &site,
                    &path,
                    0,
                    None,
                    &format!("could not open the dump file: {e}"),
                )
            });
        Self {
            site: site.to_string(),
            path,
            file: Some(file),
            written: 0,
            expected: None,
        }
    }

    /// Hand `chunk` to the file. Overrunning `expected` is a caller bug (the
    /// count and the writer disagree) and aborts just as loudly as a short
    /// write, because one of the two is certainly lying.
    pub fn write(&mut self, chunk: &[u8]) {
        if let Some(exp) = self.expected
            && self.written + chunk.len() > exp
        {
            abort(
                &self.site,
                &self.path,
                self.written,
                Some(exp),
                &format!(
                    "the writer was handed {} bytes but only {} remained of the {} it declared",
                    chunk.len(),
                    exp - self.written,
                    exp
                ),
            );
        }
        let r = self.file.as_mut().expect("live dump").write_all(chunk);
        if let Err(e) = r {
            // MEASURED, do not assume: under `RLIMIT_FSIZE` the kernel stores
            // everything up to the cap and THEN returns `EFBIG` (measured:
            // a 49 152 B frame under caps of 4 KiB / 10 KiB / 30 KiB left
            // 4 096 / 10 240 / 30 720 B on disk, with `write_all` confirming
            // none of it). So "bytes written" on this line is the file's real
            // length, not the count `write_all` got to acknowledge -- the
            // acknowledged count rides along in the reason.
            let f = self.file.as_ref().expect("live dump");
            let on_disk = f
                .metadata()
                .map(|m| m.len() as usize)
                .unwrap_or(self.written);
            abort(
                &self.site,
                &self.path,
                on_disk,
                self.expected,
                &format!(
                    "write failed with {} bytes confirmed in hand: {e}",
                    self.written
                ),
            );
        }
        self.written += chunk.len();
    }

    /// Close the dump: flush, then check the file's own length on disk.
    ///
    /// The on-disk check is not redundant with the write loop. `write_all`
    /// reports what the FILE ACCEPTED, and the reader's question is what the
    /// filesystem ended up holding -- a sparse/short final flush, a quota, or a
    /// filesystem-level reservation failure all land here rather than in
    /// `write`.
    pub fn finish(mut self) {
        let Some(mut file) = self.file.take() else {
            return;
        };
        if let Err(e) = file.flush() {
            abort(
                &self.site,
                &self.path,
                self.written,
                self.expected,
                &format!("flush failed after {} bytes: {e}", self.written),
            );
        }
        let Some(exp) = self.expected else {
            return;
        };
        let on_disk = match file.metadata() {
            Ok(m) => m.len() as usize,
            Err(e) => abort(
                &self.site,
                &self.path,
                self.written,
                Some(exp),
                &format!("could not stat the finished dump: {e}"),
            ),
        };
        if on_disk != exp {
            abort(
                &self.site,
                &self.path,
                on_disk,
                Some(exp),
                "the file on disk is not the length the writer declared",
            );
        }
    }
}

/// The single failure line every short dump now ends on. `site`, the path, the
/// bytes on disk vs the bytes owed, and the reason.
fn abort(site: &str, path: &Path, got: usize, expected: Option<usize>, why: &str) -> ! {
    panic!("{}", failure_line(site, path, got, expected, why));
}

/// The failure line itself, so the fatal and the reporting shapes cannot
/// drift apart.
fn failure_line(site: &str, path: &Path, got: usize, expected: Option<usize>, why: &str) -> String {
    let owed = match expected {
        Some(e) => format!("{e}"),
        None => "an unknown number of".to_string(),
    };
    format!(
        "ec-av1 dump FAILED [{}] {}: wrote {} of {} bytes: {}",
        site,
        path.display(),
        got,
        owed,
        why
    )
}

/// Write `bytes` to `path` as a FATAL pin: [`pin`] IS a wrapper -- three
/// [`LoudDump`] calls (`create`/`write`/`finish`), so it inherits the flush and
/// the flush-then-stat ordering for free.
///
/// lane-av1pinloud: the test-pin sites in `stream.rs` wrote with
/// `let _ = std::fs::write(..)` and then named the path in the panic that
/// followed ("stream pinned at {}"), so an `ENOSPC`/`EFBIG` left a TRUNCATED
/// stream at exactly the path the reader was told to replay -- the same
/// silent-truncation class as the dump path, one hop later. Success path is
/// byte-identical: same bytes, same path, truncate-then-write.
pub fn pin(site: &str, path: impl AsRef<Path>, bytes: &[u8]) {
    let mut d = LoudDump::create(site, path, bytes.len());
    d.write(bytes);
    d.finish();
}

/// [`pin`]'s NON-FATAL twin, for a site that must not die on a failed pin -- a
/// gate whose real failure is the assert that comes after the pin attempt, and
/// whose comment says so.
///
/// This one RE-IMPLEMENTS the check rather than wrapping [`LoudDump`], whose
/// failure channel is `panic!`: it cannot report instead of dying. So it does
/// `fs::write` + `metadata` itself. Only the failure LINE is shared, via
/// [`failure_line`] -- the ordering (flush-then-stat) and the
/// write-error-handling are not shared with [`LoudDump`], and a reader must not
/// assume they are.
///
/// Non-fatal, but not silent: a shortfall comes back as the very same failure
/// line [`pin`] would have panicked with, expected-vs-on-disk counts included,
/// for the caller to print as a warning.
pub fn pin_reporting(site: &str, path: impl AsRef<Path>, bytes: &[u8]) -> Result<(), String> {
    let path = path.as_ref();
    let expected = bytes.len();
    if let Err(e) = std::fs::write(path, bytes) {
        let on_disk = std::fs::metadata(path)
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        return Err(failure_line(
            site,
            path,
            on_disk,
            Some(expected),
            &format!("could not write the pin: {e}"),
        ));
    }
    let on_disk = match std::fs::metadata(path) {
        Ok(m) => m.len() as usize,
        Err(e) => {
            return Err(failure_line(
                site,
                path,
                expected,
                Some(expected),
                &format!("could not stat the written pin: {e}"),
            ));
        }
    };
    if on_disk != expected {
        return Err(failure_line(
            site,
            path,
            on_disk,
            Some(expected),
            "the pinned file on disk is not the length of the stream",
        ));
    }
    Ok(())
}

/// Write `planes` (already narrowed to their dump's sample width) as ONE dump
/// file, failing loudly on any shortfall. `expected` is the sum of the plane
/// lengths -- the same extents the writer iterates, so a plane that is shorter
/// than its geometry claims cannot hide here.
pub fn write_planes(site: &str, path: impl AsRef<Path>, planes: &[&[u8]]) {
    let expected: usize = planes.iter().map(|p| p.len()).sum();
    let mut d = LoudDump::create(site, path, expected);
    for p in planes {
        d.write(p);
    }
    d.finish();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ec-av1-dumpio-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn caught(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = std::panic::catch_unwind(f);
        std::panic::set_hook(prev);
        let e = r.expect_err("the short dump must abort");
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .expect("panic payload is a string")
    }

    #[test]
    fn success_path_is_byte_identical_to_the_old_write_all_loop() {
        let p = scratch("ok");
        let planes: [&[u8]; 3] = [b"abc", b"", b"de"];
        write_planes("T", &p, &planes);
        assert_eq!(std::fs::read(&p).unwrap(), b"abcde");
        let _ = std::fs::remove_file(&p);
    }

    /// `/dev/full` accepts an `open` for write and returns `ENOSPC` from every
    /// write -- the hermetic stand-in for the full filesystem that produced the
    /// 13 320 192-byte truncated frame in `lanes/unwritten-dep.report.md`.
    #[test]
    #[cfg(unix)]
    fn full_device_short_write_is_loud() {
        let msg = caught(|| write_planes("EC_AV1_FINAL_DUMP", "/dev/full", &[b"abc", b"de"]));
        assert!(
            msg.starts_with("ec-av1 dump FAILED [EC_AV1_FINAL_DUMP] /dev/full: wrote "),
            "{msg}"
        );
        assert!(msg.contains("No space left on device"), "{msg}");
        // The counts must be there, or the line does not say what was lost.
        assert!(msg.contains(" of 5 bytes"), "{msg}");
    }

    #[test]
    fn uncreatable_dump_is_loud() {
        let p = std::env::temp_dir().join(format!("ec-av1-nodir-{}/x", std::process::id()));
        let msg = caught(|| write_planes("T", &p, &[b"abc"]));
        assert!(msg.contains("could not create the dump file"), "{msg}");
    }

    /// A writer that under-declares its own expected length is the same silent
    /// hole: the reader gets a file the site believes is complete.
    #[test]
    fn under_declared_expected_length_is_loud() {
        let p = scratch("under");
        let msg = caught(|| {
            let mut d = LoudDump::create("T", &p, 2);
            d.write(b"abcdef");
            d.finish();
        });
        assert!(
            msg.contains("was handed 6 bytes but only 2 remained of the 2 it declared"),
            "{msg}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn append_unknown_takes_no_length_claim() {
        let p = scratch("append");
        {
            let mut a = LoudDump::append_unknown("T", &p);
            a.write(b"x");
            a.finish();
            let mut b = LoudDump::append_unknown("T", &p);
            b.write(b"y");
            b.finish();
        }
        assert_eq!(std::fs::read(&p).unwrap(), b"xy");
        let msg = caught(|| {
            let mut a = LoudDump::append_unknown("T", "/dev/full");
            a.write(b"x");
            a.finish();
        });
        assert!(msg.contains("of an unknown number of bytes"), "{msg}");
        let _ = std::fs::remove_file(&p);
    }
    /// THE reproduction of the incident this module exists for: a real decode,
    /// a real `RLIMIT_FSIZE`, a real truncated `EC_AV1_FINAL_DUMP` file left
    /// on disk -- and the run stopping with the bytes-vs-bytes line instead of
    /// carrying on to a measurement built on the fragment.
    ///
    /// `/dev/full` (the unit test above) proves the writer's check works but
    /// never leaves a partial file behind, because `create` on it succeeds and
    /// nothing is ever stored. `RLIMIT_FSIZE` is the shape
    /// `lanes/unwritten-dep.report.md` actually measured: frames 0..4 complete
    /// plus 13 320 192 bytes of frame 5. `SIGXFSZ` is IGNORED (`trap "" XFSZ`)
    /// so the limit surfaces as `write`'s `EFBIG` -- exactly what a full
    /// filesystem's `ENOSPC` does -- instead of killing the child outright.
    #[test]
    #[cfg(unix)]
    fn a_real_decode_under_a_file_size_cap_fails_loudly() {
        let dir = std::env::temp_dir().join(format!("ec-av1-dumpio-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prefix = dir.join("ours");
        // 8 blocks of 512 B = 4096 B: far below one 256x128 4:2:0 8-bit frame
        // (49 152 B), far above the file header, so the write lands and fails
        // PART way rather than at create.
        let out = Command::new("bash")
            .args([
                "-c",
                "ulimit -f 8; trap \"\" XFSZ; exec \"$0\" \"$@\"",
                // $0 for `bash -c` is the test binary itself.
                std::env::current_exe().unwrap().to_str().unwrap(),
                "--exact",
                "dumpio::tests::dumpio_child_decode",
                "--nocapture",
            ])
            .env("EC_AV1_DUMPLOUD_CHILD", "1")
            .env("EC_AV1_FINAL_DUMP", &prefix)
            // One thread, so the dump panic lands on the main thread and the
            // child's exit status is the decode's own.
            .env("EC_AV1_THREADS", "1")
            .output()
            .expect("spawning the capped child");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "the decode must FAIL under a file-size cap, not finish with a truncated dump\n{stderr}"
        );
        assert!(
            stderr.contains("ec-av1 dump FAILED [EC_AV1_FINAL_DUMP]"),
            "the failure line must name the dump site\n{stderr}"
        );
        assert!(
            stderr.contains("File too large"),
            "the failure line must carry the OS error\n{stderr}"
        );
        // One 256x128 4:2:0 8-bit frame is 256*128*3/2 = 49 152 B, and the
        // failure line's "wrote N of 49152" must name the file's REAL length,
        // not the count `write_all` managed to acknowledge: a line that
        // under-reports is a line that sends the reader back to the dump to
        // measure it themselves.
        let frag = std::fs::metadata(format!("{}.f0", prefix.display()))
            .unwrap()
            .len();
        assert!(
            frag < 49152,
            "the capped child left a {frag}-byte frame where 49152 B was owed"
        );
        assert!(
            stderr.contains(&format!("wrote {frag} of 49152 bytes")),
            "the failure line's written count must be the file's actual length ({frag})\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Child half of [`a_real_decode_under_a_file_size_cap_fails_loudly`]. It is
    /// not a gate: with no `EC_AV1_DUMPLOUD_CHILD` guard it decodes nothing and
    /// asserts nothing, so a plain `cargo test` run is unaffected.
    #[test]
    fn dumpio_child_decode() {
        if std::env::var("EC_AV1_DUMPLOUD_CHILD").is_err() {
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/420_mixll_256x128_6f.obu");
        let bytes = std::fs::read(&path).expect("the pinned fixture");
        let _ = crate::stream::decode_stream(&bytes);
    }

    #[test]
    fn pin_success_path_is_byte_identical() {
        let p = scratch("pin-ok");
        pin("TEST_PIN", &p, b"stream bytes");
        assert_eq!(std::fs::read(&p).unwrap(), b"stream bytes");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    #[cfg(unix)]
    fn pin_to_a_full_device_is_loud() {
        let msg = caught(|| pin("TEST_PIN", "/dev/full", b"stream bytes"));
        assert!(
            msg.starts_with("ec-av1 dump FAILED [TEST_PIN] /dev/full: wrote "),
            "{msg}"
        );
        assert!(msg.contains("No space left on device"), "{msg}");
        assert!(msg.contains(" of 12 bytes"), "{msg}");
    }

    /// The reporting shape: same failure line, returned instead of panicked,
    /// so a gate whose real failure is the assert after the pin keeps that
    /// assert -- but the reader still learns the pin is short.
    #[test]
    #[cfg(unix)]
    fn pin_reporting_is_loud_but_not_fatal() {
        let line = pin_reporting("TEST_PIN", "/dev/full", b"stream bytes").unwrap_err();
        assert!(
            line.contains("ec-av1 dump FAILED [TEST_PIN] /dev/full"),
            "{line}"
        );
        assert!(line.contains(" of 12 bytes"), "{line}");
        let p = scratch("pin-reporting-ok");
        pin_reporting("TEST_PIN", &p, b"stream bytes").expect("a healthy pin reports nothing");
        assert_eq!(std::fs::read(&p).unwrap(), b"stream bytes");
        let _ = std::fs::remove_file(&p);
    }

    /// The pin path's own reproduction: a real `RLIMIT_FSIZE` leaves a real
    /// truncated stream at the path the panic names. Without the check the
    /// reader replays the fragment and blames the decoder.
    #[test]
    #[cfg(unix)]
    fn a_pin_under_a_file_size_cap_fails_loudly() {
        let dir = std::env::temp_dir().join(format!("ec-av1-pin-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = Command::new("bash")
            .args([
                "-c",
                "ulimit -f 8; trap \"\" XFSZ; exec \"$0\" \"$@\"",
                std::env::current_exe().unwrap().to_str().unwrap(),
                "--exact",
                "dumpio::tests::pin_child_under_cap",
                "--nocapture",
            ])
            .env("EC_AV1_PINLOUD_CHILD", &dir)
            .output()
            .expect("spawning the capped child");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "a short pin must fail\n{stderr}");
        assert!(stderr.contains("ec-av1 dump FAILED [TEST_PIN]"), "{stderr}");
        assert!(stderr.contains("File too large"), "{stderr}");
        let frag = std::fs::metadata(dir.join("cap-pinned.obu")).unwrap().len();
        assert!(
            frag < 32768,
            "the capped child left {frag} bytes where 32768 B was owed"
        );
        assert!(
            stderr.contains(&format!("wrote {frag} of 32768 bytes")),
            "{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Child half of [`a_pin_under_a_file_size_cap_fails_loudly`]. Not a gate:
    /// with no `EC_AV1_PINLOUD_CHILD` set it writes nothing and asserts nothing.
    #[test]
    fn pin_child_under_cap() {
        let Ok(dir) = std::env::var("EC_AV1_PINLOUD_CHILD") else {
            return;
        };
        // `ulimit -f 8` is 8 KiB, so 4x that is guaranteed to run PAST the cap
        // (a write of exactly the cap size succeeds) and land a fragment.
        pin(
            "TEST_PIN",
            Path::new(&dir).join("cap-pinned.obu"),
            &[7u8; 32768],
        );
    }
}
