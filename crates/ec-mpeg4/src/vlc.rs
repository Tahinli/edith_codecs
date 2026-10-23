//! Prefix-code lookup tables built once from the (code, length) tables.

use std::sync::OnceLock;

use crate::bits::Br;
use crate::tables::*;

/// Single-level lookup of `bits` bits: entry = `len | sym << 8`, 0 = invalid.
pub struct Vlc {
    bits: u32,
    tab: Vec<u32>,
}

impl Vlc {
    /// `codes`: (code, len, symbol).
    fn build(codes: &[(u32, u32, u32)]) -> Vlc {
        let bits = codes.iter().map(|c| c.1).max().unwrap_or(1);
        let mut tab = vec![0u32; 1 << bits];
        for &(code, len, sym) in codes {
            if len == 0 {
                continue;
            }
            let lo = (code << (bits - len)) as usize;
            let hi = lo + (1usize << (bits - len));
            for t in &mut tab[lo..hi] {
                *t = len | (sym << 8);
            }
        }
        Vlc { bits, tab }
    }

    fn from_pairs(t: &[[u8; 2]]) -> Vlc {
        let c: Vec<_> = t
            .iter()
            .enumerate()
            .map(|(i, p)| (p[0] as u32, p[1] as u32, i as u32))
            .collect();
        Vlc::build(&c)
    }

    fn from_pairs16(t: &[[u16; 2]]) -> Vlc {
        let c: Vec<_> = t
            .iter()
            .enumerate()
            .map(|(i, p)| (p[0] as u32, p[1] as u32, i as u32))
            .collect();
        Vlc::build(&c)
    }

    /// Decode a symbol; `None` on an invalid code (nothing consumed).
    #[inline]
    pub fn get(&self, br: &mut Br) -> Option<u32> {
        let e = self.tab[br.peek(self.bits) as usize];
        if e == 0 {
            return None;
        }
        br.skip(e & 0xff);
        Some(e >> 8)
    }
}

/// Run/level table. Symbols index `run`/`level`; `n` is the escape index.
pub struct Rl {
    pub vlc: Vlc,
    pub n: usize,
    pub last: usize,
    pub run: Vec<i8>,
    pub level: Vec<i8>,
    /// `max_level[last][run]`
    pub max_level: [[u8; 64]; 2],
    /// `max_run[last][level]`
    pub max_run: [[u8; 64]; 2],
}

impl Rl {
    pub fn new(vlc: &[[u16; 2]], run: &[i8], level: &[i8], n: usize, last: usize) -> Rl {
        let mut max_level = [[0u8; 64]; 2];
        let mut max_run = [[0u8; 64]; 2];
        for i in 0..n {
            let l = (i >= last) as usize;
            let (r, lv) = (run[i] as usize, level[i] as usize);
            if lv as u8 > max_level[l][r] {
                max_level[l][r] = lv as u8;
            }
            if r as u8 > max_run[l][lv] {
                max_run[l][lv] = r as u8;
            }
        }
        Rl {
            vlc: Vlc::from_pairs16(vlc),
            n,
            last,
            run: run.to_vec(),
            level: level.to_vec(),
            max_level,
            max_run,
        }
    }
}

pub struct Vlcs {
    pub dc_lum: Vlc,
    pub dc_chrom: Vlc,
    pub intra_mcbpc: Vlc,
    pub inter_mcbpc: Vlc,
    pub cbpy: Vlc,
    pub mv: Vlc,
    pub mb_type_b: Vlc,
    pub rl_intra: Rl,
    pub rl_inter: Rl,
    #[allow(dead_code)] // kept with the RVLC tables for data partitioning
    pub rl_intra_aic: Rl,
    #[allow(dead_code)]
    pub rvlc_intra: Rl,
    #[allow(dead_code)]
    pub rvlc_inter: Rl,
    pub sprite_traj: Vlc,
}

pub fn vlcs() -> &'static Vlcs {
    static V: OnceLock<Vlcs> = OnceLock::new();
    V.get_or_init(|| {
        let mcbpc = |code: &[u8], bits: &[u8]| {
            let c: Vec<_> = code
                .iter()
                .zip(bits)
                .enumerate()
                .map(|(i, (&c, &b))| (c as u32, b as u32, i as u32))
                .collect();
            Vlc::build(&c)
        };
        Vlcs {
            dc_lum: Vlc::from_pairs(&MPEG4_DCTAB_LUM),
            dc_chrom: Vlc::from_pairs(&MPEG4_DCTAB_CHROM),
            intra_mcbpc: mcbpc(&H263_INTRA_MCBPC_CODE, &H263_INTRA_MCBPC_BITS),
            inter_mcbpc: mcbpc(&H263_INTER_MCBPC_CODE, &H263_INTER_MCBPC_BITS),
            cbpy: Vlc::from_pairs(&H263_CBPY_TAB),
            mv: Vlc::from_pairs(&MVTAB),
            mb_type_b: Vlc::from_pairs(&MB_TYPE_B_TAB),
            rl_intra: Rl::new(
                &MPEG4_INTRA_VLC,
                &MPEG4_INTRA_RUN,
                &MPEG4_INTRA_LEVEL,
                MPEG4_RL_INTRA_N,
                MPEG4_RL_INTRA_LAST,
            ),
            rl_inter: Rl::new(
                &INTER_VLC,
                &INTER_RUN,
                &INTER_LEVEL,
                H263_RL_INTER_N,
                H263_RL_INTER_LAST,
            ),
            rl_intra_aic: Rl::new(
                &INTRA_VLC_AIC,
                &INTRA_RUN_AIC,
                &INTRA_LEVEL_AIC,
                RL_INTRA_AIC_N,
                RL_INTRA_AIC_LAST,
            ),
            rvlc_intra: Rl::new(
                &INTRA_RVLC,
                &INTRA_RVLC_RUN,
                &INTRA_RVLC_LEVEL,
                RVLC_RL_INTRA_N,
                RVLC_RL_INTRA_LAST,
            ),
            rvlc_inter: Rl::new(
                &INTER_RVLC,
                &INTER_RVLC_RUN,
                &INTER_RVLC_LEVEL,
                RVLC_RL_INTER_N,
                RVLC_RL_INTER_LAST,
            ),
            sprite_traj: {
                let mut c = Vec::new();
                let mut code = 0u32;
                for (i, &l) in SPRITE_TRAJECTORY_LENS.iter().enumerate() {
                    c.push((code, l as u32, i as u32));
                    if let Some(&nl) = SPRITE_TRAJECTORY_LENS.get(i + 1) {
                        code = (code + 1) << (nl - l);
                    }
                }
                Vlc::build(&c)
            },
        }
    })
}
