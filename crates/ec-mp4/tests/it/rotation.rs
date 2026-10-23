//! What the `tkhd` display matrix says: which way the coded picture turns to be
//! seen the way it was shot -- a phone's portrait video is the case that names
//! it, and a player that ignores the matrix shows it sideways.
//!
//! The fixtures are committed here (`tests/data/gen.sh` regenerates them
//! byte-for-byte): one deterministic testsrc2 clip written back out with
//! `ffmpeg -display_rotation <deg>`, which is a real `tkhd` matrix and not the
//! `rotate` metadata that writes none at all.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use ec_core::{Demuxer, Rotation};
use ec_mp4::Mp4Demuxer;

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// The rotation the demuxer reports for `name`'s single video stream.
fn rotation_of(name: &str) -> Rotation {
    let path = data(name);
    let file = File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let demuxer =
        Mp4Demuxer::new(BufReader::new(file)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let streams = demuxer.streams();
    assert_eq!(streams.len(), 1, "{}: one track", path.display());
    streams[0].rotation
}

#[test]
fn a_tkhd_matrix_surfaces_as_a_typed_quarter_turn() {
    // ffmpeg's own counter-clockwise `-display_rotation` against this crate's
    // clockwise reading: its 90 is a quarter turn the other way, so it reads as
    // `Cw270` -- the same way edith's engine reads the identical matrices.
    assert_eq!(rotation_of("rot90.mp4"), Rotation::Cw270);
    assert_eq!(rotation_of("rot180.mp4"), Rotation::Cw180);
    assert_eq!(rotation_of("rot270.mp4"), Rotation::Cw90);
}

#[test]
fn a_file_with_no_turn_surfaces_no_rotation() {
    // ffmpeg writes no display side data for `-display_rotation 0`; the box's
    // matrix is the identity, which is no turn rather than a missing reading.
    let rotation = rotation_of("rot0.mp4");
    assert!(rotation.is_none(), "{rotation:?}");
    assert_eq!(rotation, Rotation::None);
}

#[test]
fn a_matrix_that_is_not_a_quarter_turn_is_representable() {
    // A 45-degree turn cannot be straightened: it is carried, not rounded onto
    // a quarter turn and not dropped. The terms are the box's own 16.16 values.
    let rotation = rotation_of("rot45.mp4");
    assert!(matches!(rotation, Rotation::Other { .. }), "{rotation:?}");
    assert_eq!(rotation.steps(), None);
    assert_eq!(
        rotation,
        Rotation::Other {
            a: 46_340,
            b: -46_340,
            c: 46_340,
            d: 46_340,
        }
    );
}
