//! AMR-NB decoder (fixed-point, bit-exact port of the 3GPP TS 26.073 reference as
//! carried by the Apache-2.0 opencore-amr sources).
#![allow(clippy::all)]
pub mod basic;
pub mod bitstream;
pub mod decoder;
pub mod dtx;
pub mod excitation;
pub mod lpc;
pub mod post;
pub mod tables;
