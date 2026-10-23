//! Range extension residual steps shared by the transform-skip, transquant-bypass and
//! cross-component paths (8.6.2, 8.6.6, 8.6.8).

/// Rotate an `n` x `n` block by 180 degrees in place (`transform_skip_rotation_enabled_flag`).
pub fn rotate_180(block: &mut [i32], n: usize) {
    block[..n * n].reverse();
}

/// The directional residual modification of RDPCM (8.6.8): each sample accumulates the
/// one before it, horizontally for `dir` 0 and vertically for `dir` 1.
pub fn rdpcm_accumulate(block: &mut [i32], n: usize, dir: u8) {
    if dir == 0 {
        for y in 0..n {
            for x in 1..n {
                block[y * n + x] += block[y * n + x - 1];
            }
        }
    } else {
        for y in 1..n {
            for x in 0..n {
                block[y * n + x] += block[(y - 1) * n + x];
            }
        }
    }
}

/// Cross-component prediction (8.6.6): `r += (ResScaleVal * ((rY << BitDepthC) >> BitDepthY)) >> 3`.
pub fn cross_component(chroma: &mut [i32], luma: &[i32], res_scale: i32, bd_y: u32, bd_c: u32) {
    for (c, &l) in chroma.iter_mut().zip(luma) {
        *c += (res_scale * ((l << bd_c) >> bd_y)) >> 3;
    }
}
