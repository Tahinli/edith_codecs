//! Minimal libva (VA-API) bindings and a safe layer over them.
//!
//! This is the hardware-video foundation of the `edith_codecs` family: `ec-hw`
//! builds stateless VA-API decoders and encoders on top of it, and nothing else
//! in the family talks to libva directly.
//!
//! # Shape
//!
//! * [`sys`] — the entire FFI surface, hand-written, one screenful per concern.
//!   Every ABI assumption is stated there and checked by a `const` assertion
//!   against numbers printed by `crates/ec-va/abi-probe.c`.
//! * Everything else is safe: [`Display`], [`Config`], [`Surface`], [`Context`]
//!   and [`Buffer`] are RAII handles that destroy exactly what they created, in
//!   the order libva requires (each child holds an `Arc` of its parent, so the
//!   ordering is a borrow-checker fact rather than a review comment).
//! * [`Picture`] encodes the `begin -> render -> end -> sync` protocol as a
//!   typestate machine, so submitting out of order does not compile.
//!
//! # Version policy
//!
//! Built against libva **1.23** headers and refuses older runtimes with
//! [`Error::Version`]. The alternative — assuming a struct layout and finding
//! out at runtime — is what broke the previous generation of VA-API bindings
//! when 1.23 shifted their generated definitions.
//!
//! # Example
//!
//! ```no_run
//! use ec_va::{CapReport, Display, Surface, SurfaceSpec};
//! use ec_va::caps::{Entrypoint, Profile};
//!
//! let display = Display::open()?;
//! let caps = CapReport::probe(&display)?;
//! assert!(caps.supports(Profile::H264Main, Entrypoint::VLD));
//!
//! let surfaces = Surface::create_pool(&display, &SurfaceSpec::nv12(1920, 1088), 8)?;
//! # Ok::<(), ec_va::Error>(())
//! ```

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod caps;
pub mod config;
pub mod display;
pub mod error;
pub mod picture;
pub mod surface;
pub mod sys;

pub use caps::{CapEntry, CapReport, Entrypoint, Profile, fourcc_str};
pub use config::{Config, ConfigAttrib, SurfaceCaps};
pub use display::Display;
pub use error::{Error, Result};
pub use picture::{
    Buffer, Context, Ended, MappedBuffer, New, Picture, PictureState, Rendering, Synced,
};
pub use surface::{
    Image, MappedImage, PrimeLayer, PrimeObject, PrimeSurface, Surface, SurfaceSpec,
};

/// Major libva version this crate's ABI transcription targets.
pub const MIN_VA_MAJOR: i32 = 1;
/// Minimum libva minor version accepted at runtime.
pub const MIN_VA_MINOR: i32 = 23;
