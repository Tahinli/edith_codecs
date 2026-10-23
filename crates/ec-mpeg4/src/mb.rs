//! Macroblock-layer parsing for MPEG-4 Part 2 (non data-partitioned).

use ec_core::error::{Error, Result};

use crate::bits::Br;
use crate::core::*;
use crate::header::Pt;
use crate::tables::*;
use crate::vlc::vlcs;

pub enum MbEnd {
    Ok,
    SliceEnd,
}

fn corrupt(s: &'static str) -> Error {
    Error::corrupt(s)
}

fn mid_pred(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}

fn rounded_div(a: i32, b: i32) -> i32 {
    if a >= 0 {
        (a + (b >> 1)) / b
    } else {
        (a - (b >> 1)) / b
    }
}

fn rshift(a: i64, b: i32) -> i64 {
    if a > 0 {
        (a + ((1i64 << b) >> 1)) >> b
    } else {
        (a + ((1i64 << b) >> 1) - 1) >> b
    }
}

#[inline]
fn pt_num(p: Pt) -> i32 {
    match p {
        Pt::I => 1,
        Pt::P => 2,
        Pt::B => 3,
        Pt::S => 4,
    }
}

impl Core {
    #[inline]
    pub fn mvi(&self, bx: isize, by: isize) -> usize {
        (self.mvoff as isize + by * self.b8s as isize + bx) as usize
    }

    #[inline]
    fn mvget(&self, i: isize) -> [i32; 2] {
        let m = self.cur.mv[i as usize];
        [m[0] as i32, m[1] as i32]
    }

    /// `ff_h263_pred_motion` for direction 0. Returns the prediction.
    pub fn pred_motion(&mut self, block: usize) -> (i32, i32) {
        let b8s = self.b8s as isize;
        let bx = 2 * self.mb_x as isize + (block & 1) as isize;
        let by = 2 * self.mb_y as isize + (block >> 1) as isize;
        let m = self.mvi(bx, by) as isize;
        let off: [isize; 4] = [2, 1, 1, -1];
        let a_i = m - 1;
        let a = self.mvget(a_i);
        let c_i = m + off[block] - b8s;
        if self.first_slice_line && block < 3 {
            if block == 0 {
                if self.mb_x == self.resync_mb_x {
                    (0, 0)
                } else if self.mb_x + 1 == self.resync_mb_x && self.h263_pred() {
                    let c = self.mvget(c_i);
                    if self.mb_x == 0 {
                        (c[0], c[1])
                    } else {
                        (mid_pred(a[0], 0, c[0]), mid_pred(a[1], 0, c[1]))
                    }
                } else {
                    (a[0], a[1])
                }
            } else if block == 1 {
                if self.mb_x + 1 == self.resync_mb_x && self.h263_pred() {
                    let c = self.mvget(c_i);
                    (mid_pred(a[0], 0, c[0]), mid_pred(a[1], 0, c[1]))
                } else {
                    (a[0], a[1])
                }
            } else {
                let b = self.mvget(m - b8s);
                let c = self.mvget(c_i);
                let mut a = a;
                if self.mb_x == self.resync_mb_x {
                    a = [0, 0];
                    self.cur.mv[a_i as usize] = [0, 0];
                }
                (mid_pred(a[0], b[0], c[0]), mid_pred(a[1], b[1], c[1]))
            }
        } else {
            let b = self.mvget(m - b8s);
            let c = self.mvget(c_i);
            (mid_pred(a[0], b[0], c[0]), mid_pred(a[1], b[1], c[1]))
        }
    }

    /// `ff_h263_decode_motion`.
    pub fn decode_motion(&self, br: &mut Br, pred: i32, f_code: i32) -> Result<i32> {
        let code = vlcs()
            .mv
            .get(br)
            .ok_or_else(|| corrupt("MPEG-4 motion vector VLC"))? as i32;
        if code == 0 {
            return Ok(pred);
        }
        let sign = br.bit() != 0;
        let shift = (f_code - 1) as u32;
        let mut val = code;
        if shift != 0 {
            val = (val - 1) << shift;
            val |= br.get(shift) as i32;
            val += 1;
        }
        if sign {
            val = -val;
        }
        val += pred;
        if !self.h263_long_vectors {
            let n = 5 + f_code;
            val = (val << (32 - n)) >> (32 - n);
        } else {
            if pred < -31 && val < -63 {
                val += 64;
            }
            if pred > 32 && val > 63 {
                val -= 64;
            }
        }
        Ok(val)
    }

    fn pred_dc(&self, n: usize) -> (i32, i32) {
        let (idx, wrap) = self.dc_pos(n);
        let dc = &self.dc[if n < 4 { 0 } else { n - 3 }];
        let a = dc[idx - 1] as i32;
        let mut b = dc[idx - 1 - wrap] as i32;
        let mut c = dc[idx - wrap] as i32;
        let mut a = a;
        if self.first_slice_line && n != 3 {
            if n != 2 {
                b = 1024;
                c = 1024;
            }
            if n != 1 && self.mb_x == self.resync_mb_x {
                b = 1024;
                a = 1024;
            }
        }
        if self.mb_x == self.resync_mb_x
            && self.mb_y == self.resync_mb_y + 1
            && (n == 0 || n == 4 || n == 5)
        {
            b = 1024;
        }
        if (a - b).abs() < (b - c).abs() {
            (c, 1)
        } else {
            (a, 0)
        }
    }

    #[inline]
    pub(crate) fn dc_pos(&self, n: usize) -> (usize, usize) {
        if n < 4 {
            (
                self.yidx(2 * self.mb_x + (n & 1), 2 * self.mb_y + (n >> 1)),
                self.w8,
            )
        } else {
            (self.cidx(self.mb_x, self.mb_y), self.wc)
        }
    }

    fn get_level_dc(&mut self, n: usize, pred: i32, level: i32) -> i32 {
        let scale = if n < 4 {
            self.y_dc_scale
        } else {
            self.c_dc_scale
        };
        let pred = (pred + (scale >> 1)) / scale;
        let level = level + pred;
        let ret = level;
        let mut l = level * scale;
        if l & !2047 != 0 {
            if l < 0 {
                l = 0;
            } else if !self.bug_dc_clip {
                l = 2047;
            }
        }
        let (idx, _) = self.dc_pos(n);
        let p = if n < 4 { 0 } else { n - 3 };
        self.dc[p][idx] = l as i16;
        ret
    }

    fn decode_dc(&mut self, br: &mut Br, n: usize) -> Result<(i32, i32)> {
        let v = vlcs();
        let code = if n < 4 {
            v.dc_lum.get(br)
        } else {
            v.dc_chrom.get(br)
        }
        .ok_or_else(|| corrupt("MPEG-4 illegal dc vlc"))?;
        let level = if code == 0 {
            0
        } else {
            let l = br.xbits(code);
            if code > 8 {
                br.bit();
            }
            l
        };
        let (pred, dir) = self.pred_dc(n);
        Ok((self.get_level_dc(n, pred, level), dir))
    }

    pub(crate) fn pred_ac(&mut self, blk: &mut [i16; 64], n: usize, dir: i32, ac_pred: bool) {
        let (idx, wrap) = self.dc_pos(n);
        let p = if n < 4 { 0 } else { n - 3 };
        let xy = self.mb_x + self.mb_y * self.mb_w;
        if ac_pred {
            if dir == 0 {
                let l = self.ac[p][idx - 1];
                if self.mb_x == 0
                    || self.qscale == self.qscale_table[xy - 1] as i32
                    || n == 1
                    || n == 3
                {
                    for i in 1..8 {
                        blk[i << 3] = blk[i << 3].wrapping_add(l[i]);
                    }
                } else {
                    let q = self.qscale_table[xy - 1] as i32;
                    for i in 1..8 {
                        blk[i << 3] = blk[i << 3]
                            .wrapping_add(rounded_div(l[i] as i32 * q, self.qscale) as i16);
                    }
                }
            } else {
                let t = self.ac[p][idx - wrap];
                if self.mb_y == 0
                    || self.qscale == self.qscale_table[xy - self.mb_w] as i32
                    || n == 2
                    || n == 3
                {
                    for i in 1..8 {
                        blk[i] = blk[i].wrapping_add(t[i + 8]);
                    }
                } else {
                    let q = self.qscale_table[xy - self.mb_w] as i32;
                    for i in 1..8 {
                        blk[i] = blk[i]
                            .wrapping_add(rounded_div(t[i + 8] as i32 * q, self.qscale) as i16);
                    }
                }
            }
        }
        let a = &mut self.ac[p][idx];
        for i in 1..8 {
            a[i] = blk[i << 3];
            a[8 + i] = blk[i];
        }
    }

    /// `mpeg4_decode_block` (non-RVLC). Returns the block's last index.
    fn decode_block(
        &mut self,
        br: &mut Br,
        w: &mut MbWork,
        n: usize,
        coded: bool,
        intra: bool,
        use_dc_vlc: bool,
    ) -> Result<i32> {
        let v = vlcs();
        let mut i: i32;
        let mut dc_pred_dir = 0;
        let mut pred = 0;
        let scan: &[u8; 64];
        let qmul: i32;
        let qadd: i32;
        let rl;
        let alt = self.alternate_scan;
        let ac_pred = w.ac_pred;
        if intra {
            if use_dc_vlc {
                let (lv, d) = self.decode_dc(br, n)?;
                dc_pred_dir = d;
                w.blocks[n][0] = lv as i16;
                i = 0;
            } else {
                i = -1;
                let (p, d) = self.pred_dc(n);
                pred = p;
                dc_pred_dir = d;
            }
            if ac_pred {
                scan = if dc_pred_dir == 0 || alt {
                    &ALTERNATE_VERTICAL_SCAN
                } else {
                    &ALTERNATE_HORIZONTAL_SCAN
                };
            } else {
                scan = if alt {
                    &ALTERNATE_VERTICAL_SCAN
                } else {
                    &ZIGZAG_DIRECT
                };
            }
            qmul = 1;
            qadd = 0;
            rl = &v.rl_intra;
        } else {
            i = -1;
            if !coded {
                return Ok(-1);
            }
            rl = &v.rl_inter;
            scan = if alt {
                &ALTERNATE_VERTICAL_SCAN
            } else {
                &ZIGZAG_DIRECT
            };
            if self.vol.mpeg_quant {
                qmul = 1;
                qadd = 0;
            } else {
                qmul = self.qscale << 1;
                qadd = (self.qscale - 1) | 1;
            }
        }
        if coded {
            let blk = &mut w.blocks[n];
            loop {
                let sym = rl
                    .vlc
                    .get(br)
                    .ok_or_else(|| corrupt("MPEG-4 invalid AC vlc"))?
                    as usize;
                let mut level: i32;
                if sym == rl.n {
                    let c = br.peek(2);
                    if c & 2 != 0 {
                        if c & 1 != 0 {
                            // third escape
                            br.skip(2);
                            let last = br.bit() as i32;
                            let run = br.get(6) as i32;
                            if br.bit() == 0 {
                                return Err(corrupt("MPEG-4: marker bit missing in 3. esc"));
                            }
                            level = ((br.get(12) as i32) << 20) >> 20;
                            if br.bit() == 0 {
                                return Err(corrupt("MPEG-4: marker bit missing in 3. esc"));
                            }
                            level = if level > 0 {
                                level * qmul + qadd
                            } else {
                                level * qmul - qadd
                            };
                            if (level + 2048) as u32 > 4095 {
                                level = if level < 0 { -2048 } else { 2047 };
                            }
                            i += run + 1;
                            if last != 0 {
                                i += 192;
                            }
                        } else {
                            // second escape
                            br.skip(2);
                            let s2 = rl
                                .vlc
                                .get(br)
                                .ok_or_else(|| corrupt("MPEG-4 invalid AC vlc"))?
                                as usize;
                            if s2 >= rl.n {
                                return Err(corrupt("MPEG-4 invalid AC vlc"));
                            }
                            let last = (s2 >= rl.last) as usize;
                            let r = rl.run[s2] as i32;
                            let lv = rl.level[s2] as i32;
                            i += r + rl.max_run[last][lv as usize] as i32 + 2;
                            if last != 0 {
                                i += 192;
                            }
                            level = lv * qmul + qadd;
                            if br.bit() != 0 {
                                level = -level;
                            }
                        }
                    } else {
                        // first escape
                        br.skip(1);
                        let s2 = rl
                            .vlc
                            .get(br)
                            .ok_or_else(|| corrupt("MPEG-4 invalid AC vlc"))?
                            as usize;
                        if s2 >= rl.n {
                            return Err(corrupt("MPEG-4 invalid AC vlc"));
                        }
                        let last = (s2 >= rl.last) as usize;
                        let r = rl.run[s2] as i32;
                        let lv = rl.level[s2] as i32;
                        i += r + 1;
                        if last != 0 {
                            i += 192;
                        }
                        level = (lv + rl.max_level[last][r as usize] as i32) * qmul + qadd;
                        if br.bit() != 0 {
                            level = -level;
                        }
                    }
                } else {
                    let last = (sym >= rl.last) as usize;
                    i += rl.run[sym] as i32 + 1;
                    if last != 0 {
                        i += 192;
                    }
                    level = rl.level[sym] as i32 * qmul + qadd;
                    if br.bit() != 0 {
                        level = -level;
                    }
                }
                if i > 62 {
                    i -= 192;
                    if i & !63 != 0 {
                        return Err(corrupt("MPEG-4 ac-tex damaged"));
                    }
                    blk[scan[i as usize] as usize] = level as i16;
                    break;
                }
                blk[scan[i as usize] as usize] = level as i16;
                if br.left() < 0 {
                    return Err(corrupt("MPEG-4 block overread"));
                }
            }
        }
        if intra {
            if !use_dc_vlc {
                let d0 = w.blocks[n][0] as i32;
                w.blocks[n][0] = self.get_level_dc(n, pred, d0) as i16;
                if i == -1 {
                    i = 0;
                }
            }
            let mut b = w.blocks[n];
            self.pred_ac(&mut b, n, dc_pred_dir, ac_pred);
            w.blocks[n] = b;
            if ac_pred {
                i = 63;
            }
        }
        Ok(i)
    }

    fn get_amv(&self, n: usize) -> i32 {
        let len = 1i32 << (self.f_code + 4);
        let a = self.vol.sprite_warping_accuracy as i32;
        let qs = self.vol.quarter_sample as i32;
        let sp = &self.sprite;
        let sum: i64 = if sp.real_points == 1 {
            if self.ident.divx_version == 500 && self.ident.divx_build == 413 && a >= qs {
                sp.offset[0][n] / (1i64 << (a - qs))
            } else {
                rshift(sp.offset[0][n] * (1i64 << qs), a)
            }
        } else {
            let mut dx = sp.delta[n][0] as i32;
            let mut dy = sp.delta[n][1] as i32;
            let shift = sp.shift[0];
            if n != 0 {
                dy = dy.wrapping_sub(1i32.wrapping_shl((shift + a + 1) as u32));
            } else {
                dx = dx.wrapping_sub(1i32.wrapping_shl((shift + a + 1) as u32));
            }
            let mb_v = (sp.offset[0][n] as i32)
                .wrapping_add(dx.wrapping_mul(self.mb_x as i32).wrapping_mul(16))
                .wrapping_add(dy.wrapping_mul(self.mb_y as i32).wrapping_mul(16));
            let mut sum: i32 = 0;
            for y in 0..16 {
                let mut v = mb_v.wrapping_add(dy.wrapping_mul(y));
                for _ in 0..16 {
                    sum = sum.wrapping_add(v >> shift);
                    v = v.wrapping_add(dx);
                }
            }
            rshift(sum as i64, a + 8 - qs)
        };
        (sum.max(-(len as i64)).min(len as i64 - 1)) as i32
    }

    fn set_one_direct_mv(&self, w: &mut MbWork, mx: i32, my: i32, i: usize) {
        let bx = 2 * self.mb_x as isize + (i & 1) as isize;
        let by = 2 * self.mb_y as isize + (i >> 1) as isize;
        let nx = self.mvi(bx, by);
        let nm = self.next.as_ref().unwrap().mv[nx];
        let tpp = self.ts.pp_time as i32;
        let tpb = self.ts.pb_time as i32;
        for c in 0..2 {
            let p = nm[c] as i32;
            let m = if c == 0 { mx } else { my };
            let (v0, v1);
            if ((p + 32) as u32) < 64 {
                v0 = self.direct_scale_mv[0][(p + 32) as usize] as i32 + m;
                v1 = if m != 0 {
                    v0 - p
                } else {
                    self.direct_scale_mv[1][(p + 32) as usize] as i32
                };
            } else {
                v0 = p * tpb / tpp + m;
                v1 = if m != 0 {
                    v0 - p
                } else {
                    p * (tpb - tpp) / tpp
                };
            }
            w.mv[0][i][c] = v0;
            w.mv[1][i][c] = v1;
        }
    }

    fn set_direct_mv(&self, w: &mut MbWork, mx: i32, my: i32) -> Result<()> {
        if self.ts.pp_time == 0 {
            return Err(corrupt("MPEG-4 direct mode: pp_time 0"));
        }
        let next = self.next.as_ref().unwrap();
        let mbi = self.mb_x + self.mb_y * self.mb_w;
        let kind = next.mb_kind[mbi];
        if kind & K_8X8 != 0 {
            w.mvt = Mvt::M8;
            for i in 0..4 {
                self.set_one_direct_mv(w, mx, my, i);
            }
        } else if kind & K_FIELD != 0 {
            w.mvt = Mvt::Field;
            for i in 0..2 {
                let fs = next.field_sel[mbi][i] as i32;
                w.field_select[0][i] = fs as u8;
                w.field_select[1][i] = i as u8;
                let (tpp, tpb) = if self.top_field_first {
                    (
                        (self.ts.pp_field_time as i32 - fs + i as i32) as u16,
                        (self.ts.pb_field_time as i32 - fs + i as i32) as u16,
                    )
                } else {
                    (
                        (self.ts.pp_field_time as i32 + fs - i as i32) as u16,
                        (self.ts.pb_field_time as i32 + fs - i as i32) as u16,
                    )
                };
                if tpp == 0 {
                    return Err(corrupt("MPEG-4 direct mode: zero field time"));
                }
                let (tpp, tpb) = (tpp as i32, tpb as i32);
                let fm = next.field_mv[mbi][i];
                for c in 0..2 {
                    let p = fm[c] as i32;
                    let m = if c == 0 { mx } else { my };
                    let v0 = p * tpb / tpp + m;
                    w.mv[0][i][c] = v0;
                    w.mv[1][i][c] = if m != 0 {
                        v0 - p
                    } else {
                        p * (tpb - tpp) / tpp
                    };
                }
            }
        } else {
            self.set_one_direct_mv(w, mx, my, 0);
            for i in 1..4 {
                w.mv[0][i] = w.mv[0][0];
                w.mv[1][i] = w.mv[1][0];
            }
            w.mvt = if self.bug_direct_blocksize || !self.vol.quarter_sample {
                Mvt::M16
            } else {
                Mvt::M8
            };
        }
        Ok(())
    }

    /// `mpeg4_is_resync`: 0 if not at a resync marker / end, else mb_num.
    fn is_resync(&self, br: &mut Br) -> isize {
        let mut bits_count = br.pos();
        let mut v = br.peek(16);
        let ptn = pt_num(self.pt);
        while v <= 0xFF {
            if self.pt == Pt::B || (v >> (8 - ptn)) != 1 || self.partitioned {
                break;
            }
            br.skip((8 + ptn) as u32);
            bits_count += (8 + ptn) as usize;
            v = br.peek(16);
        }
        if bits_count + 8 >= br.size_bits() {
            v >>= 8;
            v |= 0x7F >> (7 - (bits_count & 7) as u32);
            if v == 0x7F {
                return self.mb_num as isize;
            }
        } else {
            const PREFIX: [u32; 8] = [
                0x7F00, 0x7E00, 0x7C00, 0x7800, 0x7000, 0x6000, 0x4000, 0x0000,
            ];
            if v == PREFIX[bits_count & 7] {
                let mb_num_bits = usize::BITS - (self.mb_num - 1).leading_zeros();
                let save = br.pos();
                br.skip(1);
                br.align();
                let mut len = 0;
                while len < 32 {
                    if br.bit() != 0 {
                        break;
                    }
                    len += 1;
                }
                let mut mb_num = br.get(mb_num_bits) as isize;
                if mb_num == 0 || mb_num > self.mb_num as isize || br.pos() + 6 > br.size_bits() {
                    mb_num = -1;
                }
                br.set_pos(save);
                if len >= self.video_packet_prefix_len() {
                    return mb_num;
                }
            }
        }
        0
    }

    pub fn video_packet_prefix_len(&self) -> i32 {
        match self.pt {
            Pt::I => 16,
            Pt::B => self.f_code.max(self.b_code).max(2) + 15,
            _ => self.f_code + 15,
        }
    }

    /// Decode one macroblock at (mb_x, mb_y).
    pub fn decode_mb(&mut self, br: &mut Br, w: &mut MbWork) -> Result<MbEnd> {
        let v = vlcs();
        const QUANT_TAB: [i32; 4] = [-1, -2, 1, 2];
        let xy = self.mb_x + self.mb_y * self.mb_w;
        w.intra = false;
        w.mcsel = false;
        w.skipped = false;
        w.ac_pred = false;
        w.mvt = Mvt::M16;
        w.mv_dir = 1;
        w.last_index = [-1; 6];
        let mut cbp: u32;
        let mut kind = 0u8;
        let progressive = self.vol.progressive_sequence;
        let gmc_vop = self.pt == Pt::S && self.vol.sprite_usage == 2;
        let mut intra_path = false;
        let mut cbpc: u32 = 0;
        let mut dquant = false;
        if self.pt == Pt::P || self.pt == Pt::S {
            loop {
                if br.bit() != 0 {
                    // skipped MB
                    w.mv_dir = 1;
                    w.mvt = Mvt::M16;
                    if gmc_vop {
                        kind = K_GMC;
                        w.mcsel = true;
                        w.mv[0][0] = [self.get_amv(0), self.get_amv(1)];
                        self.cur.mbskip[xy] = 0;
                        w.skipped = false;
                    } else {
                        w.mcsel = false;
                        w.mv[0][0] = [0, 0];
                        self.cur.mbskip[xy] = 1;
                        w.skipped = true;
                    }
                    self.cur.mb_kind[xy] = kind;
                    return self.mb_end(br);
                }
                cbpc = v
                    .inter_mcbpc
                    .get(br)
                    .ok_or_else(|| corrupt("MPEG-4 mcbpc damaged"))?;
                if cbpc != 20 {
                    break;
                }
            }
            dquant = cbpc & 8 != 0;
            w.intra = cbpc & 4 != 0;
            if w.intra {
                intra_path = true;
            }
        } else if self.pt == Pt::I {
            loop {
                cbpc = v
                    .intra_mcbpc
                    .get(br)
                    .ok_or_else(|| corrupt("MPEG-4 I cbpc damaged"))?;
                if cbpc != 8 {
                    break;
                }
            }
            dquant = cbpc & 4 != 0;
            w.intra = true;
            intra_path = true;
        }
        self.cur.mbskip[xy] = 0;
        if intra_path {
            w.ac_pred = br.bit() != 0;
            kind = K_INTRA;
            let cbpy = v
                .cbpy
                .get(br)
                .ok_or_else(|| corrupt("MPEG-4 I cbpy damaged"))?;
            cbp = (cbpc & 3) | (cbpy << 2);
            let use_dc_vlc = self.qscale < self.intra_dc_threshold;
            if dquant {
                let q = self.qscale + QUANT_TAB[br.get(2) as usize];
                self.set_qscale(q);
            }
            if !progressive {
                w.interlaced_dct = br.bit() != 0;
            }
            self.qscale_table[xy] = self.qscale as i8;
            for i in 0..6 {
                let li = self.decode_block(br, w, i, cbp & 32 != 0, true, use_dc_vlc)?;
                w.last_index[i] = li;
                cbp += cbp;
            }
            self.cur.mb_kind[xy] = kind;
            return self.mb_end(br);
        }
        if self.pt == Pt::B {
            return self.decode_mb_b(br, w);
        }
        // inter MB in P / S
        w.mcsel = gmc_vop && (cbpc & 16) == 0 && br.bit() != 0;
        let cbpy = v
            .cbpy
            .get(br)
            .ok_or_else(|| corrupt("MPEG-4 P cbpy damaged"))?
            ^ 0x0F;
        cbp = (cbpc & 3) | (cbpy << 2);
        if dquant {
            let q = self.qscale + QUANT_TAB[br.get(2) as usize];
            self.set_qscale(q);
        }
        if !progressive && (cbp != 0 || self.bug_xvid_ilace) {
            w.interlaced_dct = br.bit() != 0;
        }
        w.mv_dir = 1;
        if cbpc & 16 == 0 {
            if w.mcsel {
                kind = K_GMC;
                w.mvt = Mvt::M16;
                w.mv[0][0] = [self.get_amv(0), self.get_amv(1)];
            } else if !progressive && br.bit() != 0 {
                kind = K_FIELD;
                w.mvt = Mvt::Field;
                w.field_select[0][0] = br.bit() as u8;
                w.field_select[0][1] = br.bit() as u8;
                let (px, py) = self.pred_motion(0);
                for i in 0..2 {
                    let mx = self.decode_motion(br, px, self.f_code)?;
                    let my = self.decode_motion(br, py / 2, self.f_code)?;
                    w.mv[0][i] = [mx, my];
                }
            } else {
                w.mvt = Mvt::M16;
                let (px, py) = self.pred_motion(0);
                let mx = self.decode_motion(br, px, self.f_code)?;
                let my = self.decode_motion(br, py, self.f_code)?;
                w.mv[0][0] = [mx, my];
            }
        } else {
            kind = K_8X8;
            w.mvt = Mvt::M8;
            for i in 0..4 {
                let (px, py) = self.pred_motion(i);
                let mx = self.decode_motion(br, px, self.f_code)?;
                let my = self.decode_motion(br, py, self.f_code)?;
                w.mv[0][i] = [mx, my];
                let bx = 2 * self.mb_x as isize + (i & 1) as isize;
                let by = 2 * self.mb_y as isize + (i >> 1) as isize;
                let ci = self.mvi(bx, by);
                self.cur.mv[ci] = [mx as i16, my as i16];
            }
        }
        self.qscale_table[xy] = self.qscale as i8;
        self.cur.mb_kind[xy] = kind;
        for i in 0..6 {
            let li = self.decode_block(br, w, i, cbp & 32 != 0, false, false)?;
            w.last_index[i] = li;
            cbp += cbp;
        }
        self.mb_end(br)
    }

    fn mb_end(&mut self, br: &mut Br) -> Result<MbEnd> {
        let xy = self.mb_x + self.mb_y * self.mb_w;
        self.qscale_table[xy] = self.qscale as i8;
        let next = self.is_resync(br);
        if next != 0 {
            if (self.mb_x + self.mb_y * self.mb_w + 1) as isize >= next {
                return Ok(MbEnd::SliceEnd);
            }
            if self.pt == Pt::B {
                let delta = if self.mb_x + 1 == self.mb_w { 2 } else { 1 };
                if let Some(n) = &self.next
                    && n.mbskip.get(xy + delta).copied().unwrap_or(0) != 0
                {
                    return Ok(MbEnd::Ok);
                }
            }
            return Ok(MbEnd::SliceEnd);
        }
        Ok(MbEnd::Ok)
    }

    fn decode_mb_b(&mut self, br: &mut Br, w: &mut MbWork) -> Result<MbEnd> {
        let v = vlcs();
        let xy = self.mb_x + self.mb_y * self.mb_w;
        let progressive = self.vol.progressive_sequence;
        w.intra = false;
        w.mcsel = false;
        if self.mb_x == 0 {
            self.last_mv = [[[0; 2]; 2]; 2];
        }
        let skipped = self
            .next
            .as_ref()
            .map(|n| n.mbskip[xy] != 0)
            .unwrap_or(false);
        if skipped {
            w.skipped = true;
            w.mv_dir = 1;
            w.mvt = Mvt::M16;
            w.mv[0][0] = [0, 0];
            w.mv[1][0] = [0, 0];
            self.cur.mb_kind[xy] = 0;
            // ffmpeg keeps mb_skipped unset for skip handling in B: mv zero forward, no residual
            return self.mb_end(br);
        }
        w.skipped = false;
        let mut cbp: u32 = 0;
        let mut direct = false;
        let mut skip_direct = false;
        let mut has_fwd = false;
        let mut has_bwd = false;
        let mut field = false;
        if br.bit() != 0 {
            direct = true;
            skip_direct = true;
        } else {
            let modb2 = br.bit() != 0;
            let t = v
                .mb_type_b
                .get(br)
                .ok_or_else(|| corrupt("MPEG-4 illegal MB_type"))?;
            match t {
                0 => direct = true,
                1 => {
                    has_fwd = true;
                    has_bwd = true;
                }
                2 => has_bwd = true,
                _ => has_fwd = true,
            }
            if !modb2 {
                cbp = br.get(6);
            }
            if !direct && cbp != 0 && br.bit() != 0 {
                let q = self.qscale + br.bit() as i32 * 4 - 2;
                self.set_qscale(q);
            }
            if !progressive {
                if cbp != 0 {
                    w.interlaced_dct = br.bit() != 0;
                }
                if !direct && br.bit() != 0 {
                    field = true;
                    if has_fwd {
                        w.field_select[0][0] = br.bit() as u8;
                        w.field_select[0][1] = br.bit() as u8;
                    }
                    if has_bwd {
                        w.field_select[1][0] = br.bit() as u8;
                        w.field_select[1][1] = br.bit() as u8;
                    }
                }
            }
            w.mv_dir = 0;
            if !direct && !field {
                w.mvt = Mvt::M16;
                if has_fwd {
                    w.mv_dir = 1;
                    let mx = self.decode_motion(br, self.last_mv[0][0][0], self.f_code)?;
                    let my = self.decode_motion(br, self.last_mv[0][0][1], self.f_code)?;
                    self.last_mv[0][1] = [mx, my];
                    self.last_mv[0][0] = [mx, my];
                    w.mv[0][0] = [mx, my];
                }
                if has_bwd {
                    w.mv_dir |= 2;
                    let mx = self.decode_motion(br, self.last_mv[1][0][0], self.b_code)?;
                    let my = self.decode_motion(br, self.last_mv[1][0][1], self.b_code)?;
                    self.last_mv[1][1] = [mx, my];
                    self.last_mv[1][0] = [mx, my];
                    w.mv[1][0] = [mx, my];
                }
            } else if !direct {
                w.mvt = Mvt::Field;
                if has_fwd {
                    w.mv_dir = 1;
                    for i in 0..2 {
                        let mx = self.decode_motion(br, self.last_mv[0][i][0], self.f_code)?;
                        let my = self.decode_motion(br, self.last_mv[0][i][1] / 2, self.f_code)?;
                        self.last_mv[0][i] = [mx, my * 2];
                        w.mv[0][i] = [mx, my];
                    }
                }
                if has_bwd {
                    w.mv_dir |= 2;
                    for i in 0..2 {
                        let mx = self.decode_motion(br, self.last_mv[1][i][0], self.b_code)?;
                        let my = self.decode_motion(br, self.last_mv[1][i][1] / 2, self.b_code)?;
                        self.last_mv[1][i] = [mx, my * 2];
                        w.mv[1][i] = [mx, my];
                    }
                }
            }
        }
        if direct {
            let (mx, my) = if skip_direct {
                (0, 0)
            } else {
                let mx = self.decode_motion(br, 0, 1)?;
                let my = self.decode_motion(br, 0, 1)?;
                (mx, my)
            };
            w.mv_dir = 3;
            self.set_direct_mv(w, mx, my)?;
        }
        self.cur.mb_kind[xy] = 0;
        self.qscale_table[xy] = self.qscale as i8;
        for i in 0..6 {
            let li = self.decode_block(br, w, i, cbp & 32 != 0, false, false)?;
            w.last_index[i] = li;
            cbp += cbp;
        }
        self.mb_end(br)
    }

    /// `ff_h263_update_motion_val` plus intra-table maintenance.
    pub fn post_mb(&mut self, w: &MbWork) {
        let xy = self.mb_x + self.mb_y * self.mb_w;
        if self.pt != Pt::B && w.mvt != Mvt::M8 {
            let (mx, my);
            if w.intra {
                mx = 0;
                my = 0;
            } else if w.mvt == Mvt::M16 {
                mx = w.mv[0][0][0];
                my = w.mv[0][0][1];
            } else {
                let sx = w.mv[0][0][0] + w.mv[0][1][0];
                let sy = w.mv[0][0][1] + w.mv[0][1][1];
                mx = (sx >> 1) | (sx & 1);
                my = sy;
                self.cur.field_mv[xy] = [
                    [w.mv[0][0][0] as i16, w.mv[0][0][1] as i16],
                    [w.mv[0][1][0] as i16, w.mv[0][1][1] as i16],
                ];
                self.cur.field_sel[xy] = [w.field_select[0][0], w.field_select[0][1]];
            }
            let b8 = self.b8s as isize;
            let base = self.mvi(2 * self.mb_x as isize, 2 * self.mb_y as isize);
            let m = [mx as i16, my as i16];
            self.cur.mv[base] = m;
            self.cur.mv[base + 1] = m;
            self.cur.mv[(base as isize + b8) as usize] = m;
            self.cur.mv[(base as isize + b8 + 1) as usize] = m;
        }
        if !w.intra {
            // ff_h263_clean_intra_table_entries
            for n in 0..6 {
                let (idx, _) = self.dc_pos(n);
                let p = if n < 4 { 0 } else { n - 3 };
                self.dc[p][idx] = 1024;
                self.ac[p][idx] = [0; 16];
            }
        }
    }
}
