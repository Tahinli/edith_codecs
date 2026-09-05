//! AV1 OBU writer — bit-exact inverse of the [`ec_av1_syntax`] parsers, for
//! the stateless hardware-decoder subset (sequence header and key-frame header
//! OBUs).
//!
//! [`bits`] holds the bit-primitive writers every higher-level OBU writer is
//! built from; each is the direct inverse of the matching reader in
//! [`ec_av1_syntax::obu`] and is roundtripped against that reader in its tests.
//!
//! AV1 is covered by the AOMedia Patent License 1.0; this crate is an
//! independent implementation written from the specification alone.
//!
//! # Unsafe
//!
//! This crate is `deny(unsafe_code)` rather than `forbid`: the two motion
//! compensation inner loops are the largest single share of a decode's time
//! and are hand-vectorised with `std::arch::x86_64` intrinsics, which are
//! unsafe by signature. The exception is confined to [`mc`]'s `simd` module
//! (plus its dispatch and its tests, each carrying its own
//! `#[allow(unsafe_code)]`); every kernel there is checked lane-for-lane
//! against the scalar function it replaces, which stays the reference
//! implementation and is what every other architecture runs.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bits;
pub mod cdf;
mod cdf_state;
mod compound;
pub mod decode;
pub mod encode;
pub mod encoder;
mod envflags;
mod film_grain;
pub mod frame;
mod gate_coverage;
pub mod intra;
pub mod mc;
pub mod motion;
mod motion_field;
pub mod msac;
pub mod mvstack;
pub mod obu;
mod par;
pub mod quant;
mod refusal_inventory;
mod restoration;
pub mod sequence;
mod superres;
pub mod stream;
mod timeline;
pub mod tile;
pub mod transform;
mod warp;
mod wedge;
