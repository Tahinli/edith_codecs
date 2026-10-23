//! Fixed-point 32- and 64-point IMDCT halves of the synthesis bank
//! (ETSI TS 102 114 annex C), in the integer formulation whose rounding is
//! the reference: every step is a Q23 multiply-and-round or an exact sum,
//! with 24-bit saturation between stages.

use crate::math::{clip23, mul23, norm};

fn sum_a(input: &[i32], output: &mut [i32], len: usize) {
    for i in 0..len {
        output[i] = input[2 * i].wrapping_add(input[2 * i + 1]);
    }
}

fn sum_b(input: &[i32], output: &mut [i32], len: usize) {
    output[0] = input[0];
    for i in 1..len {
        output[i] = input[2 * i].wrapping_add(input[2 * i - 1]);
    }
}

fn sum_c(input: &[i32], output: &mut [i32], len: usize) {
    for i in 0..len {
        output[i] = input[2 * i];
    }
}

fn sum_d(input: &[i32], output: &mut [i32], len: usize) {
    output[0] = input[1];
    for i in 1..len {
        output[i] = input[2 * i - 1].wrapping_add(input[2 * i + 1]);
    }
}

fn dct_a(input: &[i32], output: &mut [i32]) {
    const COS_MOD: [[i32; 8]; 8] = [
        [8348215, 8027397, 7398092, 6484482, 5321677, 3954362, 2435084, 822227],
        [8027397, 5321677, 822227, -3954362, -7398092, -8348215, -6484482, -2435084],
        [7398092, 822227, -6484482, -8027397, -2435084, 5321677, 8348215, 3954362],
        [6484482, -3954362, -8027397, 822227, 8348215, 2435084, -7398092, -5321677],
        [5321677, -7398092, -2435084, 8348215, -822227, -8027397, 3954362, 6484482],
        [3954362, -8348215, 5321677, 2435084, -8027397, 6484482, 822227, -7398092],
        [2435084, -6484482, 8348215, -7398092, 3954362, 822227, -5321677, 8027397],
        [822227, -2435084, 3954362, -5321677, 6484482, -7398092, 8027397, -8348215],
    ];
    for i in 0..8 {
        let mut res: i64 = 0;
        for j in 0..8 {
            res += COS_MOD[i][j] as i64 * input[j] as i64;
        }
        output[i] = norm(res, 23);
    }
}

fn dct_b(input: &[i32], output: &mut [i32]) {
    const COS_MOD: [[i32; 7]; 8] = [
        [8227423, 7750063, 6974873, 5931642, 4660461, 3210181, 1636536],
        [6974873, 3210181, -1636536, -5931642, -8227423, -7750063, -4660461],
        [4660461, -3210181, -8227423, -5931642, 1636536, 7750063, 6974873],
        [1636536, -7750063, -4660461, 5931642, 6974873, -3210181, -8227423],
        [-1636536, -7750063, 4660461, 5931642, -6974873, -3210181, 8227423],
        [-4660461, -3210181, 8227423, -5931642, -1636536, 7750063, -6974873],
        [-6974873, 3210181, 1636536, -5931642, 8227423, -7750063, 4660461],
        [-8227423, 7750063, -6974873, 5931642, -4660461, 3210181, -1636536],
    ];
    for i in 0..8 {
        let mut res: i64 = (input[0] as i64) << 23;
        for j in 0..7 {
            res += COS_MOD[i][j] as i64 * input[1 + j] as i64;
        }
        output[i] = norm(res, 23);
    }
}

fn mod_a(input: &[i32], output: &mut [i32]) {
    const COS_MOD: [i32; 16] = [
        4199362, 4240198, 4323885, 4454708, 4639772, 4890013, 5221943, 5660703, -6245623,
        -7040975, -8158494, -9809974, -12450076, -17261920, -28585092, -85479984,
    ];
    for i in 0..8 {
        output[i] = mul23(COS_MOD[i], input[i].wrapping_add(input[8 + i]));
    }
    let mut k = 7usize;
    for i in 8..16 {
        output[i] = mul23(COS_MOD[i], input[k].wrapping_sub(input[8 + k]));
        k = k.wrapping_sub(1);
    }
}

fn mod_b(input: &mut [i32], output: &mut [i32]) {
    const COS_MOD: [i32; 8] = [
        4214598, 4383036, 4755871, 5425934, 6611520, 8897610, 14448934, 42791536,
    ];
    for i in 0..8 {
        input[8 + i] = mul23(COS_MOD[i], input[8 + i]);
    }
    for i in 0..8 {
        output[i] = input[i].wrapping_add(input[8 + i]);
    }
    let mut k = 7usize;
    for i in 8..16 {
        output[i] = input[k].wrapping_sub(input[8 + k]);
        k = k.wrapping_sub(1);
    }
}

fn mod_c(input: &[i32], output: &mut [i32]) {
    const COS_MOD: [i32; 32] = [
        1048892, 1051425, 1056522, 1064244, 1074689, 1087987, 1104313, 1123884, 1146975, 1173922,
        1205139, 1241133, 1282529, 1330095, 1384791, 1447815, -1520688, -1605358, -1704360,
        -1821051, -1959964, -2127368, -2332183, -2587535, -2913561, -3342802, -3931480, -4785806,
        -6133390, -8566050, -14253820, -42727120,
    ];
    for i in 0..16 {
        output[i] = mul23(COS_MOD[i], input[i].wrapping_add(input[16 + i]));
    }
    let mut k = 15usize;
    for i in 16..32 {
        output[i] = mul23(COS_MOD[i], input[k].wrapping_sub(input[16 + k]));
        k = k.wrapping_sub(1);
    }
}

fn clp(v: &mut [i32]) {
    for x in v.iter_mut() {
        *x = clip23(*x);
    }
}

fn pre_shift(input: &[i32], buf_a: &mut [i32]) -> u32 {
    let mut mag: i32 = 0;
    for &x in input {
        mag = mag.wrapping_add(x.wrapping_abs());
    }
    let shift = if mag > 0x400000 { 2 } else { 0 };
    let round = if shift > 0 { 1 << (shift - 1) } else { 0 };
    for (a, &x) in buf_a.iter_mut().zip(input) {
        *a = x.wrapping_add(round) >> shift;
    }
    shift
}

/// 32-point IMDCT half: 32 subband samples in, 32 samples out.
pub(crate) fn imdct_half_32(output: &mut [i32], input: &[i32]) {
    let mut buf_a = [0i32; 32];
    let mut buf_b = [0i32; 32];
    let shift = pre_shift(&input[..32], &mut buf_a);

    {
        let (lo, hi) = buf_b.split_at_mut(16);
        sum_a(&buf_a, lo, 16);
        sum_b(&buf_a, hi, 16);
    }
    clp(&mut buf_b);

    {
        let (lo, hi) = buf_a.split_at_mut(16);
        sum_a(&buf_b[0..], &mut lo[0..], 8);
        sum_b(&buf_b[0..], &mut lo[8..], 8);
        sum_c(&buf_b[16..], &mut hi[0..], 8);
        sum_d(&buf_b[16..], &mut hi[8..], 8);
    }
    clp(&mut buf_a);

    dct_a(&buf_a[0..], &mut buf_b[0..]);
    dct_b(&buf_a[8..], &mut buf_b[8..]);
    dct_b(&buf_a[16..], &mut buf_b[16..]);
    dct_b(&buf_a[24..], &mut buf_b[24..]);
    clp(&mut buf_b);

    mod_a(&buf_b[0..], &mut buf_a[0..]);
    mod_b(&mut buf_b[16..], &mut buf_a[16..]);
    clp(&mut buf_a);

    mod_c(&buf_a, &mut buf_b);

    for x in buf_b.iter_mut() {
        *x = clip23(x.wrapping_mul(1 << shift));
    }
    let mut k = 31usize;
    for i in 0..16 {
        output[i] = clip23(buf_b[i].wrapping_sub(buf_b[k]));
        output[16 + i] = clip23(buf_b[i].wrapping_add(buf_b[k]));
        k = k.wrapping_sub(1);
    }
}

fn mod64_a(input: &[i32], output: &mut [i32]) {
    const COS_MOD: [i32; 32] = [
        4195568, 4205700, 4226086, 4256977, 4298755, 4351949, 4417251, 4495537, 4587901, 4695690,
        4820557, 4964534, 5130115, 5320382, 5539164, 5791261, -6082752, -6421430, -6817439,
        -7284203, -7839855, -8509474, -9328732, -10350140, -11654242, -13371208, -15725922,
        -19143224, -24533560, -34264200, -57015280, -170908480,
    ];
    for i in 0..16 {
        output[i] = mul23(COS_MOD[i], input[i].wrapping_add(input[16 + i]));
    }
    let mut k = 15usize;
    for i in 16..32 {
        output[i] = mul23(COS_MOD[i], input[k].wrapping_sub(input[16 + k]));
        k = k.wrapping_sub(1);
    }
}

fn mod64_b(input: &mut [i32], output: &mut [i32]) {
    const COS_MOD: [i32; 16] = [
        4199362, 4240198, 4323885, 4454708, 4639772, 4890013, 5221943, 5660703, 6245623, 7040975,
        8158494, 9809974, 12450076, 17261920, 28585092, 85479984,
    ];
    for i in 0..16 {
        input[16 + i] = mul23(COS_MOD[i], input[16 + i]);
    }
    for i in 0..16 {
        output[i] = input[i].wrapping_add(input[16 + i]);
    }
    let mut k = 15usize;
    for i in 16..32 {
        output[i] = input[k].wrapping_sub(input[16 + k]);
        k = k.wrapping_sub(1);
    }
}

fn mod64_c(input: &[i32], output: &mut [i32]) {
    const COS_MOD: [i32; 64] = [
        741511, 741958, 742853, 744199, 746001, 748262, 750992, 754197, 757888, 762077, 766777,
        772003, 777772, 784105, 791021, 798546, 806707, 815532, 825054, 835311, 846342, 858193,
        870912, 884554, 899181, 914860, 931667, 949686, 969011, 989747, 1012012, 1035941, -1061684,
        -1089412, -1119320, -1151629, -1186595, -1224511, -1265719, -1310613, -1359657, -1413400,
        -1472490, -1537703, -1609974, -1690442, -1780506, -1881904, -1996824, -2128058, -2279225,
        -2455101, -2662128, -2909200, -3208956, -3579983, -4050785, -4667404, -5509372, -6726913,
        -8641940, -12091426, -20144284, -60420720,
    ];
    for i in 0..32 {
        output[i] = mul23(COS_MOD[i], input[i].wrapping_add(input[32 + i]));
    }
    let mut k = 31usize;
    for i in 32..64 {
        output[i] = mul23(COS_MOD[i], input[k].wrapping_sub(input[32 + k]));
        k = k.wrapping_sub(1);
    }
}

/// 64-point IMDCT half for the X96 bank.
pub(crate) fn imdct_half_64(output: &mut [i32], input: &[i32]) {
    let mut buf_a = [0i32; 64];
    let mut buf_b = [0i32; 64];
    let shift = pre_shift(&input[..64], &mut buf_a);

    {
        let (lo, hi) = buf_b.split_at_mut(32);
        sum_a(&buf_a, lo, 32);
        sum_b(&buf_a, hi, 32);
    }
    clp(&mut buf_b);

    {
        let (lo, hi) = buf_a.split_at_mut(32);
        sum_a(&buf_b[0..], &mut lo[0..], 16);
        sum_b(&buf_b[0..], &mut lo[16..], 16);
        sum_c(&buf_b[32..], &mut hi[0..], 16);
        sum_d(&buf_b[32..], &mut hi[16..], 16);
    }
    clp(&mut buf_a);

    sum_a(&buf_a[0..], &mut buf_b[0..], 8);
    sum_b(&buf_a[0..], &mut buf_b[8..], 8);
    sum_c(&buf_a[16..], &mut buf_b[16..], 8);
    sum_d(&buf_a[16..], &mut buf_b[24..], 8);
    sum_c(&buf_a[32..], &mut buf_b[32..], 8);
    sum_d(&buf_a[32..], &mut buf_b[40..], 8);
    sum_c(&buf_a[48..], &mut buf_b[48..], 8);
    sum_d(&buf_a[48..], &mut buf_b[56..], 8);
    clp(&mut buf_b);

    dct_a(&buf_b[0..], &mut buf_a[0..]);
    for k in 1..8 {
        dct_b(&buf_b[8 * k..], &mut buf_a[8 * k..]);
    }
    clp(&mut buf_a);

    mod_a(&buf_a[0..], &mut buf_b[0..]);
    mod_b(&mut buf_a[16..], &mut buf_b[16..]);
    mod_b(&mut buf_a[32..], &mut buf_b[32..]);
    mod_b(&mut buf_a[48..], &mut buf_b[48..]);
    clp(&mut buf_b);

    mod64_a(&buf_b[0..], &mut buf_a[0..]);
    mod64_b(&mut buf_b[32..], &mut buf_a[32..]);
    clp(&mut buf_a);

    mod64_c(&buf_a, &mut buf_b);

    for x in buf_b.iter_mut() {
        *x = clip23(x.wrapping_mul(1 << shift));
    }
    let mut k = 63usize;
    for i in 0..32 {
        output[i] = clip23(buf_b[i].wrapping_sub(buf_b[k]));
        output[32 + i] = clip23(buf_b[i].wrapping_add(buf_b[k]));
        k = k.wrapping_sub(1);
    }
}
