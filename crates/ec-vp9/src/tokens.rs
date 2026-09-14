//! Coefficient token decoding (spec 8.5.2 `decode_coefs` / libvpx
//! `vp9_detokenize.c`, ported verbatim including its context bookkeeping).

use crate::bool::BoolDecoder;
use crate::tables::*;

/// One plane's entropy contexts: a has-coefficient flag per 4x4 column
/// above the current superblock row and per 4x4 row to the left.
pub(crate) struct PlaneContexts {
    pub above: Vec<u8>,
    pub left: [u8; 32],
}

impl PlaneContexts {
    pub(crate) fn new(above_len: usize) -> Self {
        Self {
            above: vec![0; above_len],
            left: [0; 32],
        }
    }
}

/// `get_coef_context` (vp9_scan.h).
#[inline]
fn get_coef_context(nb: &[i16], token_cache: &[u8; 1024], c: usize) -> usize {
    ((1 + token_cache[nb[2 * c] as usize] + token_cache[nb[2 * c + 1] as usize]) >> 1) as usize
}

#[inline]
fn band_at(tx_size: usize, c: usize) -> usize {
    if tx_size == TX_4X4 {
        COEFBAND_TRANS_4X4[c] as usize
    } else {
        COEFBAND_TRANS_8X8PLUS[c] as usize
    }
}

#[inline]
fn read_coeff(r: &mut BoolDecoder, probs: &[u8], n: usize) -> i32 {
    let mut val = 0i32;
    for i in 0..n {
        val = (val << 1) + r.read_bool(probs[i]) as i32;
    }
    val
}

/// One decoded transform block: dequantized coefficients + eob.
pub(crate) struct CoeffBlock {
    pub coeffs: Box<[i32]>,
    pub eob: usize,
}

/// `decode_coefs`, verbatim port. `dq` is `[dc, ac]`; plane 0 is luma,
/// chroma is 1; the ref axis is fixed at 0 (intra).
pub(crate) fn decode_coefs(
    r: &mut BoolDecoder,
    plane_type: usize,
    tx_size: usize,
    dequant: &[i16; 2],
    mut ctx: usize,
    scan: &[i16],
    nb: &[i16],
    probs4d: &[[[[[u8; 3]; 6]; 6]; 2]; 2],
    #[allow(unused_variables)] pos: (usize, usize),
    #[allow(unused_variables)] plane: usize,
) -> CoeffBlock {
    let max_eob = 16 << (tx_size << 1);
    let mut coef = CoeffBlock {
        coeffs: vec![0i32; max_eob].into_boxed_slice(),
        eob: 0,
    };
    let mut token_cache = [0u8; 1024];
    let mut c = 0usize;
    let dq_shift = usize::from(tx_size == TX_32X32);
    let mut dqv = dequant[0] as i32;
    let mut band = band_at(tx_size, c);
    let mut prob: &[u8; 3] = &probs4d[plane_type][0][band][ctx];
    if std::env::var_os("EC_VP9_DBG").is_some() && plane_type == 0 && ctx == 0 && band == 0 {
        eprintln!("DBG coefs tx={tx_size} dq={dequant:?} ctx={ctx} band={band} prob={prob:?}");
    }

    while c < max_eob {
        if !r.read_bool(prob[0]) {
            break;
        }
        while !r.read_bool(prob[1]) {
            dqv = dequant[1] as i32;
            token_cache[scan[c] as usize] = 0;
            c += 1;
            if c >= max_eob {
                return coef; // zero tokens to the end (no eob token)
            }
            ctx = get_coef_context(nb, &token_cache, c);
            band = band_at(tx_size, c);
            prob = &probs4d[plane_type][0][band][ctx];
        }
        let mut v: i32;
        if r.read_bool(prob[2]) {
            let base = prob[2] as usize - 1;
            let p = &PARETO8_FULL[base * 8..base * 8 + 8];
            if r.read_bool(p[0]) {
                if r.read_bool(p[3]) {
                    token_cache[scan[c] as usize] = 5;
                    if r.read_bool(p[5]) {
                        if r.read_bool(p[7]) {
                            v = 67 + read_coeff(r, &CAT6_PROB, 14);
                        } else {
                            v = 35 + read_coeff(r, &CAT5_PROB, 5);
                        }
                    } else if r.read_bool(p[6]) {
                        v = 19 + read_coeff(r, &CAT4_PROB, 4);
                    } else {
                        v = 11 + read_coeff(r, &CAT3_PROB, 3);
                    }
                } else {
                    token_cache[scan[c] as usize] = 4;
                    if r.read_bool(p[4]) {
                        v = 7 + read_coeff(r, &CAT2_PROB, 2);
                    } else {
                        v = 5 + read_coeff(r, &CAT1_PROB, 1);
                    }
                }
                v = (v * dqv) >> dq_shift;
            } else if r.read_bool(p[1]) {
                token_cache[scan[c] as usize] = 3;
                v = ((3 + r.read_bool(p[2]) as i32) * dqv) >> dq_shift;
            } else {
                token_cache[scan[c] as usize] = 2;
                v = (2 * dqv) >> dq_shift;
            }
        } else {
            token_cache[scan[c] as usize] = 1;
            v = dqv >> dq_shift;
        }
        let sign = r.read_bool(128);
        let pos = scan[c] as usize;
        coef.coeffs[pos] = if sign { -v } else { v };
        c += 1;
        if c < max_eob {
            ctx = get_coef_context(nb, &token_cache, c);
            band = band_at(tx_size, c);
            prob = &probs4d[plane_type][0][band][ctx];
        }
        dqv = dequant[1] as i32;
    }
    coef.eob = c;
    if crate::trace_enabled() {
        eprintln!(
            "C p={} x={} y={} tx={} eob={} c0={}",
            plane, pos.0, pos.1, tx_size, c, coef.coeffs[0]
        );
    }
    coef
}

/// Scan + neighbors per tx size and tx type (vp9_scan_orders).
pub(crate) fn scan_for(tx_size: usize, tx_type: usize) -> (&'static [i16], &'static [i16]) {
    match (tx_size, tx_type) {
        (TX_4X4, DCT_DCT) => (&DEFAULT_SCAN_4X4, &DEFAULT_SCAN_4X4_NEIGHBORS),
        (TX_4X4, ADST_DCT) => (&ROW_SCAN_4X4, &ROW_SCAN_4X4_NEIGHBORS),
        (TX_4X4, DCT_ADST) => (&COL_SCAN_4X4, &COL_SCAN_4X4_NEIGHBORS),
        (TX_4X4, _) => (&DEFAULT_SCAN_4X4, &DEFAULT_SCAN_4X4_NEIGHBORS),
        (TX_8X8, DCT_DCT) => (&DEFAULT_SCAN_8X8, &DEFAULT_SCAN_8X8_NEIGHBORS),
        (TX_8X8, ADST_DCT) => (&ROW_SCAN_8X8, &ROW_SCAN_8X8_NEIGHBORS),
        (TX_8X8, DCT_ADST) => (&COL_SCAN_8X8, &COL_SCAN_8X8_NEIGHBORS),
        (TX_8X8, _) => (&DEFAULT_SCAN_8X8, &DEFAULT_SCAN_8X8_NEIGHBORS),
        (TX_16X16, DCT_DCT) => (&DEFAULT_SCAN_16X16, &DEFAULT_SCAN_16X16_NEIGHBORS),
        (TX_16X16, ADST_DCT) => (&ROW_SCAN_16X16, &ROW_SCAN_16X16_NEIGHBORS),
        (TX_16X16, DCT_ADST) => (&COL_SCAN_16X16, &COL_SCAN_16X16_NEIGHBORS),
        (TX_16X16, _) => (&DEFAULT_SCAN_16X16, &DEFAULT_SCAN_16X16_NEIGHBORS),
        (_, _) => (&DEFAULT_SCAN_32X32, &DEFAULT_SCAN_32X32_NEIGHBORS),
    }
}
