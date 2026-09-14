//! Integer inverse transforms (spec 8.5.3; the libvpx `inv_txfm.c`
//! butterflies, rounding and shifts transcribed exactly). Release-build
//! `WRAPLOW` is an identity, so intermediates stay `i32`; only the final
//! add clamps.

use crate::tables::*;

#[inline]
fn dcrs(x: i64) -> i32 {
    ((x + (1 << 13)) >> 14) as i32
}

const C1: i64 = 16364;
const C2: i64 = 16305;
const C3: i64 = 16207;
const C4: i64 = 16069;
const C5: i64 = 15893;
const C6: i64 = 15679;
const C7: i64 = 15426;
const C8: i64 = 15137;
const C9: i64 = 14811;
const C10: i64 = 14449;
const C11: i64 = 14053;
const C12: i64 = 13623;
const C13: i64 = 13160;
const C14: i64 = 12665;
const C15: i64 = 12140;
const C16: i64 = 11585;
const C17: i64 = 11003;
const C18: i64 = 10394;
const C19: i64 = 9760;
const C20: i64 = 9102;
const C21: i64 = 8423;
const C22: i64 = 7723;
const C23: i64 = 7005;
const C24: i64 = 6270;
const C25: i64 = 5520;
const C26: i64 = 4756;
const C27: i64 = 3981;
const C28: i64 = 3196;
const C29: i64 = 2404;
const C30: i64 = 1606;
const C31: i64 = 804;
const S1: i64 = 5283;
const S2: i64 = 9929;
const S3: i64 = 13377;
const S4: i64 = 15212;

/// `clip_pixel_add`.
#[inline]
pub(crate) fn clip_add(d: u8, v: i32) -> u8 {
    (d as i32 + v).clamp(0, 255) as u8
}

/// vpx_iwht4x4_16_add_c (lossless).
pub(crate) fn iwht4x4_add(input: &[i32], dest: &mut [u8], stride: usize) {
    let mut output = [0i32; 16];
    for i in 0..4 {
        let ip = &input[i * 4..i * 4 + 4];
        let mut a1 = ip[0] >> 2;
        let mut c1 = ip[1] >> 2;
        let mut d1 = ip[2] >> 2;
        let mut b1 = ip[3] >> 2;
        a1 += c1;
        d1 -= b1;
        let e1 = (a1 - d1) >> 1;
        b1 = e1 - b1;
        c1 = e1 - c1;
        a1 -= b1;
        d1 += c1;
        output[i * 4..i * 4 + 4].copy_from_slice(&[a1, b1, c1, d1]);
    }
    for i in 0..4 {
        let a0 = output[i];
        let c0 = output[4 + i];
        let d0 = output[8 + i];
        let b0 = output[12 + i];
        let a2 = a0 + c0;
        let d2 = d0 - b0;
        let e1 = (a2 - d2) >> 1;
        let b1 = e1 - b0;
        let c1 = e1 - c0;
        let a1 = a2 - b1;
        let d1 = d2 + c1;
        dest[i] = clip_add(dest[i], a1);
        dest[stride + i] = clip_add(dest[stride + i], b1);
        dest[stride * 2 + i] = clip_add(dest[stride * 2 + i], c1);
        dest[stride * 3 + i] = clip_add(dest[stride * 3 + i], d1);
    }
}

pub(crate) fn idct4(input: &[i32], output: &mut [i32]) {
    let g = |v: i32| v as i64;
    let mut step = [0i32; 4];
    let t1 = (g(input[0]) + g(input[2])) * C16;
    let t2 = (g(input[0]) - g(input[2])) * C16;
    step[0] = dcrs(t1);
    step[1] = dcrs(t2);
    let t1 = g(input[1]) * C24 - g(input[3]) * C8;
    let t2 = g(input[1]) * C8 + g(input[3]) * C24;
    step[2] = dcrs(t1);
    step[3] = dcrs(t2);
    output[0] = step[0] + step[3];
    output[1] = step[1] + step[2];
    output[2] = step[1] - step[2];
    output[3] = step[0] - step[3];
}

pub(crate) fn idct8(input: &[i32], output: &mut [i32]) {
    let g = |v: i32| v as i64;
    let mut step1 = [0i32; 8];
    let mut step2 = [0i32; 8];
    step1[0] = input[0];
    step1[1] = input[2];
    step1[2] = input[4];
    step1[3] = input[6];
    step1[4] = dcrs(g(input[1]) * C28 - g(input[7]) * C4);
    step1[7] = dcrs(g(input[1]) * C4 + g(input[7]) * C28);
    step1[5] = dcrs(g(input[5]) * C12 - g(input[3]) * C20);
    step1[6] = dcrs(g(input[5]) * C20 + g(input[3]) * C12);
    step2[0] = dcrs((g(step1[0]) + g(step1[2])) * C16);
    step2[1] = dcrs((g(step1[0]) - g(step1[2])) * C16);
    step2[2] = dcrs(g(step1[1]) * C24 - g(step1[3]) * C8);
    step2[3] = dcrs(g(step1[1]) * C8 + g(step1[3]) * C24);
    step2[4] = step1[4] + step1[5];
    step2[5] = step1[4] - step1[5];
    step2[6] = -step1[6] + step1[7];
    step2[7] = step1[6] + step1[7];
    step1[0] = step2[0] + step2[3];
    step1[1] = step2[1] + step2[2];
    step1[2] = step2[1] - step2[2];
    step1[3] = step2[0] - step2[3];
    step1[4] = step2[4];
    step1[5] = dcrs((g(step2[6]) - g(step2[5])) * C16);
    step1[6] = dcrs((g(step2[5]) + g(step2[6])) * C16);
    step1[7] = step2[7];
    output[0] = step1[0] + step1[7];
    output[1] = step1[1] + step1[6];
    output[2] = step1[2] + step1[5];
    output[3] = step1[3] + step1[4];
    output[4] = step1[3] - step1[4];
    output[5] = step1[2] - step1[5];
    output[6] = step1[1] - step1[6];
    output[7] = step1[0] - step1[7];
}

pub(crate) fn idct16(input: &[i32], output: &mut [i32]) {
    let g = |v: i32| v as i64;
    let mut step1 = [0i32; 16];
    let mut step2 = [0i32; 16];
    const ORD: [usize; 16] = [0, 16, 8, 24, 4, 20, 12, 28, 2, 18, 10, 26, 6, 22, 14, 30];
    for i in 0..16 {
        step1[i] = input[ORD[i] / 2];
    }
    step2[8] = dcrs(g(step1[8]) * C30 - g(step1[15]) * C2);
    step2[15] = dcrs(g(step1[8]) * C2 + g(step1[15]) * C30);
    step2[9] = dcrs(g(step1[9]) * C14 - g(step1[14]) * C18);
    step2[14] = dcrs(g(step1[9]) * C18 + g(step1[14]) * C14);
    step2[10] = dcrs(g(step1[10]) * C22 - g(step1[13]) * C10);
    step2[13] = dcrs(g(step1[10]) * C10 + g(step1[13]) * C22);
    step2[11] = dcrs(g(step1[11]) * C6 - g(step1[12]) * C26);
    step2[12] = dcrs(g(step1[11]) * C26 + g(step1[12]) * C6);
    step2[0..8].copy_from_slice(&step1[0..8]);
    step1[4] = dcrs(g(step2[4]) * C28 - g(step2[7]) * C4);
    step1[7] = dcrs(g(step2[4]) * C4 + g(step2[7]) * C28);
    step1[5] = dcrs(g(step2[5]) * C12 - g(step2[6]) * C20);
    step1[6] = dcrs(g(step2[5]) * C20 + g(step2[6]) * C12);
    step1[0..4].copy_from_slice(&step2[0..4]);
    step1[8] = step2[8] + step2[9];
    step1[9] = step2[8] - step2[9];
    step1[10] = -step2[10] + step2[11];
    step1[11] = step2[10] + step2[11];
    step1[12] = step2[12] + step2[13];
    step1[13] = step2[12] - step2[13];
    step1[14] = -step2[14] + step2[15];
    step1[15] = step2[14] + step2[15];
    step2[0] = dcrs((g(step1[0]) + g(step1[1])) * C16);
    step2[1] = dcrs((g(step1[0]) - g(step1[1])) * C16);
    step2[2] = dcrs(g(step1[2]) * C24 - g(step1[3]) * C8);
    step2[3] = dcrs(g(step1[2]) * C8 + g(step1[3]) * C24);
    step2[4] = step1[4] + step1[5];
    step2[5] = step1[4] - step1[5];
    step2[6] = -step1[6] + step1[7];
    step2[7] = step1[6] + step1[7];
    step2[8] = step1[8];
    step2[15] = step1[15];
    step2[9] = dcrs(-g(step1[9]) * C8 + g(step1[14]) * C24);
    step2[14] = dcrs(g(step1[9]) * C24 + g(step1[14]) * C8);
    step2[10] = dcrs(-g(step1[10]) * C24 - g(step1[13]) * C8);
    step2[13] = dcrs(-g(step1[10]) * C8 + g(step1[13]) * C24);
    step2[11] = step1[11];
    step2[12] = step1[12];
    step1[0] = step2[0] + step2[3];
    step1[1] = step2[1] + step2[2];
    step1[2] = step2[1] - step2[2];
    step1[3] = step2[0] - step2[3];
    step1[4] = step2[4];
    step1[5] = dcrs((g(step2[6]) - g(step2[5])) * C16);
    step1[6] = dcrs((g(step2[5]) + g(step2[6])) * C16);
    step1[7] = step2[7];
    step1[8] = step2[8] + step2[11];
    step1[9] = step2[9] + step2[10];
    step1[10] = step2[9] - step2[10];
    step1[11] = step2[8] - step2[11];
    step1[12] = -step2[12] + step2[15];
    step1[13] = -step2[13] + step2[14];
    step1[14] = step2[13] + step2[14];
    step1[15] = step2[12] + step2[15];
    step2[0] = step1[0] + step1[7];
    step2[1] = step1[1] + step1[6];
    step2[2] = step1[2] + step1[5];
    step2[3] = step1[3] + step1[4];
    step2[4] = step1[3] - step1[4];
    step2[5] = step1[2] - step1[5];
    step2[6] = step1[1] - step1[6];
    step2[7] = step1[0] - step1[7];
    step2[8] = step1[8];
    step2[9] = step1[9];
    step2[10] = dcrs((-g(step1[10]) + g(step1[13])) * C16);
    step2[13] = dcrs((g(step1[10]) + g(step1[13])) * C16);
    step2[11] = dcrs((-g(step1[11]) + g(step1[12])) * C16);
    step2[12] = dcrs((g(step1[11]) + g(step1[12])) * C16);
    step2[14] = step1[14];
    step2[15] = step1[15];
    for i in 0..8 {
        output[i] = step2[i] + step2[15 - i];
        output[15 - i] = step2[i] - step2[15 - i];
    }
}

pub(crate) fn idct32(input: &[i32], output: &mut [i32]) {
    let g = |v: i32| v as i64;
    let mut step1 = [0i32; 32];
    let mut step2 = [0i32; 32];
    for i in 0..16 {
        step1[i] = input[[0usize, 16, 8, 24, 4, 20, 12, 28, 2, 18, 10, 26, 6, 22, 14, 30][i]];
    }
    step1[16] = dcrs(g(input[1]) * C31 - g(input[31]) * C1);
    step1[31] = dcrs(g(input[1]) * C1 + g(input[31]) * C31);
    step1[17] = dcrs(g(input[17]) * C15 - g(input[15]) * C17);
    step1[30] = dcrs(g(input[17]) * C17 + g(input[15]) * C15);
    step1[18] = dcrs(g(input[9]) * C23 - g(input[23]) * C9);
    step1[29] = dcrs(g(input[9]) * C9 + g(input[23]) * C23);
    step1[19] = dcrs(g(input[25]) * C7 - g(input[7]) * C25);
    step1[28] = dcrs(g(input[25]) * C25 + g(input[7]) * C7);
    step1[20] = dcrs(g(input[5]) * C27 - g(input[27]) * C5);
    step1[27] = dcrs(g(input[5]) * C5 + g(input[27]) * C27);
    step1[21] = dcrs(g(input[21]) * C11 - g(input[11]) * C21);
    step1[26] = dcrs(g(input[21]) * C21 + g(input[11]) * C11);
    step1[22] = dcrs(g(input[13]) * C19 - g(input[19]) * C13);
    step1[25] = dcrs(g(input[13]) * C13 + g(input[19]) * C19);
    step1[23] = dcrs(g(input[29]) * C3 - g(input[3]) * C29);
    step1[24] = dcrs(g(input[29]) * C29 + g(input[3]) * C3);
    step2[0..8].copy_from_slice(&step1[0..8]);
    step2[8] = dcrs(g(step1[8]) * C30 - g(step1[15]) * C2);
    step2[15] = dcrs(g(step1[8]) * C2 + g(step1[15]) * C30);
    step2[9] = dcrs(g(step1[9]) * C14 - g(step1[14]) * C18);
    step2[14] = dcrs(g(step1[9]) * C18 + g(step1[14]) * C14);
    step2[10] = dcrs(g(step1[10]) * C22 - g(step1[13]) * C10);
    step2[13] = dcrs(g(step1[10]) * C10 + g(step1[13]) * C22);
    step2[11] = dcrs(g(step1[11]) * C6 - g(step1[12]) * C26);
    step2[12] = dcrs(g(step1[11]) * C26 + g(step1[12]) * C6);
    step2[16] = step1[16] + step1[17];
    step2[17] = step1[16] - step1[17];
    step2[18] = -step1[18] + step1[19];
    step2[19] = step1[18] + step1[19];
    step2[20] = step1[20] + step1[21];
    step2[21] = step1[20] - step1[21];
    step2[22] = -step1[22] + step1[23];
    step2[23] = step1[22] + step1[23];
    step2[24] = step1[24] + step1[25];
    step2[25] = step1[24] - step1[25];
    step2[26] = -step1[26] + step1[27];
    step2[27] = step1[26] + step1[27];
    step2[28] = step1[28] + step1[29];
    step2[29] = step1[28] - step1[29];
    step2[30] = -step1[30] + step1[31];
    step2[31] = step1[30] + step1[31];
    step1[0..4].copy_from_slice(&step2[0..4]);
    step1[4] = dcrs(g(step2[4]) * C28 - g(step2[7]) * C4);
    step1[7] = dcrs(g(step2[4]) * C4 + g(step2[7]) * C28);
    step1[5] = dcrs(g(step2[5]) * C12 - g(step2[6]) * C20);
    step1[6] = dcrs(g(step2[5]) * C20 + g(step2[6]) * C12);
    step1[8] = step2[8] + step2[9];
    step1[9] = step2[8] - step2[9];
    step1[10] = -step2[10] + step2[11];
    step1[11] = step2[10] + step2[11];
    step1[12] = step2[12] + step2[13];
    step1[13] = step2[12] - step2[13];
    step1[14] = -step2[14] + step2[15];
    step1[15] = step2[14] + step2[15];
    step1[16] = step2[16];
    step1[31] = step2[31];
    step1[17] = dcrs(-g(step2[17]) * C4 + g(step2[30]) * C28);
    step1[30] = dcrs(g(step2[17]) * C28 + g(step2[30]) * C4);
    step1[18] = dcrs(-g(step2[18]) * C28 - g(step2[29]) * C4);
    step1[29] = dcrs(-g(step2[18]) * C4 + g(step2[29]) * C28);
    step1[19] = step2[19];
    step1[20] = step2[20];
    step1[21] = dcrs(-g(step2[21]) * C20 + g(step2[26]) * C12);
    step1[26] = dcrs(g(step2[21]) * C12 + g(step2[26]) * C20);
    step1[22] = dcrs(-g(step2[22]) * C12 - g(step2[25]) * C20);
    step1[25] = dcrs(-g(step2[22]) * C20 + g(step2[25]) * C12);
    step1[23] = step2[23];
    step1[24] = step2[24];
    step1[27] = step2[27];
    step1[28] = step2[28];
    step2[0] = dcrs((g(step1[0]) + g(step1[1])) * C16);
    step2[1] = dcrs((g(step1[0]) - g(step1[1])) * C16);
    step2[2] = dcrs(g(step1[2]) * C24 - g(step1[3]) * C8);
    step2[3] = dcrs(g(step1[2]) * C8 + g(step1[3]) * C24);
    step2[4] = step1[4] + step1[5];
    step2[5] = step1[4] - step1[5];
    step2[6] = -step1[6] + step1[7];
    step2[7] = step1[6] + step1[7];
    step2[8] = step1[8];
    step2[15] = step1[15];
    step2[9] = dcrs(-g(step1[9]) * C8 + g(step1[14]) * C24);
    step2[14] = dcrs(g(step1[9]) * C24 + g(step1[14]) * C8);
    step2[10] = dcrs(-g(step1[10]) * C24 - g(step1[13]) * C8);
    step2[13] = dcrs(-g(step1[10]) * C8 + g(step1[13]) * C24);
    step2[11] = step1[11];
    step2[12] = step1[12];
    step2[16] = step1[16] + step1[19];
    step2[17] = step1[17] + step1[18];
    step2[18] = step1[17] - step1[18];
    step2[19] = step1[16] - step1[19];
    step2[20] = -step1[20] + step1[23];
    step2[21] = -step1[21] + step1[22];
    step2[22] = step1[21] + step1[22];
    step2[23] = step1[20] + step1[23];
    step2[24] = step1[24] + step1[27];
    step2[25] = step1[25] + step1[26];
    step2[26] = step1[25] - step1[26];
    step2[27] = step1[24] - step1[27];
    step2[28] = -step1[28] + step1[31];
    step2[29] = -step1[29] + step1[30];
    step2[30] = step1[29] + step1[30];
    step2[31] = step1[28] + step1[31];
    step1[0] = step2[0] + step2[3];
    step1[1] = step2[1] + step2[2];
    step1[2] = step2[1] - step2[2];
    step1[3] = step2[0] - step2[3];
    step1[4] = step2[4];
    step1[5] = dcrs((g(step2[6]) - g(step2[5])) * C16);
    step1[6] = dcrs((g(step2[5]) + g(step2[6])) * C16);
    step1[7] = step2[7];
    step1[8] = step2[8] + step2[11];
    step1[9] = step2[9] + step2[10];
    step1[10] = step2[9] - step2[10];
    step1[11] = step2[8] - step2[11];
    step1[12] = -step2[12] + step2[15];
    step1[13] = -step2[13] + step2[14];
    step1[14] = step2[13] + step2[14];
    step1[15] = step2[12] + step2[15];
    step1[16] = step2[16];
    step1[17] = step2[17];
    step1[18] = dcrs(-g(step2[18]) * C8 + g(step2[29]) * C24);
    step1[29] = dcrs(g(step2[18]) * C24 + g(step2[29]) * C8);
    step1[19] = dcrs(-g(step2[19]) * C8 + g(step2[28]) * C24);
    step1[28] = dcrs(g(step2[19]) * C24 + g(step2[28]) * C8);
    step1[20] = dcrs(-g(step2[20]) * C24 - g(step2[27]) * C8);
    step1[27] = dcrs(-g(step2[20]) * C8 + g(step2[27]) * C24);
    step1[21] = dcrs(-g(step2[21]) * C24 - g(step2[26]) * C8);
    step1[26] = dcrs(-g(step2[21]) * C8 + g(step2[26]) * C24);
    step1[22] = step2[22];
    step1[23] = step2[23];
    step1[24] = step2[24];
    step1[25] = step2[25];
    step1[30] = step2[30];
    step1[31] = step2[31];
    step2[0] = step1[0] + step1[7];
    step2[1] = step1[1] + step1[6];
    step2[2] = step1[2] + step1[5];
    step2[3] = step1[3] + step1[4];
    step2[4] = step1[3] - step1[4];
    step2[5] = step1[2] - step1[5];
    step2[6] = step1[1] - step1[6];
    step2[7] = step1[0] - step1[7];
    step2[8] = step1[8];
    step2[9] = step1[9];
    step2[10] = dcrs((-g(step1[10]) + g(step1[13])) * C16);
    step2[13] = dcrs((g(step1[10]) + g(step1[13])) * C16);
    step2[11] = dcrs((-g(step1[11]) + g(step1[12])) * C16);
    step2[12] = dcrs((g(step1[11]) + g(step1[12])) * C16);
    step2[14] = step1[14];
    step2[15] = step1[15];
    step2[16] = step1[16] + step1[23];
    step2[17] = step1[17] + step1[22];
    step2[18] = step1[18] + step1[21];
    step2[19] = step1[19] + step1[20];
    step2[20] = step1[19] - step1[20];
    step2[21] = step1[18] - step1[21];
    step2[22] = step1[17] - step1[22];
    step2[23] = step1[16] - step1[23];
    step2[24] = -step1[24] + step1[31];
    step2[25] = -step1[25] + step1[30];
    step2[26] = -step1[26] + step1[29];
    step2[27] = -step1[27] + step1[28];
    step2[28] = step1[27] + step1[28];
    step2[29] = step1[26] + step1[29];
    step2[30] = step1[25] + step1[30];
    step2[31] = step1[24] + step1[31];
    step1[0] = step2[0] + step2[15];
    step1[1] = step2[1] + step2[14];
    step1[2] = step2[2] + step2[13];
    step1[3] = step2[3] + step2[12];
    step1[4] = step2[4] + step2[11];
    step1[5] = step2[5] + step2[10];
    step1[6] = step2[6] + step2[9];
    step1[7] = step2[7] + step2[8];
    step1[8] = step2[7] - step2[8];
    step1[9] = step2[6] - step2[9];
    step1[10] = step2[5] - step2[10];
    step1[11] = step2[4] - step2[11];
    step1[12] = step2[3] - step2[12];
    step1[13] = step2[2] - step2[13];
    step1[14] = step2[1] - step2[14];
    step1[15] = step2[0] - step2[15];
    // stage 8 (inv_txfm.c:1120): cospi_16 rotation of 20..27 before the
    // final combine — omitting it desyncs every 32x32 residual.
    step1[16] = step2[16];
    step1[17] = step2[17];
    step1[18] = step2[18];
    step1[19] = step2[19];
    step1[20] = dcrs((-g(step2[20]) + g(step2[27])) * C16);
    step1[27] = dcrs((g(step2[20]) + g(step2[27])) * C16);
    step1[21] = dcrs((-g(step2[21]) + g(step2[26])) * C16);
    step1[26] = dcrs((g(step2[21]) + g(step2[26])) * C16);
    step1[22] = dcrs((-g(step2[22]) + g(step2[25])) * C16);
    step1[25] = dcrs((g(step2[22]) + g(step2[25])) * C16);
    step1[23] = dcrs((-g(step2[23]) + g(step2[24])) * C16);
    step1[24] = dcrs((g(step2[23]) + g(step2[24])) * C16);
    step1[28] = step2[28];
    step1[29] = step2[29];
    step1[30] = step2[30];
    step1[31] = step2[31];
    for i in 0..16 {
        output[i] = step1[i] + step1[31 - i];
        output[31 - i] = step1[i] - step1[31 - i];
    }
}

pub(crate) fn iadst4(input: &[i32], output: &mut [i32]) {
    let (x0, x1, x2, x3) = (input[0] as i64, input[1] as i64, input[2] as i64, input[3] as i64);
    if x0 | x1 | x2 | x3 == 0 {
        output[..4].fill(0);
        return;
    }
    let s0 = S1 * x0 + S4 * x2 + S2 * x3;
    let s1 = S2 * x0 - S1 * x2 - S4 * x3;
    let s3 = S3 * x1;
    let s7 = (x0 - x2 + x3) as i32 as i64;
    let s2 = S3 * s7;
    output[0] = dcrs(s0 + s3);
    output[1] = dcrs(s1 + s3);
    output[2] = dcrs(s2);
    output[3] = dcrs(s0 + s1 - s3);
}

pub(crate) fn iadst8(input: &[i32], output: &mut [i32]) {
    let g = |v: i32| v as i64;
    let mut x0 = g(input[7]);
    let mut x1 = g(input[0]);
    let mut x2 = g(input[5]);
    let mut x3 = g(input[2]);
    let mut x4 = g(input[3]);
    let mut x5 = g(input[4]);
    let mut x6 = g(input[1]);
    let mut x7 = g(input[6]);
    if x0 | x1 | x2 | x3 | x4 | x5 | x6 | x7 == 0 {
        output[..8].fill(0);
        return;
    }
    let s0 = C2 * x0 + C30 * x1;
    let s1 = C30 * x0 - C2 * x1;
    let s2 = C10 * x2 + C22 * x3;
    let s3 = C22 * x2 - C10 * x3;
    let s4 = C18 * x4 + C14 * x5;
    let s5 = C14 * x4 - C18 * x5;
    let s6 = C26 * x6 + C6 * x7;
    let s7 = C6 * x6 - C26 * x7;
    x0 = dcrs(s0 + s4) as i64;
    x1 = dcrs(s1 + s5) as i64;
    x2 = dcrs(s2 + s6) as i64;
    x3 = dcrs(s3 + s7) as i64;
    x4 = dcrs(s0 - s4) as i64;
    x5 = dcrs(s1 - s5) as i64;
    x6 = dcrs(s2 - s6) as i64;
    x7 = dcrs(s3 - s7) as i64;
    let (t0, t1, t2, t3) = (x0, x1, x2, x3);
    let s4 = C8 * x4 + C24 * x5;
    let s5 = C24 * x4 - C8 * x5;
    let s6 = -C24 * x6 + C8 * x7;
    let s7 = C8 * x6 + C24 * x7;
    x0 = (t0 + t2) as i32 as i64;
    x1 = (t1 + t3) as i32 as i64;
    x2 = (t0 - t2) as i32 as i64;
    x3 = (t1 - t3) as i32 as i64;
    x4 = dcrs(s4 + s6) as i64;
    x5 = dcrs(s5 + s7) as i64;
    x6 = dcrs(s4 - s6) as i64;
    x7 = dcrs(s5 - s7) as i64;
    let s2 = C16 * (x2 + x3);
    let s3 = C16 * (x2 - x3);
    let s6 = C16 * (x6 + x7);
    let s7 = C16 * (x6 - x7);
    x2 = dcrs(s2) as i64;
    x3 = dcrs(s3) as i64;
    x6 = dcrs(s6) as i64;
    x7 = dcrs(s7) as i64;
    output[0] = x0 as i32;
    output[1] = -x4 as i32;
    output[2] = x6 as i32;
    output[3] = -x2 as i32;
    output[4] = x3 as i32;
    output[5] = -x7 as i32;
    output[6] = x5 as i32;
    output[7] = -x1 as i32;
}

pub(crate) fn iadst16(input: &[i32], output: &mut [i32]) {
    let g = |v: i32| v as i64;
    let mut x = [
        g(input[15]), g(input[0]), g(input[13]), g(input[2]),
        g(input[11]), g(input[4]), g(input[9]), g(input[6]),
        g(input[7]), g(input[8]), g(input[5]), g(input[10]),
        g(input[3]), g(input[12]), g(input[1]), g(input[14]),
    ];
    if x.iter().all(|&v| v == 0) {
        output[..16].fill(0);
        return;
    }
    let mut s = [0i64; 16];
    s[0] = x[0] * C1 + x[1] * C31;
    s[1] = x[0] * C31 - x[1] * C1;
    s[2] = x[2] * C5 + x[3] * C27;
    s[3] = x[2] * C27 - x[3] * C5;
    s[4] = x[4] * C9 + x[5] * C23;
    s[5] = x[4] * C23 - x[5] * C9;
    s[6] = x[6] * C13 + x[7] * C19;
    s[7] = x[6] * C19 - x[7] * C13;
    s[8] = x[8] * C17 + x[9] * C15;
    s[9] = x[8] * C15 - x[9] * C17;
    s[10] = x[10] * C21 + x[11] * C11;
    s[11] = x[10] * C11 - x[11] * C21;
    s[12] = x[12] * C25 + x[13] * C7;
    s[13] = x[12] * C7 - x[13] * C25;
    s[14] = x[14] * C29 + x[15] * C3;
    s[15] = x[14] * C3 - x[15] * C29;
    for i in 0..8 {
        x[i] = dcrs(s[i] + s[i + 8]) as i64;
        x[i + 8] = dcrs(s[i] - s[i + 8]) as i64;
    }
    s[0..8].copy_from_slice(&x[0..8]);
    s[8] = x[8] * C4 + x[9] * C28;
    s[9] = x[8] * C28 - x[9] * C4;
    s[10] = x[10] * C20 + x[11] * C12;
    s[11] = x[10] * C12 - x[11] * C20;
    s[12] = -x[12] * C28 + x[13] * C4;
    s[13] = x[12] * C4 + x[13] * C28;
    s[14] = -x[14] * C12 + x[15] * C20;
    s[15] = x[14] * C20 + x[15] * C12;
    for i in 0..4 {
        x[i] = (s[i] + s[i + 4]) as i32 as i64;
        x[i + 4] = (s[i] - s[i + 4]) as i32 as i64;
    }
    for i in 0..4 {
        x[8 + i] = dcrs(s[8 + i] + s[12 + i]) as i64;
        x[12 + i] = dcrs(s[8 + i] - s[12 + i]) as i64;
    }
    s[0..4].copy_from_slice(&x[0..4]);
    s[8..12].copy_from_slice(&x[8..12]);
    s[4] = x[4] * C8 + x[5] * C24;
    s[5] = x[4] * C24 - x[5] * C8;
    s[6] = -x[6] * C24 + x[7] * C8;
    s[7] = x[6] * C8 + x[7] * C24;
    s[12] = x[12] * C8 + x[13] * C24;
    s[13] = x[12] * C24 - x[13] * C8;
    s[14] = -x[14] * C24 + x[15] * C8;
    s[15] = x[14] * C8 + x[15] * C24;
    x[0] = (s[0] + s[2]) as i32 as i64;
    x[1] = (s[1] + s[3]) as i32 as i64;
    x[2] = (s[0] - s[2]) as i32 as i64;
    x[3] = (s[1] - s[3]) as i32 as i64;
    x[4] = dcrs(s[4] + s[6]) as i64;
    x[5] = dcrs(s[5] + s[7]) as i64;
    x[6] = dcrs(s[4] - s[6]) as i64;
    x[7] = dcrs(s[5] - s[7]) as i64;
    x[8] = (s[8] + s[10]) as i32 as i64;
    x[9] = (s[9] + s[11]) as i32 as i64;
    x[10] = (s[8] - s[10]) as i32 as i64;
    x[11] = (s[9] - s[11]) as i32 as i64;
    x[12] = dcrs(s[12] + s[14]) as i64;
    x[13] = dcrs(s[13] + s[15]) as i64;
    x[14] = dcrs(s[12] - s[14]) as i64;
    x[15] = dcrs(s[13] - s[15]) as i64;
    for (i, j) in [(2usize, 3usize), (6, 7), (10, 11), (14, 15)] {
        let a = C16 * (x[i] + x[j]);
        let b = C16 * (x[i] - x[j]);
        x[i] = dcrs(a) as i64;
        x[j] = dcrs(b) as i64;
    }
    let o = [
        x[0], -x[8], x[12], -x[4], x[6], x[14], x[10], x[2],
        x[3], x[11], x[15], x[7], x[5], x[13], x[9], -x[1],
    ];
    for (i, &v) in o.iter().enumerate() {
        output[i] = v as i32;
    }
}

/// One 2D transform pass: `rows` then `cols` then add with the block's
/// rounding shift (vpx_idct4x4_16_add_c and friends; vp9_iht* for the
/// ADST combinations).
pub(crate) fn inverse_transform_add(
    tx_size: usize,
    tx_type: usize,
    coeffs: &[i32],
    dest: &mut [u8],
    off: usize,
    stride: usize,
    lossless: bool,
) {
    if lossless {
        iwht4x4_add(coeffs, &mut dest[off..], stride);
        return;
    }
    if tx_size == TX_32X32 {
        let mut out = [0i32; 1024];
        for i in 0..32 {
            idct32(&coeffs[i * 32..], &mut out[i * 32..]);
        }
        let mut temp_in = [0i32; 32];
        let mut temp_out = [0i32; 32];
        for i in 0..32 {
            for (j, ti) in temp_in.iter_mut().enumerate() {
                *ti = out[j * 32 + i];
            }
            idct32(&temp_in, &mut temp_out);
            for j in 0..32 {
                let d = &mut dest[off + j * stride + i];
                *d = clip_add(*d, (temp_out[j] + 32) >> 6);
            }
        }
        return;
    }
    let (n, shift) = match tx_size {
        TX_4X4 => (4usize, 4u32),
        TX_8X8 => (8, 5),
        _ => (16, 6),
    };
    // ADST_DCT codes ADST vertical (= cols pass); DCT_ADST horizontal.
    let adst_cols = matches!(tx_type, ADST_DCT | ADST_ADST);
    let adst_rows = matches!(tx_type, DCT_ADST | ADST_ADST);
    let (row_fn, col_fn): (fn(&[i32], &mut [i32]), fn(&[i32], &mut [i32])) = match tx_size {
        TX_4X4 => (
            if adst_rows { iadst4 } else { idct4 },
            if adst_cols { iadst4 } else { idct4 },
        ),
        TX_8X8 => (
            if adst_rows { iadst8 } else { idct8 },
            if adst_cols { iadst8 } else { idct8 },
        ),
        _ => (
            if adst_rows { iadst16 } else { idct16 },
            if adst_cols { iadst16 } else { idct16 },
        ),
    };
    let mut out = vec![0i32; n * n];
    for i in 0..n {
        row_fn(&coeffs[i * n..], &mut out[i * n..]);
    }
    let mut temp_in = vec![0i32; n];
    let mut temp_out = vec![0i32; n];
    for i in 0..n {
        for j in 0..n {
            temp_in[j] = out[j * n + i];
        }
        col_fn(&temp_in, &mut temp_out);
        for j in 0..n {
            let d = &mut dest[off + j * stride + i];
            *d = clip_add(*d, (temp_out[j] + (1 << (shift - 1))) >> shift);
        }
    }
}


#[cfg(test)]
mod scratch_probe {
    use super::*;
    #[test]
    fn probe_dc38() {
        for (name, tt) in [("dct_dct", DCT_DCT), ("dct_adst", DCT_ADST)] {
            let mut c = [0i32; 64];
            c[0] = 38;
            let mut d = vec![129u8; 64];
            inverse_transform_add(TX_8X8, tt, &c, &mut d, 0, 8, false);
            println!("{name}: {:?}", &d[..8]);
        }
    }
}
