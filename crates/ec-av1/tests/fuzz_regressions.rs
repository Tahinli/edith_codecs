//! Regression inputs found by the `decode_stream` fuzz target (`crates/ec-av1/fuzz`).
//!
//! Each input must decode without a panic; an error is the expected outcome.

use ec_av1::stream::{decode_stream_with, decode_stream_with_threads};
use ec_core::Error;

/// An `OBU_FRAME` whose frame header sets `show_existing_frame`. The header returns
/// before `tile_info` is read, so the tile group used to index empty tile starts
/// (`index out of bounds: the len is 0 but the index is 0`).
#[test]
fn obu_frame_with_show_existing_frame_does_not_panic() {
    let data = include_bytes!("data/fuzz/decode_stream-obu-frame-show-existing.bin");
    let result = decode_stream_with(data, |_picture, _index, _shown| Ok(()));
    assert!(result.is_err(), "a show_existing_frame OBU_FRAME is corrupt, not decodable");
}

/// A film grain parameter set whose scaling points repeat a value: the zero-width
/// segment made `build_scaling_lut` divide by zero. Such points are corrupt (6.8.20).
#[test]
fn film_grain_repeated_scaling_point_does_not_panic() {
    let data = include_bytes!("data/fuzz/decode_stream-film-grain-points.bin");
    let result = decode_stream_with(data, |_picture, _index, _shown| Ok(()));
    assert!(result.is_err(), "non-increasing film grain points are corrupt, not decodable");
}

/// The input recorded as a CfL AC-size crash. Its first inconsistent block is a
/// prediction override of another size (flagged before the CfL check runs), so the
/// frame is refused as corrupt and nothing indexes past a buffer. Runs on one thread,
/// as the `decode_stream` fuzz target does.
#[test]
fn cfl_transform_larger_than_ac_buffer_is_corrupt() {
    let data = include_bytes!("data/fuzz/decode_stream-cfl-ac-size.bin");
    let err = decode_stream_with_threads(data, 1, |_picture, _index, _shown| Ok(()))
        .expect_err("a block whose reconstruction inputs mismatch is corrupt");
    assert!(matches!(err, Error::Corrupt { .. }), "got {err:?}");
}

/// A palette/intrabc prediction override whose size is not the transform block's: the
/// frame is refused as corrupt. Runs on one thread, as the `decode_stream` fuzz target does.
#[test]
fn palette_override_of_another_size_is_corrupt() {
    let data = include_bytes!("data/fuzz/decode_stream-palette-pred-size.bin");
    let err = decode_stream_with_threads(data, 1, |_picture, _index, _shown| Ok(()))
        .expect_err("a prediction override of the wrong size is corrupt");
    assert!(matches!(err, Error::Corrupt { .. }), "got {err:?}");
    assert!(err.to_string().contains("prediction override"), "got {err}");
}

/// An intrabc block vector (spec 6.10.25 `is_mv_valid`) that leaves the i16 range the mi
/// grid stores: `mv16` asserted on it (debug builds) and wrapped it (release). The frame
/// is refused as corrupt. Runs on one thread, as the `decode_stream` fuzz target does.
#[test]
fn intrabc_vector_outside_the_spec_range_is_corrupt() {
    let data = include_bytes!("data/fuzz/decode_stream-mv-i16-range.bin");
    let err = decode_stream_with_threads(data, 1, |_picture, _index, _shown| Ok(()))
        .expect_err("a motion vector outside the spec's range is corrupt");
    assert!(matches!(err, Error::Corrupt { .. }), "got {err:?}");
    assert!(err.to_string().contains("motion vector"), "got {err}");
}
