//! Where a REAL-LIBRARY fixture is read from, and the one presence probe every
//! gate that needs one routes through.
//!
//! The clips these gates read (`h264-1080p-23.976-8bit.mp4`,
//! `h264-2160p-23.976-8bit.mp4`, `real-library-manifest.tsv`) are
//! multi-megabyte encodes of a shared library, not pins: they cannot be
//! committed under `crates/<crate>/fixtures/` the way the `.obu` pins are
//! ([`crate_pin`](crate::stream)), so they live in the gitignored ROOT
//! `fixtures/` — `.gitignore:2`. A linked worktree therefore never has that
//! directory, and a gate that skips on its absence reports GREEN having
//! asserted nothing: the class `lane-av1skipfix` closed for the pins, still open
//! for the clips.
//!
//! TWO defects met here, both of which this module closes:
//!
//! 1. **Silent skip.** `h264_clip_frames` and each gate's own
//!    `if !clip.exists() { eprintln!("SKIP …"); return; }` skipped with no env
//!    escape, so `EC_AV1_REQUIRE_FFMPEG=1` — a batch run's way of saying "this
//!    gate must actually run" — did not make the ABSENT CLIP a failure. The
//!    `a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders`
//!    gate printed `SKIP the 1080p tile round trip: no fixture` and libtest
//!    reported `ok` on a tree with no root `fixtures/` at all.
//!
//! 2. **Two roots.** `scripts/verify-fixture-library.sh` validates
//!    `$EC_FIXTURES` (defaulting to `$ROOT/fixtures`) while these gates read
//!    `$ROOT/fixtures` unconditionally. Pointing the preflight at a library
//!    elsewhere reported `resolve 0 missing` and `GREEN` while the clip gates'
//!    root was absent — GREEN with an absent root, the exact false green this
//!    module exists to kill. So [`root`] honours `EC_FIXTURES` FIRST: the
//!    preflight and the gates now resolve the same tree, and when it is unset
//!    the preflight's default IS the gates' root, so its mode-(i) finding fires
//!    exactly when the clip gates will skip.
//!
//! The shape is the crate's existing convention, not a new one: the
//! assert-under-require / print-one-line-otherwise probe is the same one
//! `crates/ec-h264/tests/conformance.rs::require_fixtures` and the per-crate
//! `require_fixture` helpers use, and the file's own presence check lives HERE,
//! inside the probe that owns it — a bare `clip.exists()` at a call site is
//! precisely what the anti-regression scan for the sibling (tool) class
//! rejects.

use std::path::{Path, PathBuf};

/// The envs that turn an absent library clip into a hard failure.
///
/// * `EC_REQUIRE_FIXTURES` — the fleet-wide convention, the same one
///   `scripts/verify-fixture-library.sh` and every per-crate `require_fixture`
///   honour, and the one `EC_REQUIRE_FIXTURES=1` in the preflight's docs names.
/// * `EC_AV1_REQUIRE_FFMPEG` / `EC_AV1_REQUIRE_AOMENC` — this crate's own
///   requirement flags ([`have_ffmpeg`](crate::encode::tests::have_ffmpeg)).
///   Every gate reachable through [`require`] also decodes its result through
///   ffmpeg, so a batch that sets them has already said the gate must run; a
///   missing clip must not be the softer failure.
///
/// All three are checked, not just one: a run that sets only the preflight's
/// env, and a run that sets only this crate's, both get the failure.
fn required() -> bool {
    std::env::var_os("EC_REQUIRE_FIXTURES").is_some()
        || std::env::var_os("EC_AV1_REQUIRE_FFMPEG").is_some()
        || std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_some()
}

/// The fixture root the tests read, in precedence order:
///
/// 1. `EC_FIXTURES` — the same variable `scripts/verify-fixture-library.sh`
///    resolves, so its verdict and these gates' bytes are the same tree. This
///    is the half of the two-roots mismatch that the preflight cannot fix from
///    its side.
/// 2. `<crate>/../../fixtures` — the gitignored root `fixtures/`, the default
///    the preflight uses when `EC_FIXTURES` is unset.
pub fn root() -> PathBuf {
    match std::env::var_os("EC_FIXTURES") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"),
    }
}

/// `require` on a path relative to [`root`]:
/// `require("video/h264-1080p-23.976-8bit.mp4", …)`. The literal is
/// LIBRARY-RELATIVE, which is the row shape `scripts/gen-fixture-library.sh`
/// reads back out of this file, so the preflight keeps resolving these paths.
pub fn path(rel: &str) -> PathBuf {
    root().join(rel)
}

/// The one presence probe for a library fixture. Returns the path when the file
/// is there. When it is not:
///
/// * under [`required()`] — PANIC, naming the resolved path, so the gate is RED
///   instead of green-with-nothing-checked;
/// * otherwise — prints exactly ONE `SKIP` line naming the resolved path and the
///   three ways out (link the library, point `EC_FIXTURES` at it, or set a
///   require env), and returns `None`.
///
/// A caller that gets `None` must `return` WITHOUT printing a second line: the
/// path and the escape hatch are already on the one line above, and a second
/// "no fixture" line is the noise that hid this defect from a reading of the
/// output.
pub fn require(rel: &str, what: &str) -> Option<PathBuf> {
    require_at(path(rel), what)
}

/// [`require`] for a path the caller resolved itself — an `EC_AV1_WALL_CLIP`
/// override, which names a clip OUTSIDE any library root, so it must not be
/// joined onto one.
pub fn require_at(clip: PathBuf, what: &str) -> Option<PathBuf> {
    if clip.is_file() {
        return Some(clip);
    }
    assert!(
        !required(),
        "{what}: the library clip is absent at {} -- this gate would prove nothing. A linked \
         worktree has no gitignored root `fixtures/`: run scripts/link-fixtures.sh, or point \
         EC_FIXTURES at a library root, or set EC_REQUIRE_FIXTURES=1 / EC_AV1_REQUIRE_FFMPEG=1 \
         only on a tree that really has the library.",
        clip.display()
    );
    eprintln!(
        "SKIP {what}: {} absent at {} -- this gate proved nothing; run \
         scripts/link-fixtures.sh, point EC_FIXTURES at a library root, or set \
         EC_REQUIRE_FIXTURES=1 / EC_AV1_REQUIRE_FFMPEG=1 to make this a failure.",
        clip.file_name().unwrap_or_default().to_string_lossy(),
        clip.display()
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two-roots reconciliation, asserted on the RESOLVER rather than
    /// described in prose: with `EC_FIXTURES` set, [`root`] is that directory and
    /// NOT `<crate>/../../fixtures`. (The variable is process-constant in this
    /// crate — nothing calls `set_var` — so the assertion holds for whatever the
    /// batch exported; it is the precedence, not a particular value, that is
    /// under test.)
    #[test]
    fn the_resolver_follows_ec_fixtures_over_the_gitignored_root() {
        let root = super::root();
        if let Some(p) = std::env::var_os("EC_FIXTURES")
            && !p.is_empty()
        {
            assert_eq!(
                root,
                PathBuf::from(p),
                "EC_FIXTURES must win over the default root"
            );
        } else {
            assert!(
                root.ends_with("../../fixtures"),
                "the default root is the gitignored <crate>/../../fixtures, got {}",
                root.display()
            );
        }
        assert_eq!(
            super::path("video/h264-1080p-23.976-8bit.mp4"),
            root.join("video/h264-1080p-23.976-8bit.mp4")
        );
    }
}
