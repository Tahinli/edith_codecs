//! Microsoft MPEG-4 (DivX 3 / MS-MPEG4 versions 2 and 3) picture and macroblock decoding.
//!
//! Reuses the H.263/MPEG-4 machinery (motion prediction, AC prediction, motion compensation,
//! IDCT, picture buffers) and adds the MS-specific headers, VLC tables, DC and coded-block
//! prediction and run/level escapes. Follows libavcodec's `msmpeg4dec.c`.

use std::sync::OnceLock;
use std::sync::Arc;

use ec_core::error::{Error, Result};
use ec_core::timebase::Timestamp;

use crate::bits::Br;
use crate::core::*;
use crate::header::Pt;
use crate::msdata::*;
use crate::tables::*;
use crate::vlc::{vlcs, Rl};

fn corrupt(s: &'static str) -> Error {
    Error::corrupt(s)
}

/// Prefix-code decoder for codes up to 32 bits: a first-level table plus buckets for longer codes.
pub struct Vlc2 {
    maxlen: u32,
    first: u32,
    tab: Vec<u32>,
    buckets: Vec<Vec<(u32, u32, u32)>>,
}

impl Vlc2 {
    /// `codes`: (code, len, symbol); codes with len 0 are skipped.
    pub fn build(codes: &[(u32, u32, u32)]) -> Vlc2 {
        let maxlen = codes.iter().map(|c| c.1).max().unwrap_or(1);
        let first = maxlen.min(10);
        let mut tab = vec![0u32; 1 << first];
        let mut buckets: Vec<Vec<(u32, u32, u32)>> = Vec::new();
        for &(code, len, sym) in codes {
            if len == 0 {
                continue;
            }
            if len <= first {
                let lo = (code << (first - len)) as usize;
                let hi = lo + (1usize << (first - len));
                for t in &mut tab[lo..hi] {
                    *t = len | (sym << 8);
                }
            } else {
                let pre = (code >> (len - first)) as usize;
                if tab[pre] & 0x8000_0000 == 0 {
                    buckets.push(Vec::new());
                    tab[pre] = 0x8000_0000 | (buckets.len() as u32 - 1);
                }
                let b = (tab[pre] & 0x7fff_ffff) as usize;
                buckets[b].push((code, len, sym));
            }
        }
        Vlc2 {
            maxlen,
            first,
            tab,
            buckets,
        }
    }

    pub fn from_pairs<T: Copy + Into<u32>>(t: &[[T; 2]]) -> Vlc2 {
        let c: Vec<_> = t
            .iter()
            .enumerate()
            .map(|(i, p)| (p[0].into(), p[1].into(), i as u32))
            .collect();
        Vlc2::build(&c)
    }

    pub fn get(&self, br: &mut Br) -> Option<u32> {
        let p = br.peek(self.maxlen);
        let e = self.tab[(p >> (self.maxlen - self.first)) as usize];
        if e == 0 {
            return None;
        }
        if e & 0x8000_0000 == 0 {
            br.skip(e & 0xff);
            return Some(e >> 8);
        }
        for &(code, len, sym) in &self.buckets[(e & 0x7fff_ffff) as usize] {
            if p >> (self.maxlen - len) == code {
                br.skip(len);
                return Some(sym);
            }
        }
        None
    }
}

pub struct MsVlcs {
    rl: [&'static Rl; 6],
    mb_non_intra: Vlc2,
    mb_i: Vlc2,
    dc: [[Vlc2; 2]; 2],
    mv: [Vlc2; 2],
    v2_mb_type: Vlc2,
    v2_intra_cbpc: Vlc2,
    v2_dc_lum: Vlc2,
    v2_dc_chroma: Vlc2,
}

struct RlOwned {
    t: [Rl; 4],
}

fn rl_owned() -> &'static RlOwned {
    static R: OnceLock<RlOwned> = OnceLock::new();
    R.get_or_init(|| RlOwned {
        t: [
            Rl::new(&RL_T0_VLC, &RL_T0_RUN, &RL_T0_LEVEL, 132, 85),
            Rl::new(&RL_T2_VLC, &RL_T2_RUN, &RL_T2_LEVEL, 185, 119),
            Rl::new(&RL_T1_VLC, &RL_T1_RUN, &RL_T1_LEVEL, 148, 81),
            Rl::new(&RL_T4_VLC, &RL_T4_RUN, &RL_T4_LEVEL, 168, 99),
        ],
    })
}

/// `ff_vlc_init_from_lengths`: codes are assigned in table order.
fn from_lengths(lens: &[u8], syms: &[u16]) -> Vlc2 {
    let mut code: u64 = 0;
    let mut c = Vec::new();
    for (i, &l) in lens.iter().enumerate() {
        if l == 0 {
            continue;
        }
        c.push(((code >> (32 - l as u32)) as u32, l as u32, syms[i] as u32));
        code += 1u64 << (32 - l as u32);
    }
    Vlc2::build(&c)
}

/// `init_h263_dc_for_msmpeg4`: the version 1/2 DC code for `level` (-256..256).
fn v2_dc_codes(tab: &[[u8; 2]; 13]) -> Vlc2 {
    let mut c = Vec::new();
    for level in -256i32..256 {
        let mut size = 0usize;
        let mut v = level.unsigned_abs();
        while v != 0 {
            v >>= 1;
            size += 1;
        }
        let l = if level < 0 {
            (-level) ^ ((1 << size) - 1)
        } else {
            level
        };
        let mut len = tab[size][1] as u32;
        let mut code = tab[size][0] as u32;
        code ^= (1 << len) - 1;
        if size > 0 {
            code = (code << size) | l as u32;
            len += size as u32;
            if size > 8 {
                code = (code << 1) | 1;
                len += 1;
            }
        }
        c.push((code, len, (level + 256) as u32));
    }
    Vlc2::build(&c)
}

pub fn msvlcs() -> &'static MsVlcs {
    static V: OnceLock<MsVlcs> = OnceLock::new();
    V.get_or_init(|| {
        let o = rl_owned();
        let v = vlcs();
        MsVlcs {
            rl: [&o.t[0], &o.t[1], &v.rl_intra, &o.t[2], &o.t[3], &v.rl_inter],
            mb_non_intra: Vlc2::from_pairs(&MB_NON_INTRA),
            mb_i: Vlc2::from_pairs(&MB_I),
            dc: [
                [Vlc2::from_pairs(&DC_TABLE_0_0), Vlc2::from_pairs(&DC_TABLE_0_1)],
                [Vlc2::from_pairs(&DC_TABLE_1_0), Vlc2::from_pairs(&DC_TABLE_1_1)],
            ],
            mv: [
                from_lengths(&MV_TABLE0_LENS, &MV_TABLE0),
                from_lengths(&MV_TABLE1_LENS, &MV_TABLE1),
            ],
            v2_mb_type: Vlc2::from_pairs(&V2_MB_TYPE),
            v2_intra_cbpc: Vlc2::from_pairs(&V2_INTRA_CBPC),
            v2_dc_lum: v2_dc_codes(&MPEG4_DCTAB_LUM),
            v2_dc_chroma: v2_dc_codes(&MPEG4_DCTAB_CHROM),
        }
    })
}

/// Per-stream MS-MPEG4 state.
#[derive(Default)]
pub struct MsState {
    pub slice_height: usize,
    pub rl_table_index: usize,
    pub rl_chroma_table_index: usize,
    pub dc_table_index: usize,
    pub mv_table_index: usize,
    pub use_skip_mb_code: bool,
    pub flipflop_rounding: bool,
    /// Coded-block flags per luma 8x8 block (same layout as `Core::dc[0]`).
    pub coded: Vec<u8>,
}

fn decode012(br: &mut Br) -> usize {
    if br.bit() == 0 {
        0
    } else {
        br.bit() as usize + 1
    }
}

/// `FASTDIV(a, b)` for the DC scales used here: floor(a * ceil(2^32/b) / 2^32).
#[inline]
fn fastdiv(a: i32, b: i32) -> i32 {
    let inv = (1i64 << 32).div_euclid(b as i64) + if (1i64 << 32) % b as i64 != 0 { 1 } else { 0 };
    ((a as i64 * inv) >> 32) as i32
}

impl Core {
    /// Decode one MS-MPEG4 picture (one packet).
    pub fn decode_msmpeg4_packet(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        if self.mb_num == 0 {
            return Err(Error::unsupported(
                "MS-MPEG4",
                "picture size unknown (needs width and height from the container)".to_string(),
            ));
        }
        let mut br = Br::new(data);
        let ver = self.ms_version;
        // At least one bit per macroblock.
        if (br.left() as i64) * 8 < self.mb_num as i64 {
            return Err(corrupt("MS-MPEG4: packet too small"));
        }
        let pt = match br.get(2) + 1 {
            1 => Pt::I,
            2 => Pt::P,
            _ => return Err(corrupt("MS-MPEG4: invalid picture type")),
        };
        let q = br.get(5) as i32;
        if q == 0 {
            return Err(corrupt("MS-MPEG4: invalid qscale"));
        }
        if pt == Pt::I {
            let code = br.get(5) as usize;
            if code < 0x17 {
                return Err(corrupt("MS-MPEG4: bad slice code"));
            }
            self.ms.slice_height = self.mb_h / (code - 0x16);
            if ver == 3 {
                self.ms.rl_chroma_table_index = decode012(&mut br);
                self.ms.rl_table_index = decode012(&mut br);
                self.ms.dc_table_index = br.bit() as usize;
            } else {
                self.ms.rl_chroma_table_index = 2;
                self.ms.rl_table_index = 2;
                self.ms.dc_table_index = 0;
            }
            self.no_rounding = true;
        } else {
            if ver == 3 {
                self.ms.use_skip_mb_code = br.bit() != 0;
                self.ms.rl_table_index = decode012(&mut br);
                self.ms.rl_chroma_table_index = self.ms.rl_table_index;
                self.ms.dc_table_index = br.bit() as usize;
                self.ms.mv_table_index = br.bit() as usize;
            } else {
                self.ms.use_skip_mb_code = br.bit() != 0;
                self.ms.rl_table_index = 2;
                self.ms.rl_chroma_table_index = 2;
                self.ms.dc_table_index = 0;
                self.ms.mv_table_index = 0;
            }
            if self.ms.flipflop_rounding {
                self.no_rounding = !self.no_rounding;
            } else {
                self.no_rounding = false;
            }
        }
        self.pt = pt;
        self.set_qscale(q);
        self.picture_number += 1;

        // frame start
        let mut cur = Pic::new(self.mb_w, self.mb_h);
        cur.pts = pts;
        cur.pict = Some(pt);
        cur.keyframe = pt == Pt::I;
        self.last = self.next.take();
        if self.last.is_none() && pt != Pt::I {
            self.last = Some(Arc::new(Pic::gray(self.mb_w, self.mb_h)));
            self.dummy_last = true;
        }
        self.cur = cur;
        self.reset_pred_tables();
        self.done.iter_mut().for_each(|d| *d = false);
        self.damaged = false;

        let mut w = Box::new(MbWork::new());
        self.mb_x = 0;
        self.mb_y = 0;
        let mut res = self.ms_slice(&mut br, &mut w);
        while self.mb_y < self.mb_h {
            let sh = self.ms.slice_height;
            if sh == 0 || self.mb_x != 0 || res.is_err() || !self.mb_y.is_multiple_of(sh) || br.left() < 0 {
                break;
            }
            self.clean_buffers();
            if self.ms_slice(&mut br, &mut w).is_err() {
                res = Err(corrupt("MS-MPEG4 slice"));
            }
        }
        if res.is_err() {
            self.damaged = true;
        }
        if pt == Pt::I {
            self.ms_ext_header(&mut br, data.len());
        }
        self.conceal();
        self.frame_end(pt);
        Ok(())
    }

    /// `ff_msmpeg4_decode_ext_header` (versions below WMV1).
    fn ms_ext_header(&mut self, br: &mut Br, buf_size: usize) {
        let left = (buf_size * 8) as isize - br.pos() as isize;
        let length: isize = if self.ms_version >= 3 { 17 } else { 16 };
        if left >= length && left < length + 8 {
            br.skip(5);
            br.skip(11);
            self.ms.flipflop_rounding = if self.ms_version >= 3 {
                br.bit() != 0
            } else {
                false
            };
        } else if left < length + 8 {
            self.ms.flipflop_rounding = false;
        }
    }

    fn ms_slice(&mut self, br: &mut Br, w: &mut MbWork) -> Result<()> {
        self.last_resync_pos = br.pos();
        self.first_slice_line = true;
        self.resync_mb_x = self.mb_x;
        self.resync_mb_y = self.mb_y;
        let q = self.qscale;
        self.set_qscale(q);
        while self.mb_y < self.mb_h {
            if self.resync_mb_y + self.ms.slice_height == self.mb_y {
                return Ok(());
            }
            while self.mb_x < self.mb_w {
                if self.resync_mb_x == self.mb_x && self.resync_mb_y + 1 == self.mb_y {
                    self.first_slice_line = false;
                }
                if let Err(e) = self.ms_decode_mb(br, w) {
                    for b in w.blocks.iter_mut() {
                        *b = [0; 64];
                    }
                    return Err(e);
                }
                self.post_mb(w);
                self.reconstruct(w);
                let xy = self.mb_y * self.mb_w + self.mb_x;
                self.done[xy] = true;
                self.mb_x += 1;
            }
            self.mb_x = 0;
            self.mb_y += 1;
        }
        Ok(())
    }

    fn ms_decode_mb(&mut self, br: &mut Br, w: &mut MbWork) -> Result<()> {
        let v = msvlcs();
        let xy = self.mb_x + self.mb_y * self.mb_w;
        w.intra = false;
        w.mcsel = false;
        w.skipped = false;
        w.ac_pred = false;
        w.interlaced_dct = false;
        w.mvt = Mvt::M16;
        w.mv_dir = 1;
        w.last_index = [-1; 6];
        self.cur.mbskip[xy] = 0;
        self.qscale_table[xy] = self.qscale as i8;
        let ver = self.ms_version;
        if br.left() <= 0 {
            return Err(corrupt("MS-MPEG4: out of data"));
        }
        let mut cbp: u32;
        if self.pt == Pt::P {
            if self.ms.use_skip_mb_code && br.bit() != 0 {
                self.cur.mbskip[xy] = 1;
                self.cur.mb_kind[xy] = 0;
                w.skipped = true;
                w.mv[0][0] = [0, 0];
                return Ok(());
            }
            if ver == 3 {
                let code = v
                    .mb_non_intra
                    .get(br)
                    .ok_or_else(|| corrupt("MS-MPEG4 mb_non_intra"))?;
                w.intra = code & 0x40 == 0;
                cbp = code & 0x3f;
            } else {
                let code = v
                    .v2_mb_type
                    .get(br)
                    .ok_or_else(|| corrupt("MS-MPEG4 mb type"))?;
                w.intra = code >> 2 != 0;
                cbp = code & 3;
            }
        } else {
            w.intra = true;
            if ver == 3 {
                let code = v.mb_i.get(br).ok_or_else(|| corrupt("MS-MPEG4 mb_i"))?;
                cbp = 0;
                for i in 0..6 {
                    let mut val = (code >> (5 - i)) & 1;
                    if i < 4 {
                        let (idx, wrap) = self.dc_pos(i);
                        let c = &self.ms.coded;
                        let (a, b, cc) = (c[idx - 1], c[idx - 1 - wrap], c[idx - wrap]);
                        let pred = if b == cc { a } else { cc } as u32;
                        val ^= pred;
                        self.ms.coded[idx] = val as u8;
                    }
                    cbp |= val << (5 - i);
                }
            } else {
                cbp = v
                    .v2_intra_cbpc
                    .get(br)
                    .ok_or_else(|| corrupt("MS-MPEG4 intra cbpc"))?;
            }
        }
        if !w.intra {
            if ver == 3 {
                // cbp already complete
            } else {
                let cbpy = vlcs()
                    .cbpy
                    .get(br)
                    .ok_or_else(|| corrupt("MS-MPEG4 cbpy"))?;
                cbp |= cbpy << 2;
                if cbp & 3 != 3 {
                    cbp ^= 0x3c;
                }
            }
            let (px, py) = self.pred_motion(0);
            let (mx, my) = if ver == 3 {
                let sym = v.mv[self.ms.mv_table_index]
                    .get(br)
                    .ok_or_else(|| corrupt("MS-MPEG4 mv"))?;
                let (mut mx, mut my) = if sym != 0 {
                    ((sym >> 8) as i32, (sym & 0xff) as i32)
                } else {
                    (br.get(6) as i32, br.get(6) as i32)
                };
                mx += px - 32;
                my += py - 32;
                let wrap = |m: i32| {
                    if m <= -64 {
                        m + 64
                    } else if m >= 64 {
                        m - 64
                    } else {
                        m
                    }
                };
                (wrap(mx), wrap(my))
            } else {
                (self.ms2_motion(br, px)?, self.ms2_motion(br, py)?)
            };
            w.mv[0][0] = [mx, my];
            self.cur.mb_kind[xy] = 0;
        } else {
            self.cur.mb_kind[xy] = K_INTRA;
            if ver == 3 {
                w.ac_pred = br.bit() != 0;
            } else {
                w.ac_pred = br.bit() != 0;
                let cbpy = vlcs()
                    .cbpy
                    .get(br)
                    .ok_or_else(|| corrupt("MS-MPEG4 cbpy"))?;
                cbp |= cbpy << 2;
            }
        }
        for i in 0..6 {
            let li = self.ms_block(br, w, i, (cbp >> (5 - i)) & 1 != 0)?;
            w.last_index[i] = li;
        }
        Ok(())
    }

    /// `msmpeg4v2_decode_motion`.
    fn ms2_motion(&self, br: &mut Br, pred: i32) -> Result<i32> {
        let code = vlcs()
            .mv
            .get(br)
            .ok_or_else(|| corrupt("MS-MPEG4 v2 mv"))? as i32;
        if code == 0 {
            return Ok(pred);
        }
        let sign = br.bit() != 0;
        let mut val = if sign { -code } else { code };
        val += pred;
        if val <= -64 {
            val += 64;
        } else if val >= 64 {
            val -= 64;
        }
        Ok(val)
    }

    /// `msmpeg4_decode_dc` plus `ff_msmpeg4_pred_dc`. Returns (level, direction).
    fn ms_dc(&mut self, br: &mut Br, n: usize) -> Result<(i32, i32)> {
        let v = msvlcs();
        let mut level: i32;
        if self.ms_version <= 2 {
            let t = if n < 4 { &v.v2_dc_lum } else { &v.v2_dc_chroma };
            level = t.get(br).ok_or_else(|| corrupt("MS-MPEG4 illegal dc vlc"))? as i32 - 256;
        } else {
            level = v.dc[self.ms.dc_table_index][(n >= 4) as usize]
                .get(br)
                .ok_or_else(|| corrupt("MS-MPEG4 illegal dc vlc"))? as i32;
            if level == 119 {
                level = br.get(8) as i32;
                if br.bit() != 0 {
                    level = -level;
                }
            } else if level != 0 && br.bit() != 0 {
                level = -level;
            }
        }
        let scale = if n < 4 { self.y_dc_scale } else { self.c_dc_scale };
        let (idx, wrap) = self.dc_pos(n);
        let p = if n < 4 { 0 } else { n - 3 };
        let dc = &self.dc[p];
        let mut a = dc[idx - 1] as i32;
        let mut b = dc[idx - 1 - wrap] as i32;
        let mut c = dc[idx - wrap] as i32;
        if self.first_slice_line && n & 2 == 0 {
            b = 1024;
            c = 1024;
        }
        a = fastdiv(a + (scale >> 1), scale);
        b = fastdiv(b + (scale >> 1), scale);
        c = fastdiv(c + (scale >> 1), scale);
        let (pred, dir) = if (a - b).abs() <= (b - c).abs() {
            (c, 1)
        } else {
            (a, 0)
        };
        level += pred;
        self.dc[p][idx] = (level * scale) as i16;
        Ok((level, dir))
    }

    /// `ff_msmpeg4_decode_block`. Returns the block's last index.
    fn ms_block(&mut self, br: &mut Br, w: &mut MbWork, n: usize, coded: bool) -> Result<i32> {
        let v = msvlcs();
        let ver = self.ms_version;
        let intra = w.intra;
        let rl: &Rl;
        let (qmul, qadd): (i32, i32);
        let run_diff: i32;
        let mut i: i32;
        let mut scan: &[u8; 64] = &ZIGZAG_DIRECT;
        let mut dir = 0;
        if intra {
            qmul = 1;
            qadd = 0;
            let (level, d) = self.ms_dc(br, n)?;
            dir = d;
            let (rli, scale) = if n < 4 {
                (self.ms.rl_table_index, self.y_dc_scale)
            } else {
                (3 + self.ms.rl_chroma_table_index, self.c_dc_scale)
            };
            if level > 256 * scale {
                return Err(corrupt("MS-MPEG4 dc overflow"));
            }
            rl = v.rl[rli];
            w.blocks[n][0] = level as i16;
            run_diff = 0;
            i = 0;
            if !coded {
                let blk = &mut w.blocks[n];
                self.pred_ac(blk, n, dir, w.ac_pred);
                return Ok(0);
            }
            if w.ac_pred {
                scan = if dir == 0 {
                    &ALTERNATE_VERTICAL_SCAN
                } else {
                    &ALTERNATE_HORIZONTAL_SCAN
                };
            }
        } else {
            qmul = self.qscale << 1;
            qadd = (self.qscale - 1) | 1;
            i = -1;
            rl = v.rl[3 + self.ms.rl_table_index];
            run_diff = if ver == 2 { 0 } else { 1 };
            if !coded {
                return Ok(-1);
            }
        }
        // (run, level) of a table symbol with ffmpeg's biased encoding: run + 1, +192 when last.
        let sym_rl = |s: usize| -> (i32, i32) {
            let mut run = rl.run[s] as i32 + 1;
            if s >= rl.last {
                run += 192;
            }
            (run, rl.level[s] as i32 * qmul + qadd)
        };
        loop {
            let sym = rl
                .vlc
                .get(br)
                .ok_or_else(|| corrupt("MS-MPEG4 illegal ac vlc"))? as usize;
            let mut level: i32;
            if sym == rl.n {
                if br.bit() != 0 {
                    // first escape
                    let s2 = rl
                        .vlc
                        .get(br)
                        .ok_or_else(|| corrupt("MS-MPEG4 illegal ac vlc"))?
                        as usize;
                    if s2 == rl.n {
                        return Err(corrupt("MS-MPEG4 bad escape"));
                    }
                    let (run, lv) = sym_rl(s2);
                    i += run;
                    level = lv + rl.max_level[(run >> 7) as usize][((run - 1) & 63) as usize] as i32 * qmul;
                    if br.bit() != 0 {
                        level = -level;
                    }
                } else if br.bit() == 0 {
                    // third escape
                    let last = br.bit() as i32;
                    let run = br.get(6) as i32;
                    level = (br.get(8) as u8 as i8) as i32;
                    if level > 0 {
                        level = level * qmul + qadd;
                    } else {
                        level = level * qmul - qadd;
                    }
                    i += run + 1;
                    if last != 0 {
                        i += 192;
                    }
                } else {
                    // second escape
                    let s2 = rl
                        .vlc
                        .get(br)
                        .ok_or_else(|| corrupt("MS-MPEG4 illegal ac vlc"))?
                        as usize;
                    if s2 == rl.n {
                        return Err(corrupt("MS-MPEG4 bad escape"));
                    }
                    let (run, lv) = sym_rl(s2);
                    let l0 = rl.level[s2] as usize;
                    i += run + rl.max_run[(run >> 7) as usize][l0] as i32 + run_diff;
                    level = lv;
                    if br.bit() != 0 {
                        level = -level;
                    }
                }
            } else {
                let (run, lv) = sym_rl(sym);
                i += run;
                level = lv;
                if br.bit() != 0 {
                    level = -level;
                }
            }
            if i > 62 {
                i -= 192;
                if i & !63 != 0 {
                    if br.left() >= 0 {
                        i = 63;
                        break;
                    }
                    return Err(corrupt("MS-MPEG4 ac-tex damaged"));
                }
                w.blocks[n][scan[i as usize] as usize] = level as i16;
                break;
            }
            w.blocks[n][scan[i as usize] as usize] = level as i16;
        }
        if intra {
            let blk = &mut w.blocks[n];
            self.pred_ac(blk, n, dir, w.ac_pred);
        }
        Ok(i)
    }
}
