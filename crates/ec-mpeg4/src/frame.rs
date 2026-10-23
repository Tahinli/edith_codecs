//! VOP-level decoding: start-code scan, header handling, slice loop, resync,
//! reference management and output order.

use std::sync::Arc;

use ec_core::error::{Error, Result};
use ec_core::timebase::Timestamp;

use crate::bits::Br;
use crate::core::*;
use crate::header::*;

/// FFmpeg's MAX_NVOP_SIZE.
const MAX_NVOP_SIZE: usize = 19;

fn av_log2(x: usize) -> u32 {
    if x == 0 {
        0
    } else {
        usize::BITS - 1 - x.leading_zeros()
    }
}

pub enum Scanned {
    Vop,
    NoVop,
}

impl Core {
    /// Search for headers and the next VOP start code (`ff_mpeg4_parse_picture_header`).
    pub fn scan_headers(&mut self, br: &mut Br, header_only: bool) -> Result<Scanned> {
        br.align();
        let mut startcode: u32 = 0xff;
        let mut vol_seen = false;
        loop {
            if br.pos() >= br.size_bits() {
                if header_only {
                    return Ok(Scanned::NoVop);
                }
                if br.size_bits() == 8
                    && (self.ident.divx_version >= 0 || self.ident.xvid_build >= 0)
                {
                    return Ok(Scanned::NoVop);
                }
                return Err(Error::corrupt("MPEG-4: no VOP start code"));
            }
            let v = br.get(8);
            startcode = (startcode << 8) | v;
            if startcode & 0xFFFF_FF00 != 0x100 {
                continue;
            }
            if (0x120..=0x12F).contains(&startcode) {
                if vol_seen {
                    continue;
                }
                vol_seen = true;
                let old = (self.vol.width, self.vol.height);
                parse_vol(br, &mut self.vol, self.picture_number)?;
                self.have_vol = true;
                self.is_h263 = false;
                let _ = old;
            } else if startcode == 0x1B2 {
                let mut buf = Vec::new();
                while buf.len() < 256 && br.left() >= 24 && br.peek(24) != 1 {
                    buf.push(br.get(8) as u8);
                }
                self.ident.parse_user_data(&buf);
            } else if startcode == 0x1B6 {
                return Ok(Scanned::Vop);
            }
            br.align();
            startcode = 0xff;
        }
    }

    pub fn decode_mpeg4_packet(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        self.last_pts = pts;
        self.skipped_last_frame = false;
        // A stashed packed B-VOP replaces the (placeholder) packet that follows it.
        let mut stash = self.stash.take();
        if let Some(st) = stash.as_ref() {
            let mut drop = false;
            if self.ident.divx_packed
                && let Some(i) = data
                    .windows(4)
                    .position(|w| w[0] == 0 && w[1] == 0 && w[2] == 1)
                && data[i + 3] == 0xB0
            {
                drop = true;
            }
            if drop || !(self.ident.divx_packed || data.len() <= MAX_NVOP_SIZE) || st.is_empty() {
                stash = None;
            }
        }
        let from_stash = stash.is_some();
        let chunk: &[u8] = stash.as_deref().unwrap_or(data);
        let used = self.decode_one_vop(chunk, pts)?;
        // FFmpeg's packed-bitstream rule: a following I/B VOP in the same packet
        // is kept and decoded in place of the next (placeholder) packet.
        let cur = if from_stash {
            0
        } else {
            used.unwrap_or(data.len()).min(data.len())
        };
        if data.len() - cur > 7 {
            let mut i = cur;
            while i + 4 < data.len() {
                if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && data[i + 3] == 0xB6 {
                    if data[i + 4] & 0x40 == 0 {
                        self.stash = Some(data[cur..].to_vec());
                    }
                    break;
                }
                i += 1;
            }
        }
        Ok(())
    }

    fn update_workarounds(&mut self) {
        self.finish_workaround_flags();
    }

    /// Decode one VOP from `data`. Returns the byte position reached.
    fn decode_one_vop(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<Option<usize>> {
        let mut br = Br::new(data);
        if let Scanned::NoVop = self.scan_headers(&mut br, false)? {
            return Ok(None);
        }
        if !self.have_vol {
            return Err(Error::corrupt("MPEG-4: VOP before any VOL header"));
        }
        if self.vol.data_partitioning {
            return Err(Error::unsupported(
                "MPEG-4 Part 2",
                "data partitioning (and reversible VLC) is not implemented".to_string(),
            ));
        }
        if self.vol.rvlc {
            return Err(Error::unsupported(
                "MPEG-4 Part 2",
                "reversible VLC is not implemented".to_string(),
            ));
        }
        let (vw, vh) = (self.vol.width as usize, self.vol.height as usize);
        if vw == 0 || vh == 0 {
            return Err(Error::corrupt("MPEG-4: VOL without picture size"));
        }
        if vw != self.w || vh != self.h || self.mb_num == 0 {
            self.set_size(vw, vh);
            self.out_flush_on_resize();
        }
        let mut vop = Vop::default();
        let r = parse_vop(
            &mut br,
            &mut self.vol,
            &mut self.ts,
            &mut vop,
            &self.ident,
            self.bug_ump4,
            self.picture_number,
        )?;
        match r {
            VopResult::Skipped { not_coded: false } => return Ok(Some(br.pos() >> 3)),
            VopResult::Skipped { not_coded: true } => {
                self.nvop();
                return Ok(Some(br.pos() >> 3));
            }
            VopResult::Coded => {}
        }
        let pt = vop.pict_type.unwrap();
        self.sprite.width = self.w as i32;
        self.sprite.height = self.h as i32;
        let sp_ok = pt == Pt::S && self.vol.sprite_usage == 2;
        if pt == Pt::S && !sp_ok {
            self.sprite = SpriteState {
                width: self.w as i32,
                height: self.h as i32,
                ..SpriteState::default()
            };
        }
        parse_vop_tail(
            &mut br,
            &self.vol,
            &mut vop,
            if sp_ok { Some(&mut self.sprite) } else { None },
            &self.ident,
        )?;
        self.pt = pt;
        self.no_rounding = vop.no_rounding;
        self.alternate_scan = vop.alternate_scan;
        self.intra_dc_threshold = vop.intra_dc_threshold;
        self.top_field_first = vop.top_field_first;
        self.f_code = vop.f_code;
        self.b_code = vop.b_code;
        self.set_qscale(vop.qscale);
        self.partitioned = false;
        if self.vol.vo_type == 0
            && !self.vol.vol_control_parameters
            && self.ident.divx_version == -1
            && self.picture_number == 0
        {
            self.vol.low_delay = true;
        }
        self.picture_number += 1;
        if pt == Pt::S && self.vol.sprite_usage != 2 {
            return Err(Error::unsupported(
                "MPEG-4 Part 2",
                "static sprite (S-VOP without GMC)".to_string(),
            ));
        }
        self.update_workarounds();
        if pt != Pt::B && self.mb_num / 2 > br.left().max(0) as usize {
            return Err(Error::corrupt("MPEG-4: VOP too short"));
        }
        if pt == Pt::B {
            if self.last.is_none() {
                return Ok(Some(br.pos() >> 3));
            }
            self.init_direct_mv();
        }
        self.decode_picture(&mut br, pts, pt).map(Some)
    }

    /// Everything after the picture header: reference handling, slices, output.
    pub fn decode_picture(&mut self, br: &mut Br, pts: Option<Timestamp>, pt: Pt) -> Result<usize> {
        // frame start
        let mut cur = Pic::new(self.mb_w, self.mb_h);
        cur.pts = pts;
        cur.interlaced = !self.vol.progressive_sequence;
        cur.tff = self.top_field_first;
        cur.pict = Some(pt);
        cur.keyframe = pt == Pt::I;
        if pt != Pt::B {
            self.last = self.next.take();
            if self.last.is_none() && pt != Pt::I {
                self.last = Some(Arc::new(Pic::gray(self.mb_w, self.mb_h)));
                self.dummy_last = true;
            }
        } else if self.next.is_none() {
            return Ok(br.pos() >> 3);
        }
        self.cur = cur;
        self.reset_pred_tables();
        self.done.iter_mut().for_each(|d| *d = false);
        self.damaged = false;

        let mut w = Box::new(MbWork::new());
        self.mb_x = 0;
        self.mb_y = 0;
        let mut res = self.decode_slice(br, &mut w);
        while self.mb_y < self.mb_h {
            let (px, py) = (self.mb_x, self.mb_y);
            if !self.resync(br) {
                break;
            }
            if py * self.mb_w + px < self.mb_y * self.mb_w + self.mb_x {
                self.damaged = true;
            }
            if !self.is_h263 {
                self.clean_buffers();
            }
            if self.decode_slice(br, &mut w).is_err() {
                res = Err(Error::corrupt("MPEG-4 slice"));
            }
        }
        if res.is_err() {
            self.damaged = true;
        }
        let end_pos = br.pos() >> 3;
        self.conceal();
        self.frame_end(pt);
        Ok(end_pos)
    }

    fn out_flush_on_resize(&mut self) {
        // A new picture size starts a fresh reference chain; pending output is delivered.
        if let Some(n) = self.next.take()
            && !self.vol.low_delay
        {
            self.push_out(&n);
        }
        self.last = None;
    }

    /// Handle a not-coded VOP (N-VOP): like libavcodec, no picture is produced; the
    /// previous picture simply stays on screen for this packet's duration.
    fn nvop(&mut self) {
        self.skipped_last_frame = true;
    }

    pub(crate) fn frame_end(&mut self, pt: Pt) {
        let cur = std::mem::replace(&mut self.cur, Pic::new(0, 0));
        let cur = Arc::new(cur);
        if pt == Pt::B || self.vol.low_delay {
            self.push_out(&cur);
        } else if let Some(l) = self.last.clone() {
            self.push_out(&l);
        }
        if pt != Pt::B {
            self.next = Some(cur);
        }
        self.dummy_last = false;
    }

    /// Fill macroblocks that were not decoded (corrupt data) from the reference.
    pub(crate) fn conceal(&mut self) {
        if self.done.iter().all(|&d| d) {
            return;
        }
        self.damaged = true;
        let rf = self.last.clone().or_else(|| self.next.clone());
        let aw = self.mb_w * 16;
        let cw = aw / 2;
        for mby in 0..self.mb_h {
            for mbx in 0..self.mb_w {
                if self.done[mby * self.mb_w + mbx] {
                    continue;
                }
                for r in 0..16 {
                    let o = (mby * 16 + r) * aw + mbx * 16;
                    match &rf {
                        Some(p) => self.cur.y[o..o + 16].copy_from_slice(&p.y[o..o + 16]),
                        None => self.cur.y[o..o + 16].fill(0x80),
                    }
                }
                for r in 0..8 {
                    let o = (mby * 8 + r) * cw + mbx * 8;
                    match &rf {
                        Some(p) => {
                            self.cur.u[o..o + 8].copy_from_slice(&p.u[o..o + 8]);
                            self.cur.v[o..o + 8].copy_from_slice(&p.v[o..o + 8]);
                        }
                        None => {
                            self.cur.u[o..o + 8].fill(0x80);
                            self.cur.v[o..o + 8].fill(0x80);
                        }
                    }
                }
            }
        }
    }

    /// One slice (`decode_slice`): MBs until a resync marker, an error or the picture end.
    fn decode_slice(&mut self, br: &mut Br, w: &mut MbWork) -> Result<()> {
        self.last_resync_pos = br.pos();
        self.first_slice_line = true;
        self.resync_mb_x = self.mb_x;
        self.resync_mb_y = self.mb_y;
        let q = self.qscale;
        self.set_qscale(q);
        while self.mb_y < self.mb_h {
            while self.mb_x < self.mb_w {
                if self.resync_mb_x == self.mb_x && self.resync_mb_y + 1 == self.mb_y {
                    self.first_slice_line = false;
                }
                let r = self.decode_mb_any(br, w);
                match r {
                    Err(e) => {
                        for b in w.blocks.iter_mut() {
                            *b = [0; 64];
                        }
                        return Err(e);
                    }
                    Ok(end) => {
                        self.post_mb(w);
                        self.reconstruct(w);
                        let xy = self.mb_y * self.mb_w + self.mb_x;
                        self.done[xy] = true;
                        if let crate::mb::MbEnd::SliceEnd = end {
                            self.mb_x += 1;
                            if self.mb_x >= self.mb_w {
                                self.mb_x = 0;
                                self.mb_y += 1;
                            }
                            return Ok(());
                        }
                    }
                }
                self.mb_x += 1;
            }
            self.mb_x = 0;
            self.mb_y += 1;
        }
        Ok(())
    }

    fn decode_mb_any(&mut self, br: &mut Br, w: &mut MbWork) -> Result<crate::mb::MbEnd> {
        if self.is_h263 {
            self.decode_mb_h263(br, w)
        } else {
            self.decode_mb(br, w)
        }
    }

    /// `ff_h263_resync` for MPEG-4 (video packets) and H.263 (GOBs).
    fn resync(&mut self, br: &mut Br) -> bool {
        if !self.is_h263 {
            br.skip(1);
            br.align();
        }
        if br.peek(16) == 0 {
            let pos = br.pos();
            let ok = if self.is_h263 {
                self.gob_header(br).is_ok()
            } else {
                self.video_packet_header(br).is_ok()
            };
            if ok {
                return true;
            }
            br.set_pos(pos);
        }
        br.set_pos(self.last_resync_pos);
        br.align();
        let mut left = br.left();
        while left > 16 + 1 + 5 + 5 {
            if br.peek(16) == 0 {
                let bak = br.pos();
                let ok = if self.is_h263 {
                    self.gob_header(br).is_ok()
                } else {
                    self.video_packet_header(br).is_ok()
                };
                if ok {
                    return true;
                }
                br.set_pos(bak);
            }
            br.skip(8);
            left -= 8;
        }
        false
    }

    fn video_packet_header(&mut self, br: &mut Br) -> Result<()> {
        let mb_num_bits = av_log2(self.mb_num - 1) + 1;
        if br.pos() as isize > br.size_bits() as isize - 20 {
            return Err(Error::corrupt("MPEG-4 video packet header: no space"));
        }
        let mut len = 0;
        while len < 32 {
            if br.bit() != 0 {
                break;
            }
            len += 1;
        }
        if len != self.video_packet_prefix_len() {
            return Err(Error::corrupt(
                "MPEG-4 video packet marker does not match f_code",
            ));
        }
        let mb_num = br.get(mb_num_bits) as usize;
        if mb_num >= self.mb_num || mb_num == 0 {
            return Err(Error::corrupt("MPEG-4 illegal mb_num in video packet"));
        }
        self.mb_x = mb_num % self.mb_w;
        self.mb_y = mb_num / self.mb_w;
        let q = br.get(self.vol.quant_precision) as i32;
        if q != 0 {
            self.qscale = q;
        }
        let ext = br.bit() != 0;
        if ext {
            while br.bit() != 0 {
                if br.left() <= 0 {
                    return Err(Error::corrupt("MPEG-4 video packet header overread"));
                }
            }
            br.skip(1);
            br.skip(self.vol.time_increment_bits);
            br.skip(1);
            br.skip(2);
            br.skip(3);
            if self.pt == Pt::S && self.vol.sprite_usage == 2 {
                parse_sprite_trajectory(br, &self.vol, &mut self.sprite, &self.ident)?;
            }
            if self.pt != Pt::I {
                br.skip(3);
            }
            if self.pt == Pt::B {
                br.skip(3);
            }
        }
        Ok(())
    }
}
