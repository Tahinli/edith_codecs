//! Picture order count (8.3.1).

/// `PicOrderCntVal` of a picture from its `slice_pic_order_cnt_lsb`.
///
/// `prev_tid0_poc` is the POC of `prevTid0Pic`, the previous picture with
/// TemporalId 0 that is not a RASL, RADL or sub-layer non-reference picture.
/// An IRAP picture with NoRaslOutputFlag set always has a zero MSB.
pub fn derive_poc(
    prev_tid0_poc: i32,
    log2_max_poc_lsb: u32,
    poc_lsb: u32,
    irap_no_rasl: bool,
) -> i32 {
    let max_lsb = 1i64 << log2_max_poc_lsb;
    let lsb = i64::from(poc_lsb);
    let msb = if irap_no_rasl {
        0
    } else {
        let prev = i64::from(prev_tid0_poc);
        let prev_lsb = prev & (max_lsb - 1);
        let prev_msb = prev - prev_lsb;
        if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
            prev_msb + max_lsb
        } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
            prev_msb - max_lsb
        } else {
            prev_msb
        }
    };
    (msb + lsb) as i32
}
