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
//! This crate is `forbid(unsafe_code)`: it contains no `unsafe` block, fn or
//! impl, and the motion compensation kernels are safe Rust. The scalar
//! function each kernel replaces stays the reference implementation.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Codec signatures are tuples of slices and iterator adaptors; one alias per signature would add indirection without clarity.
#![allow(clippy::type_complexity)]

pub mod bits;
mod autothreads;
pub mod cdf;
mod cdf_state;
pub mod census;
mod compound;
pub mod decode;
pub mod encode;
mod dumpio;
pub mod encoder;
#[doc(hidden)]
pub mod fault;
mod envflags;
mod film_grain;
mod filter_search;
pub mod frame;
// Test-only: the real-library fixture root and the one presence probe every
// gate that needs a clip routes through.
mod gate_coverage;
mod hits;
pub mod intra;
#[cfg(test)]
mod library_fixture;
pub mod mc;
pub mod motion;
mod motion_field;
pub mod msac;
pub mod mvstack;
pub mod obu;
mod par;
pub mod probe;
pub mod qm;
pub mod quant;
mod refusal_inventory;
mod restoration;
pub mod sequence;
pub mod speed;
mod stack_budget;
pub mod stream;
mod superres;
#[cfg(test)]
mod test_source;
#[cfg(test)]
pub(crate) mod tool_cache;
pub mod tile;
mod timeline;
pub mod transform;
mod warp;
mod wedge;
