//! Decoder state: pictures, per-frame tables and the picture-level flow.

use std::collections::VecDeque;
use std::sync::Arc;

use ec_core::error::Result;
use ec_core::frame::{ColorInfo, Interlace, PixelFormat, Plane, VideoFrame};
use ec_core::timebase::Timestamp;

use crate::bits::Br;
use crate::header::*;
use crate::tables::*;

pub const K_INTRA: u8 = 1;
pub const K_8X8: u8 = 2;
pub const K_FIELD: u8 = 4;
pub const K_GMC: u8 = 8;

#[derive(Clone)]
pub struct Pic {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub mv: Vec<[i16; 2]>,
    pub mb_kind: Vec<u8>,
    pub mbskip: Vec<u8>,
    pub field_mv: Vec<[[i16; 2]; 2]>,
    pub field_sel: Vec<[u8; 2]>,
    pub pts: Option<Timestamp>,
    pub interlaced: bool,
    pub tff: bool,
    pub keyframe: bool,
    pub pict: Option<Pt>,
}

impl Pic {
    pub fn new(mb_w: usize, mb_h: usize) -> Pic {
        let (aw, ah) = (mb_w * 16, mb_h * 16);
        Pic {
            y: vec![0; aw * ah],
            u: vec![0; aw * ah / 4],
            v: vec![0; aw * ah / 4],
            mv: vec![[0; 2]; (2 * mb_w + 1) * (2 * mb_h + 1) + 8],
            mb_kind: vec![0; mb_w * mb_h],
            mbskip: vec![0; mb_w * mb_h],
            field_mv: vec![[[0; 2]; 2]; mb_w * mb_h],
            field_sel: vec![[0; 2]; mb_w * mb_h],
            pts: None,
            interlaced: false,
            tff: false,
            keyframe: false,
            pict: None,
        }
    }

    pub fn gray(mb_w: usize, mb_h: usize) -> Pic {
        let mut p = Pic::new(mb_w, mb_h);
        p.y.fill(0x80);
        p.u.fill(0x80);
        p.v.fill(0x80);
        p
    }
}

/// A decoded picture ready for the caller, with the bits `VideoFrame` can't carry.
pub struct Output {
    pub frame: VideoFrame,
    pub info: PictureInfo,
}

/// Facts about a decoded picture that [`VideoFrame`] cannot carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PictureInfo {
    /// `I`, `P`, `B` or `S` (GMC) coding type.
    pub pict_type: char,
    /// The picture was partly or wholly concealed after a bitstream error.
    pub damaged: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mvt {
    M16,
    M8,
    Field,
}

pub struct MbWork {
    pub intra: bool,
    pub mv_dir: u8,
    pub mvt: Mvt,
    pub mv: [[[i32; 2]; 4]; 2],
    pub field_select: [[u8; 2]; 2],
    pub mcsel: bool,
    pub interlaced_dct: bool,
    pub skipped: bool,
    pub ac_pred: bool,
    pub blocks: [[i16; 64]; 6],
    pub last_index: [i32; 6],
}

impl MbWork {
    pub fn new() -> MbWork {
        MbWork {
            intra: false,
            mv_dir: 1,
            mvt: Mvt::M16,
            mv: [[[0; 2]; 4]; 2],
            field_select: [[0; 2]; 2],
            mcsel: false,
            interlaced_dct: false,
            skipped: false,
            ac_pred: false,
            blocks: [[0; 64]; 6],
            last_index: [-1; 6],
        }
    }
}

pub struct Core {
    pub ident: Ident,
    pub vol: Vol,
    pub have_vol: bool,
    pub is_h263: bool,
    pub h263_long_vectors: bool,
    pub gob_index: usize,
    pub ts: TimeState,
    pub picture_number: u64,
    pub sprite: SpriteState,
    // geometry
    pub w: usize,
    pub h: usize,
    pub mb_w: usize,
    pub mb_h: usize,
    pub mb_num: usize,
    pub b8s: usize,
    pub mvoff: usize,
    // references and output
    pub last: Option<Arc<Pic>>,
    pub next: Option<Arc<Pic>>,
    pub cur: Pic,
    pub out: VecDeque<Output>,
    /// Remainder of a packed packet (DivX packed B-frames), decoded with the next packet.
    pub stash: Option<Vec<u8>>,
    /// The last VOP seen was a not-coded VOP (N-VOP).
    pub skipped_last_frame: bool,
    pub last_pts: Option<Timestamp>,
    pub dummy_last: bool,
    pub done: Vec<bool>,
    pub last_resync_pos: usize,
    pub raster_end_zz: [u8; 64],
    pub raster_end_alt: [u8; 64],
    pub tag: [u8; 4],
    // picture state
    pub pt: Pt,
    pub qscale: i32,
    pub f_code: i32,
    pub b_code: i32,
    pub no_rounding: bool,
    pub alternate_scan: bool,
    pub intra_dc_threshold: i32,
    pub top_field_first: bool,
    pub partitioned: bool,
    pub y_dc_scale: i32,
    pub c_dc_scale: i32,
    // macroblock position / slice
    pub mb_x: usize,
    pub mb_y: usize,
    pub resync_mb_x: usize,
    pub resync_mb_y: usize,
    pub first_slice_line: bool,
    pub damaged: bool,
    // prediction tables
    pub w8: usize,
    pub wc: usize,
    pub dc: [Vec<i16>; 3],
    pub ac: [Vec<[i16; 16]>; 3],
    pub qscale_table: Vec<i8>,
    pub cbp_table: Vec<u8>,
    pub pred_dir_table: Vec<u8>,
    pub mb_type_tab: Vec<u8>,
    pub mb_acpred: Vec<u8>,
    pub last_mv: [[[i32; 2]; 2]; 2],
    pub direct_scale_mv: [[i16; 64]; 2],
    // bug workarounds
    pub bug_qpel_chroma: bool,
    pub bug_qpel_chroma2: bool,
    pub bug_hpel_chroma: bool,
    pub bug_dc_clip: bool,
    pub bug_direct_blocksize: bool,
    pub bug_edge: bool,
    pub bug_ump4: bool,
    pub bug_xvid_ilace: bool,
    pub xvid_idct: bool,
    pub force_xvid_idct: Option<bool>,
    /// MS-MPEG4 version (2 or 3), 0 for MPEG-4 / H.263.
    pub ms_version: u8,
    pub ms: crate::msmpeg4::MsState,
}

impl Core {
    pub fn new() -> Core {
        Core {
            ident: Ident::default(),
            vol: Vol::default(),
            have_vol: false,
            is_h263: false,
            h263_long_vectors: false,
            gob_index: 1,
            ts: TimeState::default(),
            picture_number: 0,
            sprite: SpriteState::default(),
            w: 0,
            h: 0,
            mb_w: 0,
            mb_h: 0,
            mb_num: 0,
            b8s: 0,
            mvoff: 0,
            last: None,
            next: None,
            cur: Pic::new(0, 0),
            out: VecDeque::new(),
            stash: None,
            skipped_last_frame: false,
            last_pts: None,
            dummy_last: false,
            done: Vec::new(),
            last_resync_pos: 0,
            raster_end_zz: raster_end(&ZIGZAG_DIRECT),
            raster_end_alt: raster_end(&ALTERNATE_VERTICAL_SCAN),
            tag: [0; 4],
            pt: Pt::I,
            qscale: 1,
            f_code: 1,
            b_code: 1,
            no_rounding: false,
            alternate_scan: false,
            intra_dc_threshold: 0,
            top_field_first: false,
            partitioned: false,
            y_dc_scale: 8,
            c_dc_scale: 8,
            mb_x: 0,
            mb_y: 0,
            resync_mb_x: 0,
            resync_mb_y: 0,
            first_slice_line: true,
            damaged: false,
            w8: 0,
            wc: 0,
            dc: [Vec::new(), Vec::new(), Vec::new()],
            ac: [Vec::new(), Vec::new(), Vec::new()],
            qscale_table: Vec::new(),
            cbp_table: Vec::new(),
            pred_dir_table: Vec::new(),
            mb_type_tab: Vec::new(),
            mb_acpred: Vec::new(),
            last_mv: [[[0; 2]; 2]; 2],
            direct_scale_mv: [[0; 64]; 2],
            bug_qpel_chroma: false,
            bug_qpel_chroma2: false,
            bug_hpel_chroma: false,
            bug_dc_clip: false,
            bug_direct_blocksize: false,
            bug_edge: false,
            bug_ump4: false,
            bug_xvid_ilace: false,
            xvid_idct: false,
            force_xvid_idct: None,
            ms_version: 0,
            ms: Default::default(),
        }
    }

    pub fn reset(&mut self) {
        self.last = None;
        self.next = None;
        self.out.clear();
        self.stash = None;
        self.skipped_last_frame = false;
        self.ts = TimeState::default();
    }

    /// Set up geometry-dependent buffers for a new picture size.
    pub fn set_size(&mut self, w: usize, h: usize) {
        self.w = w;
        self.h = h;
        self.mb_w = w.div_ceil(16);
        self.mb_h = h.div_ceil(16);
        self.mb_num = self.mb_w * self.mb_h;
        self.b8s = 2 * self.mb_w + 1;
        self.mvoff = self.b8s + 1;
        self.done = vec![false; self.mb_num];
        self.w8 = 2 * self.mb_w + 1;
        self.wc = self.mb_w + 1;
        self.last = None;
        self.next = None;
        let ny = self.w8 * (2 * self.mb_h + 1) + 2;
        let nc = self.wc * (self.mb_h + 1) + 2;
        self.dc = [vec![1024; ny], vec![1024; nc], vec![1024; nc]];
        self.ac = [vec![[0; 16]; ny], vec![[0; 16]; nc], vec![[0; 16]; nc]];
        self.qscale_table = vec![0; self.mb_num];
        self.cbp_table = vec![0; self.mb_num];
        self.pred_dir_table = vec![0; self.mb_num];
        self.mb_type_tab = vec![0; self.mb_num];
        self.mb_acpred = vec![0; self.mb_num];
        self.ms.coded = vec![0; ny];
    }

    pub fn h263_pred(&self) -> bool {
        !self.is_h263
    }

    pub fn set_qscale(&mut self, q: i32) {
        let q = q.clamp(1, 31);
        self.qscale = q;
        if self.ms_version != 0 {
            if self.ms_version == 3 {
                self.y_dc_scale = crate::msdata::OLD_FF_Y_DC_SCALE[q as usize] as i32;
                self.c_dc_scale = crate::msdata::WMV1_C_DC_SCALE[q as usize] as i32;
            } else {
                self.y_dc_scale = 8;
                self.c_dc_scale = 8;
            }
        } else if self.is_h263 {
            // The H.263 decoder keeps the MPEG-1 DC scale (8) for intra DC.
            self.y_dc_scale = 8;
            self.c_dc_scale = 8;
        } else {
            self.y_dc_scale = MPEG4_Y_DC_SCALE_TABLE[q as usize] as i32;
            self.c_dc_scale = MPEG4_C_DC_SCALE_TABLE[q as usize] as i32;
        }
    }

    /// Index of luma block cell (bx, by) in `dc[0]`/`ac[0]`.
    #[inline]
    pub fn yidx(&self, bx: usize, by: usize) -> usize {
        (by + 1) * self.w8 + bx + 1
    }

    #[inline]
    pub fn cidx(&self, x: usize, y: usize) -> usize {
        (y + 1) * self.wc + x + 1
    }

    /// Reset DC/AC predictor tables to their "unavailable" state.
    pub fn reset_pred_tables(&mut self) {
        for d in &mut self.dc {
            d.fill(1024);
        }
        for a in &mut self.ac {
            a.fill([0; 16]);
        }
    }

    /// `ff_mpeg4_clean_buffers`
    pub fn clean_buffers(&mut self) {
        let (mx, my) = (self.mb_x as isize, self.mb_y as isize);
        let w8 = self.w8 as isize;
        // luma: cells (2mx-1, 2my-1) .. + (2*w8+1)
        let start = (2 * my - 1 + 1) * w8 + (2 * mx - 1 + 1);
        for k in 0..(2 * w8 + 1) {
            let i = (start + k) as usize;
            if i < self.ac[0].len() {
                self.ac[0][i] = [0; 16];
            }
        }
        let wc = self.wc as isize;
        for p in 1..3 {
            let start = (my - 1 + 1) * wc + (mx - 1 + 1);
            for k in 0..(wc + 1) {
                let i = (start + k) as usize;
                if i < self.ac[p].len() {
                    self.ac[p][i] = [0; 16];
                }
            }
        }
        // Only the frame MVs are cleared, not the second (field) entries: ffmpeg's quirk.
        self.last_mv[0][0] = [0; 2];
        self.last_mv[1][0] = [0; 2];
    }

    pub fn init_direct_mv(&mut self) {
        let pb = self.ts.pb_time as i32;
        let pp = self.ts.pp_time as i32;
        for i in 0..64 {
            let v = i as i32 - 32;
            self.direct_scale_mv[0][i] = (v * pb / pp) as i16;
            self.direct_scale_mv[1][i] = (v * (pb - pp) / pp) as i16;
        }
    }

    // ------------------------------------------------------------------
    // Output

    pub fn make_frame(&self, p: &Pic) -> VideoFrame {
        let (w, h) = (self.w, self.h);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let aw = self.mb_w * 16;
        let crop = |src: &[u8], stride: usize, w: usize, h: usize| -> Plane {
            let mut d = Vec::with_capacity(w * h);
            for r in 0..h {
                d.extend_from_slice(&src[r * stride..r * stride + w]);
            }
            Plane::new(d, w)
        };
        let planes = vec![
            crop(&p.y, aw, w, h),
            crop(&p.u, aw / 2, cw, ch),
            crop(&p.v, aw / 2, cw, ch),
        ];
        let interlace = if p.interlaced {
            if p.tff {
                Interlace::TopFieldFirst
            } else {
                Interlace::BottomFieldFirst
            }
        } else {
            Interlace::Progressive
        };
        VideoFrame {
            format: PixelFormat::I420,
            width: w as u32,
            height: h as u32,
            planes,
            color: ColorInfo::default(),
            pts: p.pts,
            interlace,
        }
    }

    pub fn push_out(&mut self, p: &Pic) {
        let frame = self.make_frame(p);
        let pict_type = match p.pict {
            Some(Pt::I) => 'I',
            Some(Pt::P) => 'P',
            Some(Pt::B) => 'B',
            Some(Pt::S) => 'S',
            None => '?',
        };
        self.out.push_back(Output {
            frame,
            info: PictureInfo {
                pict_type,
                damaged: self.damaged,
            },
        });
    }

    /// Drain the delayed reference at end of stream.
    pub fn flush(&mut self) {
        if let Some(n) = self.next.take()
            && (!self.vol.low_delay || self.skipped_last_frame)
        {
            if self.skipped_last_frame {
                // libavcodec: a stream ending with an N-VOP outputs the held
                // picture with the timestamp of the last input packet.
                let mut c = (*n).clone();
                c.pts = self.last_pts;
                self.push_out(&c);
            } else {
                self.push_out(&n);
            }
        }
        self.last = None;
        self.stash = None;
        self.skipped_last_frame = false;
    }

    /// `ff_mpeg4_workaround_bugs`: derive the emulated-bug flags from the
    /// stream identification (user data and optional container FourCC).
    pub fn finish_workaround_flags(&mut self) {
        let tag = &self.tag;
        let is = |t: &[u8; 4]| tag == t;
        let id = &mut self.ident;
        if id.xvid_build == -1
            && id.divx_version == -1
            && id.lavc_build == -1
            && (is(b"XVID") || is(b"XVIX") || is(b"RMP4") || is(b"ZMP4") || is(b"SIPP"))
        {
            id.xvid_build = 0;
        }
        if id.xvid_build == -1
            && id.divx_version == -1
            && id.lavc_build == -1
            && is(b"DIVX")
            && self.vol.vo_type == 0
            && !self.vol.vol_control_parameters
        {
            id.divx_version = 400;
        }
        if id.xvid_build >= 0 && id.divx_version >= 0 {
            id.divx_version = -1;
            id.divx_build = -1;
        }
        let id = self.ident;
        self.bug_xvid_ilace = self.tag == *b"XVIX";
        self.bug_ump4 = self.tag == *b"UMP4";
        self.bug_qpel_chroma = false;
        self.bug_qpel_chroma2 = false;
        self.bug_hpel_chroma = false;
        self.bug_dc_clip = false;
        self.bug_direct_blocksize = false;
        self.bug_edge = false;
        if id.divx_version >= 500 && id.divx_build < 1814 {
            self.bug_qpel_chroma = true;
        }
        if id.divx_version > 502 && id.divx_build < 1814 {
            self.bug_qpel_chroma2 = true;
        }
        let xb = id.xvid_build as u32;
        if xb <= 1 {
            self.bug_qpel_chroma = true;
        }
        if xb <= 12 {
            self.bug_edge = true;
        }
        if xb <= 32 {
            self.bug_dc_clip = true;
        }
        let lb = id.lavc_build as u32;
        if lb < 4655 {
            self.bug_direct_blocksize = true;
        }
        if lb < 4670 {
            self.bug_edge = true;
        }
        if lb <= 4712 {
            self.bug_dc_clip = true;
        }
        if id.divx_version >= 0 {
            self.bug_direct_blocksize = true;
            self.bug_hpel_chroma = true;
        }
        if (id.divx_version as u32) < 500 {
            self.bug_edge = true;
        }
        if id.xvid_build >= 0 {
            self.xvid_idct = true;
        }
        if let Some(f) = self.force_xvid_idct {
            self.xvid_idct = f;
        }
    }

    // ------------------------------------------------------------------
    // Packet-level flow

    /// Decode all pictures found in `data`.
    pub fn decode_packet(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        if data.len() >= 3 && data[0] == 0 && data[1] == 0 && data[2] & 0xFC == 0x80 {
            return self.decode_h263_packet(data, pts);
        }
        self.is_h263 = false;
        self.decode_mpeg4_packet(data, pts)
    }

    /// Parse headers in `data` (decoder configuration): VOL and friends only.
    pub fn parse_config(&mut self, data: &[u8]) -> Result<()> {
        let mut br = Br::new(data);
        self.scan_headers(&mut br, true).map(|_| ())
    }
}

fn raster_end(scan: &[u8; 64]) -> [u8; 64] {
    let mut out = [0u8; 64];
    let mut m = 0;
    for i in 0..64 {
        m = m.max(scan[i]);
        out[i] = m;
    }
    out
}
