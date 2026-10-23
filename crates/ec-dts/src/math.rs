//! Fixed-point helpers shared by the dequantizer and the synthesis bank.

/// Round-to-nearest right shift of a 64-bit product, truncated to 32 bits.
#[inline]
pub(crate) fn norm(a: i64, bits: u32) -> i32 {
    if bits > 0 {
        ((a + (1i64 << (bits - 1))) >> bits) as i32
    } else {
        a as i32
    }
}

#[inline]
pub(crate) fn mul(a: i32, b: i32, bits: u32) -> i32 {
    norm(a as i64 * b as i64, bits)
}

#[inline]
pub(crate) fn mul23(a: i32, b: i32) -> i32 {
    mul(a, b, 23)
}

/// Clip to the signed 24-bit range.
#[inline]
pub(crate) fn clip23(a: i32) -> i32 {
    a.clamp(-(1 << 23), (1 << 23) - 1)
}
