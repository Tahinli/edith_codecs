//! VA-API (libva) layer over [`cros_libva`].
//!
//! This is the hardware-video foundation of the `edith_codecs` family: `ec-hw`
//! builds stateless VA-API decoders and encoders on top of it, and nothing else
//! in the family talks to libva directly. Every libva call goes through
//! cros-libva's safe wrappers, so this crate contains no `unsafe` of its own.
//!
//! * [`va`] — the whole cros-libva API, re-exported: raw constants and typed
//!   codec parameter buffers ([`va::BufferType`], e.g.
//!   `BufferType::PictureParameter(PictureParameter::H264(..))`).
//! * [`Display`], [`Config`], [`Context`], [`Surface`], [`Picture`], [`Buffer`]
//!   and [`Image`] are cros-libva's RAII handles. Each child holds an `Rc` of
//!   its parent, so libva's destruction order is enforced by ownership.
//! * [`surface`] adds PRIME export/import, timed sync and pool creation;
//!   [`caps`] probes what the driver supports; [`vpp`] is a video-processing
//!   pipeline.
//!
//! # Version policy
//!
//! The pinned cros-libva revision builds against libva 1.23. The runtime
//! version is not exposed by cros-libva, so it is not checked here.
//!
//! # Example
//!
//! ```no_run
//! use ec_va::caps::{CapReport, Entrypoint, Profile};
//!
//! let display = ec_va::open()?;
//! let caps = CapReport::probe(&display)?;
//! assert!(caps.supports(Profile::H264Main, Entrypoint::VLD));
//! # Ok::<(), ec_va::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cacheline;
pub mod caps;
pub mod config;
pub mod display;
pub mod error;
pub mod picture;
pub mod surface;
pub mod vpp;

pub use cros_libva as va;

pub use caps::{CapEntry, CapReport, Entrypoint, Profile, fourcc_str};
pub use config::{Config, ConfigAttrib, SurfaceCaps, caps_of};
pub use display::{Display, open, open_path};
pub use error::{Error, Result};
pub use picture::{Buffer, Context, Picture, create_context};
pub use surface::{
    Image, PrimeLayer, PrimeObject, PrimeSurface, Surface, SurfaceExt, SurfaceSpec, create_pool,
    import_prime,
};
pub use vpp::Vpp;
