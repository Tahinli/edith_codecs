//! Codec parameter buffers for stateless VA-API decoding.
//!
//! Each codec module builds its parameters as plain Rust values and converts
//! them, field for field, into cros-libva's typed buffers
//! (`BufferType::PictureParameter(PictureParameter::H264(..))` and friends).
//! cros-libva owns the `#[repr(C)]` transcriptions and their ABI checks, so
//! nothing in this crate reinterprets memory.

pub mod av1;
pub mod h264;
pub mod vp9;
pub mod hevc;

use ec_va::va::VA_INVALID_SURFACE;

/// `VA_INVALID_SURFACE`: the id a driver expects in an unused reference slot.
pub const INVALID_SURFACE: u32 = VA_INVALID_SURFACE;
