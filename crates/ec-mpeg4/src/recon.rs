//! Motion compensation and residual reconstruction.

use crate::core::*;
use crate::dsp;
use crate::header::Pt;
use crate::tables::*;

const WS: usize = 24;
type WinBuf = [u8; WS * 20];

/// Geometry and flags shared by the MC routines.
#[derive(Clone, Copy)]
struct Geo {
    mb_x: i32,
    mb_y: i32,
    aw: usize,
    ew: i32,
    eh: i32,
    w: i32,
    h: i32,
    bug_hpel_chroma: bool,
    bug_qpel_chroma: bool,
    bug_qpel_chroma2: bool,
    no_rounding: bool,
}

/// A source window: either a view straight into the plane or a clamped copy.
fn window<'a>(
    plane: &'a [u8],
    stride: usize,
    pw: i32,
    ph: i32,
    x: i32,
    y: i32,
    ww: i32,
    wh: i32,
    ystep: i32,
    ysel: i32,
    buf: &'a mut WinBuf,
) -> (&'a [u8], usize) {
    let last_row = (y + wh - 1) * ystep + ysel;
    if x >= 0 && x + ww <= pw && y * ystep + ysel >= 0 && last_row < ph {
        let off = (y * ystep + ysel) as usize * stride + x as usize;
        return (&plane[off..], stride * ystep as usize);
    }
    for r in 0..wh {
        let fy = ((y + r) * ystep + ysel).clamp(0, ph - 1) as usize;
        let row = &plane[fy * stride..fy * stride + pw as usize];
        let o = &mut buf[r as usize * WS..r as usize * WS + ww as usize];
        for (c, d) in o.iter_mut().enumerate() {
            *d = row[(x + c as i32).clamp(0, pw - 1) as usize];
        }
    }
    (&buf[..], WS)
}

fn put_or_avg_hpel(
    avg: bool,
    dst: &mut [u8],
    ds: usize,
    src: &[u8],
    ss: usize,
    w: usize,
    h: usize,
    dxy: usize,
    no_rnd: bool,
) {
    if avg {
        dsp::avg_hpel(dst, ds, src, ss, w, h, dxy, false);
    } else {
        dsp::put_hpel(dst, ds, src, ss, w, h, dxy, no_rnd);
    }
}

fn put_or_avg_qpel(
    avg: bool,
    dst: &mut [u8],
    ds: usize,
    src: &[u8],
    ss: usize,
    size: usize,
    mc: usize,
    no_rnd: bool,
) {
    if avg {
        dsp::avg_qpel(dst, ds, src, ss, size, mc, false);
    } else {
        dsp::put_qpel(dst, ds, src, ss, size, mc, no_rnd);
    }
}

/// `mpeg_motion_internal` for H.263-family output (16x16 or one field of 8 rows).
#[allow(clippy::too_many_arguments)]
fn mpeg_motion(
    g: &Geo,
    cur: &mut Pic,
    rf: &Pic,
    fb: bool,
    bottom: usize,
    sel: usize,
    mx: i32,
    my: i32,
    avg: bool,
    no_rnd: bool,
) {
    let h = if fb { 8 } else { 16 };
    let fbi = fb as i32;
    let dxy = (((my & 1) << 1) | (mx & 1)) as usize;
    let src_x = g.mb_x * 16 + (mx >> 1);
    let src_y = (g.mb_y << (4 - fbi)) + (my >> 1);
    let (uvdxy, uvsrc_x, uvsrc_y);
    if g.bug_hpel_chroma && fb {
        let cmx = (mx >> 1) | (mx & 1);
        let cmy = my >> 1;
        uvdxy = (((cmy & 1) << 1) | (cmx & 1)) as usize;
        uvsrc_x = g.mb_x * 8 + (cmx >> 1);
        uvsrc_y = (g.mb_y << 2) + (cmy >> 1);
    } else {
        uvdxy = dxy | (my & 2) as usize | ((mx & 2) >> 1) as usize;
        uvsrc_x = src_x >> 1;
        uvsrc_y = src_y >> 1;
    }
    let aw = g.aw;
    let cw = aw / 2;
    let (ystep, ysel) = (1 + fbi, if fb { sel as i32 } else { 0 });
    let mut buf: WinBuf = [0; WS * 20];
    let (s, ss) = window(
        &rf.y,
        aw,
        g.ew,
        g.eh,
        src_x,
        src_y,
        17,
        h + 1,
        ystep,
        ysel,
        &mut buf,
    );
    let doff = (g.mb_y as usize * 16 + bottom) * aw + g.mb_x as usize * 16;
    dsp_dst(
        avg,
        &mut cur.y[doff..],
        aw << fb as usize,
        s,
        ss,
        16,
        h as usize,
        dxy,
        no_rnd,
    );
    let ch = h as usize >> 1;
    let coff = (g.mb_y as usize * 8 + bottom) * cw + g.mb_x as usize * 8;
    for p in 0..2 {
        let plane = if p == 0 { &rf.u } else { &rf.v };
        let mut buf: WinBuf = [0; WS * 20];
        let (s, ss) = window(
            plane,
            cw,
            g.ew >> 1,
            g.eh >> 1,
            uvsrc_x,
            uvsrc_y,
            9,
            ch as i32 + 1,
            ystep,
            ysel,
            &mut buf,
        );
        let dst = if p == 0 { &mut cur.u } else { &mut cur.v };
        dsp_dst(
            avg,
            &mut dst[coff..],
            cw << fb as usize,
            s,
            ss,
            8,
            ch,
            uvdxy,
            no_rnd,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn dsp_dst(
    avg: bool,
    dst: &mut [u8],
    ds: usize,
    src: &[u8],
    ss: usize,
    w: usize,
    h: usize,
    dxy: usize,
    no_rnd: bool,
) {
    put_or_avg_hpel(avg, dst, ds, src, ss, w, h, dxy, no_rnd);
}

/// `qpel_motion`.
#[allow(clippy::too_many_arguments)]
fn qpel_motion(
    g: &Geo,
    cur: &mut Pic,
    rf: &Pic,
    fb: bool,
    bottom: usize,
    sel: usize,
    mx: i32,
    my: i32,
    avg: bool,
    no_rnd: bool,
) {
    let h = if fb { 8 } else { 16 };
    let fbi = fb as i32;
    let dxy = (((my & 3) << 2) | (mx & 3)) as usize;
    let src_x = g.mb_x * 16 + (mx >> 2);
    let src_y = g.mb_y * (16 >> fbi) + (my >> 2);
    let (mut cmx, mut cmy);
    if fb {
        cmx = mx / 2;
        cmy = my >> 1;
    } else if g.bug_qpel_chroma2 {
        const RTAB: [i32; 8] = [0, 0, 1, 1, 0, 0, 0, 1];
        cmx = (mx >> 1) + RTAB[(mx & 7) as usize];
        cmy = (my >> 1) + RTAB[(my & 7) as usize];
    } else if g.bug_qpel_chroma {
        cmx = (mx >> 1) | (mx & 1);
        cmy = (my >> 1) | (my & 1);
    } else {
        cmx = mx / 2;
        cmy = my / 2;
    }
    cmx = (cmx >> 1) | (cmx & 1);
    cmy = (cmy >> 1) | (cmy & 1);
    let uvdxy = ((cmx & 1) | ((cmy & 1) << 1)) as usize;
    cmx >>= 1;
    cmy >>= 1;
    let uvsrc_x = g.mb_x * 8 + cmx;
    let uvsrc_y = g.mb_y * (8 >> fbi) + cmy;
    let aw = g.aw;
    let cw = aw / 2;
    let (ystep, ysel) = (1 + fbi, if fb { sel as i32 } else { 0 });
    let mut buf: WinBuf = [0; WS * 20];
    let (s, ss) = window(
        &rf.y,
        aw,
        g.ew,
        g.eh,
        src_x,
        src_y,
        17,
        h + 1,
        ystep,
        ysel,
        &mut buf,
    );
    let doff = (g.mb_y as usize * 16 + bottom) * aw + g.mb_x as usize * 16;
    let ds = aw << fb as usize;
    if !fb {
        put_or_avg_qpel(avg, &mut cur.y[doff..], ds, s, ss, 16, dxy, no_rnd);
    } else {
        put_or_avg_qpel(avg, &mut cur.y[doff..], ds, s, ss, 8, dxy, no_rnd);
        put_or_avg_qpel(avg, &mut cur.y[doff + 8..], ds, &s[8..], ss, 8, dxy, no_rnd);
    }
    let ch = h as usize >> 1;
    let coff = (g.mb_y as usize * 8 + bottom) * cw + g.mb_x as usize * 8;
    for p in 0..2 {
        let plane = if p == 0 { &rf.u } else { &rf.v };
        let mut buf: WinBuf = [0; WS * 20];
        let (s, ss) = window(
            plane,
            cw,
            g.ew >> 1,
            g.eh >> 1,
            uvsrc_x,
            uvsrc_y,
            9,
            ch as i32 + 1,
            ystep,
            ysel,
            &mut buf,
        );
        let dst = if p == 0 { &mut cur.u } else { &mut cur.v };
        put_or_avg_hpel(
            avg,
            &mut dst[coff..],
            cw << fb as usize,
            s,
            ss,
            8,
            ch,
            uvdxy,
            no_rnd,
        );
    }
}

fn round_chroma(x: i32) -> i32 {
    const T: [i32; 16] = [0, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1];
    T[(x & 15) as usize] + (x >> 3)
}

/// `chroma_4mv_motion`.
fn chroma_4mv(g: &Geo, cur: &mut Pic, rf: &Pic, mx: i32, my: i32, avg: bool, no_rnd: bool) {
    let mx = round_chroma(mx);
    let my = round_chroma(my);
    let mut dxy = (((my & 1) << 1) | (mx & 1)) as usize;
    let mut src_x = g.mb_x * 8 + (mx >> 1);
    let mut src_y = g.mb_y * 8 + (my >> 1);
    src_x = src_x.clamp(-8, g.w >> 1);
    if src_x == g.w >> 1 {
        dxy &= !1;
    }
    src_y = src_y.clamp(-8, g.h >> 1);
    if src_y == g.h >> 1 {
        dxy &= !2;
    }
    let cw = g.aw / 2;
    let coff = g.mb_y as usize * 8 * cw + g.mb_x as usize * 8;
    for p in 0..2 {
        let plane = if p == 0 { &rf.u } else { &rf.v };
        let mut buf: WinBuf = [0; WS * 20];
        let (s, ss) = window(
            plane,
            cw,
            g.ew >> 1,
            g.eh >> 1,
            src_x,
            src_y,
            9,
            9,
            1,
            0,
            &mut buf,
        );
        let dst = if p == 0 { &mut cur.u } else { &mut cur.v };
        put_or_avg_hpel(avg, &mut dst[coff..], cw, s, ss, 8, 8, dxy, no_rnd);
    }
}

/// `apply_8x8`.
fn apply_8x8(
    g: &Geo,
    cur: &mut Pic,
    rf: &Pic,
    mv: &[[i32; 2]; 4],
    quarter: bool,
    avg: bool,
    no_rnd: bool,
) {
    let aw = g.aw;
    let (mut sx, mut sy) = (0, 0);
    for i in 0..4 {
        let (mx, my) = (mv[i][0], mv[i][1]);
        let bx = (i & 1) as i32 * 8;
        let by = (i >> 1) as i32 * 8;
        let doff = (g.mb_y as usize * 16 + by as usize) * aw + g.mb_x as usize * 16 + bx as usize;
        let mut buf: WinBuf = [0; WS * 20];
        if quarter {
            let mut dxy = (((my & 3) << 2) | (mx & 3)) as usize;
            let mut src_x = g.mb_x * 16 + (mx >> 2) + bx;
            let mut src_y = g.mb_y * 16 + (my >> 2) + by;
            src_x = src_x.clamp(-16, g.w);
            if src_x == g.w {
                dxy &= !3;
            }
            src_y = src_y.clamp(-16, g.h);
            if src_y == g.h {
                dxy &= !12;
            }
            let (s, ss) = window(&rf.y, aw, g.ew, g.eh, src_x, src_y, 9, 9, 1, 0, &mut buf);
            put_or_avg_qpel(avg, &mut cur.y[doff..], aw, s, ss, 8, dxy, no_rnd);
            sx += mx / 2;
            sy += my / 2;
        } else {
            let mut dxy = 0usize;
            let mut src_x = g.mb_x * 16 + bx + (mx >> 1);
            let mut src_y = g.mb_y * 16 + by + (my >> 1);
            src_x = src_x.clamp(-16, g.w);
            if src_x != g.w {
                dxy |= (mx & 1) as usize;
            }
            src_y = src_y.clamp(-16, g.h);
            if src_y != g.h {
                dxy |= ((my & 1) << 1) as usize;
            }
            let (s, ss) = window(&rf.y, aw, g.ew, g.eh, src_x, src_y, 9, 9, 1, 0, &mut buf);
            put_or_avg_hpel(avg, &mut cur.y[doff..], aw, s, ss, 8, 8, dxy, no_rnd);
            sx += mx;
            sy += my;
        }
    }
    chroma_4mv(g, cur, rf, sx, sy, avg, no_rnd);
}

/// `gmc1_motion` (one warp point).
fn gmc1_motion(g: &Geo, cur: &mut Pic, rf: &Pic, sp: &crate::header::SpriteState, acc: i32) {
    let aw = g.aw;
    let cw = aw / 2;
    let rounder = 128 - g.no_rounding as i32;
    // luma
    let mut mx = sp.offset[0][0] as i32;
    let mut my = sp.offset[0][1] as i32;
    let mut src_x = g.mb_x * 16 + (mx >> (acc + 1));
    let mut src_y = g.mb_y * 16 + (my >> (acc + 1));
    mx = mx.wrapping_mul(1 << (3 - acc));
    my = my.wrapping_mul(1 << (3 - acc));
    src_x = src_x.clamp(-16, g.w);
    if src_x == g.w {
        mx = 0;
    }
    src_y = src_y.clamp(-16, g.h);
    if src_y == g.h {
        my = 0;
    }
    let doff = g.mb_y as usize * 16 * aw + g.mb_x as usize * 16;
    let mut buf: WinBuf = [0; WS * 20];
    let (s, ss) = window(&rf.y, aw, g.ew, g.eh, src_x, src_y, 17, 17, 1, 0, &mut buf);
    if (mx | my) & 7 != 0 {
        dsp::gmc1(&mut cur.y[doff..], aw, s, ss, 16, mx & 15, my & 15, rounder);
        dsp::gmc1(
            &mut cur.y[doff + 8..],
            aw,
            &s[8..],
            ss,
            16,
            mx & 15,
            my & 15,
            rounder,
        );
    } else {
        let dxy = (((mx >> 3) & 1) | ((my >> 2) & 2)) as usize;
        put_or_avg_hpel(
            false,
            &mut cur.y[doff..],
            aw,
            s,
            ss,
            16,
            16,
            dxy,
            g.no_rounding,
        );
    }
    // chroma
    let mut mx = sp.offset[1][0] as i32;
    let mut my = sp.offset[1][1] as i32;
    let mut src_x = g.mb_x * 8 + (mx >> (acc + 1));
    let mut src_y = g.mb_y * 8 + (my >> (acc + 1));
    mx = mx.wrapping_mul(1 << (3 - acc));
    my = my.wrapping_mul(1 << (3 - acc));
    src_x = src_x.clamp(-8, g.w >> 1);
    if src_x == g.w >> 1 {
        mx = 0;
    }
    src_y = src_y.clamp(-8, g.h >> 1);
    if src_y == g.h >> 1 {
        my = 0;
    }
    let coff = g.mb_y as usize * 8 * cw + g.mb_x as usize * 8;
    for p in 0..2 {
        let plane = if p == 0 { &rf.u } else { &rf.v };
        let mut buf: WinBuf = [0; WS * 20];
        let (s, ss) = window(
            plane,
            cw,
            g.ew >> 1,
            g.eh >> 1,
            src_x,
            src_y,
            9,
            9,
            1,
            0,
            &mut buf,
        );
        let dst = if p == 0 { &mut cur.u } else { &mut cur.v };
        dsp::gmc1(&mut dst[coff..], cw, s, ss, 8, mx & 15, my & 15, rounder);
    }
}

impl Core {
    fn geo(&self) -> Geo {
        Geo {
            mb_x: self.mb_x as i32,
            mb_y: self.mb_y as i32,
            aw: self.mb_w * 16,
            ew: if self.bug_edge {
                self.w as i32
            } else {
                (self.mb_w * 16) as i32
            },
            eh: if self.bug_edge {
                self.h as i32
            } else {
                (self.mb_h * 16) as i32
            },
            w: self.w as i32,
            h: self.h as i32,
            bug_hpel_chroma: self.bug_hpel_chroma,
            bug_qpel_chroma: self.bug_qpel_chroma,
            bug_qpel_chroma2: self.bug_qpel_chroma2,
            no_rounding: self.no_rounding,
        }
    }

    /// Reconstruct the current macroblock (prediction plus residual).
    pub fn reconstruct(&mut self, w: &mut MbWork) {
        if w.intra {
            self.recon_intra(w);
            return;
        }
        let g = self.geo();
        let quarter = self.vol.quarter_sample;
        let no_rnd = self.no_rounding && self.pt != Pt::B;
        let last = self.last.clone();
        let next = self.next.clone();
        let mut avg = false;
        for dir in 0..2usize {
            if w.mv_dir & (1 << dir) == 0 {
                continue;
            }
            let rf = match if dir == 0 { &last } else { &next } {
                Some(r) => r.clone(),
                None => continue,
            };
            let nr = no_rnd && !avg;
            match w.mvt {
                Mvt::M16 => {
                    if w.mcsel {
                        let acc = self.vol.sprite_warping_accuracy as i32;
                        if self.sprite.real_points == 1 {
                            gmc1_motion(&g, &mut self.cur, &rf, &self.sprite, acc);
                        } else {
                            gmc_motion(&g, &mut self.cur, &rf, &self.sprite, acc);
                        }
                    } else if quarter {
                        qpel_motion(
                            &g,
                            &mut self.cur,
                            &rf,
                            false,
                            0,
                            0,
                            w.mv[dir][0][0],
                            w.mv[dir][0][1],
                            avg,
                            nr,
                        );
                    } else {
                        mpeg_motion(
                            &g,
                            &mut self.cur,
                            &rf,
                            false,
                            0,
                            0,
                            w.mv[dir][0][0],
                            w.mv[dir][0][1],
                            avg,
                            nr,
                        );
                    }
                }
                Mvt::M8 => apply_8x8(&g, &mut self.cur, &rf, &w.mv[dir], quarter, avg, nr),
                Mvt::Field => {
                    for i in 0..2 {
                        let sel = w.field_select[dir][i] as usize;
                        if quarter {
                            qpel_motion(
                                &g,
                                &mut self.cur,
                                &rf,
                                true,
                                i,
                                sel,
                                w.mv[dir][i][0],
                                w.mv[dir][i][1],
                                avg,
                                nr,
                            );
                        } else {
                            mpeg_motion(
                                &g,
                                &mut self.cur,
                                &rf,
                                true,
                                i,
                                sel,
                                w.mv[dir][i][0],
                                w.mv[dir][i][1],
                                avg,
                                nr,
                            );
                        }
                    }
                }
            }
            avg = true;
        }
        self.recon_inter_residual(w);
    }

    fn recon_inter_residual(&mut self, w: &mut MbWork) {
        let aw = self.mb_w * 16;
        let cw = aw / 2;
        let (dls, dlo) = if w.interlaced_dct {
            (aw * 2, aw)
        } else {
            (aw, aw * 8)
        };
        let base_y = self.mb_y * 16 * aw + self.mb_x * 16;
        let base_c = self.mb_y * 8 * cw + self.mb_x * 8;
        let alt = self.alternate_scan;
        let scan: &[u8; 64] = if alt {
            &ALTERNATE_VERTICAL_SCAN
        } else {
            &ZIGZAG_DIRECT
        };
        for n in 0..6 {
            let li = w.last_index[n];
            if li < 0 {
                continue;
            }
            if self.vol.mpeg_quant {
                dsp::unquant_mpeg2_inter(
                    &mut w.blocks[n],
                    scan,
                    li,
                    self.qscale,
                    &self.vol.inter_matrix,
                );
            } else if self.is_h263 {
                dsp::unquant_h263_inter(
                    &mut w.blocks[n],
                    self.qscale,
                    self.raster_end_zz[li as usize] as usize,
                );
            }
            let blk = &mut w.blocks[n];
            let (plane, off, stride) = match n {
                0 => (0, base_y, dls),
                1 => (0, base_y + 8, dls),
                2 => (0, base_y + dlo, dls),
                3 => (0, base_y + dlo + 8, dls),
                4 => (1, base_c, cw),
                _ => (2, base_c, cw),
            };
            let dst = match plane {
                0 => &mut self.cur.y[off..],
                1 => &mut self.cur.u[off..],
                _ => &mut self.cur.v[off..],
            };
            if self.xvid_idct {
                dsp::idct_xvid_add(blk, dst, stride);
            } else {
                dsp::idct_simple_add(blk, dst, stride);
            }
            *blk = [0; 64];
        }
    }

    fn recon_intra(&mut self, w: &mut MbWork) {
        let aw = self.mb_w * 16;
        let cw = aw / 2;
        let (dls, dlo) = if w.interlaced_dct {
            (aw * 2, aw)
        } else {
            (aw, aw * 8)
        };
        let base_y = self.mb_y * 16 * aw + self.mb_x * 16;
        let base_c = self.mb_y * 8 * cw + self.mb_x * 8;
        let alt = self.alternate_scan;
        let scan: &[u8; 64] = if alt {
            &ALTERNATE_VERTICAL_SCAN
        } else {
            &ZIGZAG_DIRECT
        };
        let raster_end = if alt {
            &self.raster_end_alt
        } else {
            &self.raster_end_zz
        };
        for n in 0..6 {
            let li = w.last_index[n];
            let dcs = if n < 4 {
                self.y_dc_scale
            } else {
                self.c_dc_scale
            };
            let blk = &mut w.blocks[n];
            if self.vol.mpeg_quant {
                dsp::unquant_mpeg2_intra(
                    blk,
                    scan,
                    li,
                    self.qscale,
                    &self.vol.intra_matrix,
                    dcs,
                    false,
                );
            } else {
                let re = if li < 0 {
                    0
                } else {
                    raster_end[li as usize] as usize
                };
                dsp::unquant_h263_intra(blk, self.qscale, dcs, false, w.ac_pred, re);
            }
            let (plane, off, stride) = match n {
                0 => (0, base_y, dls),
                1 => (0, base_y + 8, dls),
                2 => (0, base_y + dlo, dls),
                3 => (0, base_y + dlo + 8, dls),
                4 => (1, base_c, cw),
                _ => (2, base_c, cw),
            };
            let dst = match plane {
                0 => &mut self.cur.y[off..],
                1 => &mut self.cur.u[off..],
                _ => &mut self.cur.v[off..],
            };
            if self.xvid_idct {
                dsp::idct_xvid_put(blk, dst, stride);
            } else {
                dsp::idct_simple_put(blk, dst, stride);
            }
            *blk = [0; 64];
        }
    }
}

/// `gmc_motion` (0, 2 or 3 warp points).
fn gmc_motion(g: &Geo, cur: &mut Pic, rf: &Pic, sp: &crate::header::SpriteState, a: i32) {
    let aw = g.aw;
    let cw = aw / 2;
    let d = |i: usize, j: usize| sp.delta[i][j] as i32;
    let off = |i: usize, j: usize| sp.offset[i][j] as i32;
    let r = (1i32 << (2 * a + 1)) - g.no_rounding as i32;
    let (mbx, mby) = (g.mb_x, g.mb_y);
    let ox = off(0, 0)
        .wrapping_add(d(0, 0).wrapping_mul(mbx * 16))
        .wrapping_add(d(0, 1).wrapping_mul(mby * 16));
    let oy = off(0, 1)
        .wrapping_add(d(1, 0).wrapping_mul(mbx * 16))
        .wrapping_add(d(1, 1).wrapping_mul(mby * 16));
    let doff = g.mb_y as usize * 16 * aw + g.mb_x as usize * 16;
    dsp::gmc(
        &mut cur.y[doff..],
        aw,
        &rf.y,
        aw,
        16,
        ox,
        oy,
        d(0, 0),
        d(0, 1),
        d(1, 0),
        d(1, 1),
        a + 1,
        r,
        g.ew,
        g.eh,
    );
    dsp::gmc(
        &mut cur.y[doff + 8..],
        aw,
        &rf.y,
        aw,
        16,
        ox.wrapping_add(d(0, 0).wrapping_mul(8)),
        oy.wrapping_add(d(1, 0).wrapping_mul(8)),
        d(0, 0),
        d(0, 1),
        d(1, 0),
        d(1, 1),
        a + 1,
        r,
        g.ew,
        g.eh,
    );
    let ox = off(1, 0)
        .wrapping_add(d(0, 0).wrapping_mul(mbx * 8))
        .wrapping_add(d(0, 1).wrapping_mul(mby * 8));
    let oy = off(1, 1)
        .wrapping_add(d(1, 0).wrapping_mul(mbx * 8))
        .wrapping_add(d(1, 1).wrapping_mul(mby * 8));
    let coff = g.mb_y as usize * 8 * cw + g.mb_x as usize * 8;
    for p in 0..2 {
        let plane = if p == 0 { &rf.u } else { &rf.v };
        let dst = if p == 0 { &mut cur.u } else { &mut cur.v };
        dsp::gmc(
            &mut dst[coff..],
            cw,
            plane,
            cw,
            8,
            ox,
            oy,
            d(0, 0),
            d(0, 1),
            d(1, 0),
            d(1, 1),
            a + 1,
            r,
            (g.ew + 1) >> 1,
            (g.eh + 1) >> 1,
        );
    }
}
