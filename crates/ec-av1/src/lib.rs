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

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bits;
pub mod cdf;
mod cdf_state;
mod compound;
pub mod decode;
pub mod encode;
pub mod encoder;
mod film_grain;
pub mod frame;
pub mod intra;
pub mod mc;
pub mod motion;
mod motion_field;
pub mod msac;
pub mod mvstack;
pub mod obu;
pub mod quant;
mod restoration;
pub mod sequence;
pub mod stream;
pub mod tile;
pub mod transform;
mod warp;
mod wedge;
