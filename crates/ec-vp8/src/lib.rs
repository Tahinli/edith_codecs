//! Native VP8 (RFC 6386) decoder — the future replacement for edith's
//! runtime-dlopen'd libvpx seat.
//!
//! The crate is written from the specification ([RFC 6386]) alone; libvpx's
//! output is used only as a test oracle (sample-exactness witnesses), never
//! as code to port. Every module documents the spec section it implements.
//!
//! [RFC 6386]: https://www.rfc-editor.org/rfc/rfc6386
//!
//! # Unsafe
//!
//! This crate contains no `unsafe` code.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bool;
pub mod frame;
pub mod header;
pub mod tables;

pub use ec_core::{Error, Result};
pub use header::PersistedState;
