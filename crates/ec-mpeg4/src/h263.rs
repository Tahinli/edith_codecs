//! H.263 baseline (short video header) picture and macroblock decoding.

use ec_core::error::{Error, Result};
use ec_core::timebase::Timestamp;

use crate::bits::Br;
use crate::core::*;
use crate::header::Pt;
use crate::mb::MbEnd;
use crate::tables::*;
use crate::vlc::vlcs;

fn unsupported(what: &str) -> Error {
    Error::unsupported("H.263", what.to_string())
}

fn corrupt(s: &'static str) -> Error {
    Error::corrupt(s)
}

impl Core {
    pub fn decode_h263_packet(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        let mut br = Br::new(data);
        // picture start code (22 bits), searching like FFmpeg does
        br.align();
        let mut startcode = br.get(14);
        let mut i = br.left();
        while i > 24 {
            startcode = ((startcode << 8) | br.get(8)) & 0x003F_FFFF;
            if startcode == 0x20 {
                break;
            }
            i -= 8;
        }
        if startcode != 0x20 {
            return Err(corrupt("H.263: bad picture start code"));
        }
        br.skip(8); // temporal reference
        if br.bit() != 1 {
            return Err(corrupt("H.263: marker bit missing in PTYPE"));
        }
        if br.bit() != 0 {
            return Err(corrupt("H.263: bad H.263 id"));
        }
        br.skip(3); // split screen, document camera, freeze picture release
        let format = br.get(3) as usize;
        if format == 7 || format == 6 {
            return Err(unsupported(
                "PLUSPTYPE / H.263+ extended picture header (Annex I-T)",
            ));
        }
        let (width, height) = (
            H263_FORMAT[format][0] as usize,
            H263_FORMAT[format][1] as usize,
        );
        if width == 0 {
            return Err(corrupt("H.263: forbidden source format"));
        }
        let pt = if br.bit() == 0 { Pt::I } else { Pt::P };
        let long_vectors = br.bit() != 0;
        if br.bit() != 0 {
            return Err(unsupported("syntax-based arithmetic coding (Annex E)"));
        }
        if br.bit() != 0 {
            return Err(unsupported("advanced prediction mode / OBMC (Annex F)"));
        }
        if br.bit() != 0 {
            return Err(unsupported("PB-frames (Annex G)"));
        }
        let q = br.get(5) as i32;
        br.skip(1);
        if width * height / 256 / 8 > br.left().max(0) as usize {
            return Err(corrupt("H.263: picture too short"));
        }
        // PEI / PSPARE
        if br.left() <= 0 {
            return Err(corrupt("H.263: truncated header"));
        }
        while br.bit() != 0 {
            br.skip(8);
            if br.left() <= 0 {
                return Err(corrupt("H.263: truncated header"));
            }
        }
        self.is_h263 = true;
        self.have_vol = true;
        self.h263_long_vectors = long_vectors;
        self.vol.progressive_sequence = true;
        self.vol.quarter_sample = false;
        self.vol.mpeg_quant = false;
        self.vol.low_delay = true;
        self.vol.width = width as u32;
        self.vol.height = height as u32;
        if width != self.w || height != self.h || self.mb_num == 0 {
            self.set_size(width, height);
        }
        self.gob_index = if height <= 400 {
            1
        } else if height <= 800 {
            2
        } else {
            4
        };
        self.pt = pt;
        self.no_rounding = false;
        self.alternate_scan = false;
        self.f_code = 1;
        self.b_code = 1;
        self.set_qscale(q);
        self.picture_number += 1;
        self.bug_edge = false;
        self.bug_hpel_chroma = false;
        self.bug_qpel_chroma = false;
        self.bug_qpel_chroma2 = false;
        self.bug_xvid_ilace = false;
        self.xvid_idct = self.force_xvid_idct.unwrap_or(false);
        self.decode_picture(&mut br, pts, pt).map(|_| ())
    }

    pub fn gob_header(&mut self, br: &mut Br) -> Result<()> {
        if br.peek(16) != 0 {
            return Err(corrupt("H.263: no GOB start code"));
        }
        br.skip(16);
        let mut left = br.left().min(32);
        while left > 13 {
            if br.bit() != 0 {
                break;
            }
            left -= 1;
        }
        if left <= 13 {
            return Err(corrupt("H.263: GOB header truncated"));
        }
        let gn = br.get(5) as usize;
        self.mb_x = 0;
        self.mb_y = self.gob_index * gn;
        br.skip(2);
        self.qscale = br.get(5) as i32;
        if self.mb_y >= self.mb_h || self.qscale == 0 {
            return Err(corrupt("H.263: bad GOB header"));
        }
        Ok(())
    }

    fn h263_block(
        &mut self,
        br: &mut Br,
        w: &mut MbWork,
        n: usize,
        coded: bool,
        intra: bool,
    ) -> Result<i32> {
        let rl = &vlcs().rl_inter;
        let mut i: i32;
        if intra {
            let mut level = br.get(8) as i32;
            if level == 255 {
                level = 128;
            }
            w.blocks[n][0] = level as i16;
            i = 1;
        } else {
            i = 0;
        }
        if !coded {
            return Ok(i - 1);
        }
        i -= 1;
        let blk = &mut w.blocks[n];
        loop {
            let sym = rl
                .vlc
                .get(br)
                .ok_or_else(|| corrupt("H.263 illegal ac vlc"))? as usize;
            let (mut level, run, last);
            if sym == rl.n {
                last = br.bit() != 0;
                run = br.get(6) as i32;
                level = (br.get(8) as u8 as i8) as i32;
                if level == -128 {
                    let lo = br.get(5) as i32;
                    let hi = ((br.get(6) as i32) << 26) >> 26;
                    level = lo + hi * 32;
                }
            } else {
                last = sym >= rl.last;
                run = rl.run[sym] as i32;
                level = rl.level[sym] as i32;
                if br.bit() != 0 {
                    level = -level;
                }
            }
            i += run + 1;
            if i >= 64 {
                return Err(corrupt("H.263 run overflow"));
            }
            blk[ZIGZAG_DIRECT[i as usize] as usize] = level as i16;
            if last {
                break;
            }
            if br.left() < 0 {
                return Err(corrupt("H.263 block overread"));
            }
        }
        Ok(i)
    }

    pub fn decode_mb_h263(&mut self, br: &mut Br, w: &mut MbWork) -> Result<MbEnd> {
        let v = vlcs();
        const QUANT_TAB: [i32; 4] = [-1, -2, 1, 2];
        let xy = self.mb_x + self.mb_y * self.mb_w;
        w.intra = false;
        w.mcsel = false;
        w.skipped = false;
        w.ac_pred = false;
        w.interlaced_dct = false;
        w.mvt = Mvt::M16;
        w.mv_dir = 1;
        w.last_index = [-1; 6];
        let mut cbp: u32;
        let mut cbpc: u32;
        let dquant: bool;
        let mut kind = 0u8;
        self.cur.mbskip[xy] = 0;

        let intra_path = if self.pt == Pt::P {
            loop {
                if br.bit() != 0 {
                    self.cur.mbskip[xy] = 1;
                    w.skipped = true;
                    w.mv[0][0] = [0, 0];
                    self.cur.mb_kind[xy] = 0;
                    self.qscale_table[xy] = self.qscale as i8;
                    return self.h263_end(br);
                }
                cbpc = v
                    .inter_mcbpc
                    .get(br)
                    .ok_or_else(|| corrupt("H.263 cbpc damaged"))?;
                if cbpc != 20 {
                    break;
                }
            }
            dquant = cbpc & 8 != 0;
            w.intra = cbpc & 4 != 0;
            w.intra
        } else {
            loop {
                cbpc = v
                    .intra_mcbpc
                    .get(br)
                    .ok_or_else(|| corrupt("H.263 I cbpc damaged"))?;
                if cbpc != 8 {
                    break;
                }
            }
            dquant = cbpc & 4 != 0;
            w.intra = true;
            true
        };
        if intra_path {
            kind = K_INTRA;
            let cbpy = v
                .cbpy
                .get(br)
                .ok_or_else(|| corrupt("H.263 I cbpy damaged"))?;
            cbp = (cbpc & 3) | (cbpy << 2);
            if dquant {
                let q = self.qscale + QUANT_TAB[br.get(2) as usize];
                self.set_qscale(q);
            }
        } else {
            let cbpy = v
                .cbpy
                .get(br)
                .ok_or_else(|| corrupt("H.263 cbpy damaged"))?
                ^ 0xF;
            cbp = (cbpc & 3) | (cbpy << 2);
            if dquant {
                let q = self.qscale + QUANT_TAB[br.get(2) as usize];
                self.set_qscale(q);
            }
            if cbpc & 16 == 0 {
                w.mvt = Mvt::M16;
                let (px, py) = self.pred_motion(0);
                let mx = self.decode_motion(br, px, 1)?;
                let my = self.decode_motion(br, py, 1)?;
                w.mv[0][0] = [mx, my];
            } else {
                kind = K_8X8;
                w.mvt = Mvt::M8;
                for i in 0..4 {
                    let (px, py) = self.pred_motion(i);
                    let mx = self.decode_motion(br, px, 1)?;
                    let my = self.decode_motion(br, py, 1)?;
                    w.mv[0][i] = [mx, my];
                    let bx = 2 * self.mb_x as isize + (i & 1) as isize;
                    let by = 2 * self.mb_y as isize + (i >> 1) as isize;
                    let ci = self.mvi(bx, by);
                    self.cur.mv[ci] = [mx as i16, my as i16];
                }
            }
        }
        self.cur.mb_kind[xy] = kind;
        self.qscale_table[xy] = self.qscale as i8;
        for i in 0..6 {
            let li = self.h263_block(br, w, i, cbp & 32 != 0, w.intra)?;
            w.last_index[i] = li;
            cbp += cbp;
        }
        self.h263_end(br)
    }

    fn h263_end(&mut self, br: &mut Br) -> Result<MbEnd> {
        if br.left() < 0 {
            return Err(corrupt("H.263 overread"));
        }
        let mut v = br.peek(16);
        let left = br.left();
        if left < 16 {
            v >>= (16 - left) as u32;
        }
        if v == 0 {
            Ok(MbEnd::SliceEnd)
        } else {
            Ok(MbEnd::Ok)
        }
    }
}
