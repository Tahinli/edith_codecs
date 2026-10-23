//! The decoding core: start-code parsing, picture management, slice and
//! macroblock decoding, reconstruction and display-order output.

use std::collections::VecDeque;

use ec_core::error::{Error, Result};
use ec_core::frame::{ColorInfo, Interlace, PixelFormat, Plane, VideoFrame};
use ec_core::timebase::Timestamp;

use crate::bits::Bits;
use crate::block::{BlockParams, decode_inter, decode_intra};
use crate::header::*;
use crate::idct::{idct_add, idct_add_dc, idct_put, idct_put_dc};
use crate::mc::{Image, predict};
use crate::tables::*;

/// Facts about a decoded picture that [`VideoFrame`] cannot carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureInfo {
    /// Coding type of the picture (of the first field for field pairs).
    pub picture_type: PictureType,
    /// temporal_reference of the picture.
    pub temporal_reference: u16,
    /// top_field_first from the picture coding extension (false for MPEG-1).
    pub top_field_first: bool,
    /// repeat_first_field: display the first field again after the second.
    pub repeat_first_field: bool,
    /// progressive_frame: the two fields come from one instant.
    pub progressive_frame: bool,
    /// progressive_sequence from the sequence extension (true for MPEG-1).
    pub progressive_sequence: bool,
    /// True when the frame was coded as two field pictures.
    pub field_pictures: bool,
    /// True for MPEG-1 syntax (no sequence extension).
    pub mpeg1: bool,
    /// True when some macroblocks were missing or damaged and were concealed.
    pub corrupt: bool,
    /// The GOP header preceding this picture had closed_gop set.
    pub closed_gop: bool,
}

struct Pic {
    planes: [Vec<u8>; 3],
    stride: [usize; 3],
    height: [usize; 3],
    meta: Option<Meta>,
    emitted: bool,
}

#[derive(Clone)]
struct Meta {
    pts: Option<Timestamp>,
    info: PictureInfo,
    interlace: Interlace,
}

/// Frame under construction (one or two pictures).
struct CurFrame {
    idx: usize,
    ptype: PictureType,
    first_structure: Structure,
    waiting_second: bool,
    pts: Option<Timestamp>,
    tr: u16,
    tff: bool,
    rff: bool,
    progressive_frame: bool,
    corrupt: bool,
    field_pictures: bool,
    closed_gop: bool,
}

/// State of the picture (frame or field) being decoded.
struct PicState {
    hdr: PictureHeader,
    mb_done: Vec<bool>,
    rows: usize,
    parity: usize,
    second_field: bool,
}

const MV_FRAME: u8 = 0;
const MV_FIELD_IN_FRAME: u8 = 1;
const MV_DUAL_FRAME: u8 = 2;
const MV_FIELD16: u8 = 3;
const MV_16X8: u8 = 4;
const MV_DUAL_FIELD: u8 = 5;

#[derive(Clone, Copy, Default)]
struct Motion {
    /// [s][r][t]
    mv: [[[i32; 2]; 2]; 2],
    sel: [[u8; 2]; 2],
    dmv: [i32; 2],
    kind: u8,
    fwd: bool,
    bwd: bool,
}

struct MbInfo {
    intra: bool,
    m: Motion,
    dct_field: bool,
    cbp: u32,
    last: [usize; 8],
}

pub(crate) struct Core {
    pub(crate) seq: Option<SequenceInfo>,
    qm: QuantMatrices,
    pool: Vec<Pic>,
    ref_old: Option<usize>,
    ref_new: Option<usize>,
    cur: Option<CurFrame>,
    pic: Option<PicState>,
    pending_hdr: Option<PictureHeader>,
    pending_pts: Option<Timestamp>,
    pub(crate) out: VecDeque<(VideoFrame, PictureInfo)>,
    buf: Vec<u8>,
    marks: Vec<(usize, Option<Timestamp>)>,
    closed_gop: bool,
    unsupported: Option<Error>,
    seq_changed: bool,
    /// Pictures dropped because a reference was missing.
    pub(crate) dropped: u64,
}

const MAX_OUT: usize = 64;
const MAX_BUF: usize = 1 << 26;

impl Core {
    pub(crate) fn new() -> Core {
        Core {
            seq: None,
            qm: QuantMatrices::default(),
            pool: Vec::new(),
            ref_old: None,
            ref_new: None,
            cur: None,
            pic: None,
            pending_hdr: None,
            pending_pts: None,
            out: VecDeque::new(),
            buf: Vec::new(),
            marks: Vec::new(),
            closed_gop: false,
            unsupported: None,
            seq_changed: false,
            dropped: 0,
        }
    }

    /// Drop everything but the stream parameters' defaults (seek).
    pub(crate) fn reset(&mut self) {
        self.pool.clear();
        self.ref_old = None;
        self.ref_new = None;
        self.cur = None;
        self.pic = None;
        self.pending_hdr = None;
        self.out.clear();
        self.buf.clear();
        self.marks.clear();
        self.unsupported = None;
    }

    // ------------------------------------------------------------------
    // Byte stream
    // ------------------------------------------------------------------

    /// Append stream bytes (arbitrary chunking) and decode every complete unit.
    pub(crate) fn push(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        if self.buf.len() + data.len() > MAX_BUF {
            // No start code for a very long time: the data is not video.
            self.buf.clear();
            self.marks.clear();
        }
        self.marks.push((self.buf.len(), pts));
        self.buf.extend_from_slice(data);
        self.process(false)
    }

    /// Decode the trailing unit too (the caller knows the data is complete).
    pub(crate) fn push_complete(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<()> {
        self.push(data, pts)?;
        self.process(true)
    }

    fn pts_at(&self, off: usize) -> Option<Timestamp> {
        self.marks.iter().rev().find(|m| m.0 <= off).and_then(|m| m.1)
    }

    fn process(&mut self, all: bool) -> Result<()> {
        let buf = std::mem::take(&mut self.buf);
        let mut starts: Vec<usize> = Vec::new();
        let mut k = 2usize;
        while k < buf.len() {
            if buf[k] == 1 && buf[k - 1] == 0 && buf[k - 2] == 0 {
                if k + 1 < buf.len() {
                    starts.push(k - 2);
                }
                k += 3;
            } else {
                k += 1;
            }
        }
        let units = if all { starts.len() } else { starts.len().saturating_sub(1) };
        for k in 0..units {
            let s = starts[k];
            let e = if k + 1 < starts.len() { starts[k + 1] } else { buf.len() };
            let pts = self.pts_at(s);
            // A start code can begin on the previous code byte (00 00 01 00 00 01): empty payload.
            self.unit(buf[s + 3], &buf[(s + 4).min(e)..e], pts);
        }
        let keep_from = if all {
            buf.len()
        } else if units < starts.len() {
            starts[units]
        } else {
            // No complete start code yet: keep a possible partial start code.
            buf.len().saturating_sub(3)
        };
        self.buf = buf[keep_from..].to_vec();
        let n = keep_from;
        for m in &mut self.marks {
            m.0 = m.0.saturating_sub(n);
        }
        // Keep only the last mark at offset 0.
        while self.marks.len() > 1 && self.marks[1].0 == 0 {
            self.marks.remove(0);
        }
        if all {
            self.marks.clear();
        }
        match self.unsupported.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn fail(&mut self, e: Error) {
        if self.unsupported.is_none() {
            self.unsupported = Some(e);
        }
    }

    fn unit(&mut self, code: u8, payload: &[u8], pts: Option<Timestamp>) {
        match code {
            0x00 => {
                self.end_picture();
                if self.cur.as_ref().is_some_and(|c| !c.waiting_second) {
                    self.finish_frame();
                }
                match parse_picture_header(payload) {
                    Ok(h) => {
                        self.pending_hdr = Some(h);
                        self.pending_pts = pts;
                    }
                    Err(e) => {
                        self.pending_hdr = None;
                        self.fail(e);
                    }
                }
            }
            0xB3 => {
                self.end_picture();
                self.finish_frame();
                let mut qm = QuantMatrices::default();
                match parse_sequence_header(payload, &mut qm) {
                    Ok(mut s) => {
                        let changed = self
                            .seq
                            .as_ref()
                            .is_none_or(|o| o.width != s.width || o.height != s.height);
                        if !changed && let Some(o) = self.seq.as_ref() {
                            // The header carries no chroma format; keep the extension's.
                            s.chroma_format = o.chroma_format;
                        }
                        if changed {
                            self.flush_output();
                            self.ref_old = None;
                            self.ref_new = None;
                            self.pool.clear();
                        }
                        self.seq = Some(s);
                        self.qm = qm;
                        self.seq_changed = true;
                    }
                    Err(e) => self.fail(e),
                }
            }
            0xB5 => self.extension(payload),
            0xB8 => {
                self.end_picture();
                self.finish_frame();
                let mut b = Bits::new(payload);
                b.skip(25);
                self.closed_gop = b.read1();
            }
            0xB7 => {
                self.end_picture();
                self.finish_frame();
            }
            0x01..=0xAF => self.slice(code, payload),
            _ => {}
        }
    }

    fn extension(&mut self, payload: &[u8]) {
        let mut b = Bits::new(payload);
        let id = b.read(4);
        match id {
            1 => {
                if let Some(seq) = self.seq.as_mut() {
                    // Sequence extension right after a sequence header.
                    let mut s2 = seq.clone();
                    match parse_sequence_extension(&mut b, &mut s2) {
                        Ok(()) => {
                            let changed = s2.width != seq.width || s2.height != seq.height || s2.chroma_format != seq.chroma_format;
                            *seq = s2;
                            if changed {
                                self.cur = None;
                                self.pool.clear();
                                self.ref_old = None;
                                self.ref_new = None;
                            }
                        }
                        Err(e) => self.fail(e),
                    }
                }
            }
            3 => {
                let cf = self.seq.as_ref().map_or(1, |s| s.chroma_format);
                let mut qm = self.qm.clone();
                match parse_quant_matrix_extension(&mut b, &mut qm, cf) {
                    Ok(()) => self.qm = qm,
                    Err(e) => self.fail(e),
                }
            }
            5 => self.fail(Error::unsupported(
                "MPEG-2 scalable extension",
                "SNR, spatial and temporal scalability layers are not decoded",
            )),
            8 => {
                if let Some(h) = self.pending_hdr.as_mut()
                    && let Err(e) = parse_picture_coding_extension(&mut b, h) {
                        self.pending_hdr = None;
                        self.fail(e);
                    }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Picture management
    // ------------------------------------------------------------------

    fn alloc_pic(&mut self) -> usize {
        let seq = self.seq.as_ref().expect("sequence");
        let mw = seq.mb_width() * 16;
        let mh = seq.mb_height() * 16;
        let (sx, sy) = chroma_shift(seq.chroma_format);
        let cw = mw >> sx;
        let ch = mh >> sy;
        let busy = |i: usize, c: &Core| {
            Some(i) == c.ref_old
                || Some(i) == c.ref_new
                || c.cur.as_ref().is_some_and(|f| f.idx == i)
        };
        let new_pic = || Pic {
            planes: [vec![128; mw * mh], vec![128; cw * ch], vec![128; cw * ch]],
            stride: [mw, cw, cw],
            height: [mh, ch, ch],
            meta: None,
            emitted: false,
        };
        for i in 0..self.pool.len() {
            if !busy(i, self) {
                let p = &mut self.pool[i];
                // The MB rows can change without a resize: never reuse planes of another geometry.
                if p.stride != [mw, cw, cw] || p.height != [mh, ch, ch] {
                    *p = new_pic();
                }
                p.meta = None;
                p.emitted = false;
                return i;
            }
        }
        self.pool.push(new_pic());
        self.pool.len() - 1
    }

    fn begin_picture(&mut self) -> bool {
        let Some(hdr) = self.pending_hdr.take() else {
            return false;
        };
        let Some(seq) = self.seq.clone() else {
            return false;
        };
        if seq.width > MAX_DIMENSION
            || seq.height > MAX_DIMENSION
            || seq.width * seq.height > MAX_AREA
        {
            self.fail(Error::unsupported(
                format!("{}x{} MPEG video", seq.width, seq.height),
                "picture larger than the decoder's allocation bound",
            ));
            return false;
        }
        if seq.mpeg2 && !hdr.has_extension {
            self.fail(Error::corrupt("MPEG-2 picture without picture_coding_extension"));
            return false;
        }
        if seq.chroma_format != 1 && seq.chroma_format != 2 {
            self.fail(Error::unsupported(
                format!("MPEG-2 chroma_format {}", seq.chroma_format),
                "only 4:2:0 and 4:2:2 are decoded",
            ));
            return false;
        }
        let rows_frame = seq.mb_height();
        // Second field of the frame in progress?
        let mut second = false;
        if let Some(cf) = self.cur.as_ref()
            && cf.waiting_second
                && hdr.structure != Structure::Frame
                && hdr.structure != cf.first_structure
            {
                second = true;
            }
        if !second {
            self.finish_frame();
        }
        let ptype = hdr.picture_type;
        if !second {
            // Reference availability.
            let missing = match ptype {
                PictureType::I => false,
                PictureType::P => self.ref_new.is_none(),
                PictureType::B => self.ref_new.is_none(),
            };
            if ptype == PictureType::B && self.ref_old.is_none() && self.ref_new.is_some() {
                // Leading B pictures of an open GOP: the forward reference is
                // gone. Not decodable correctly.
                self.dropped += 1;
                self.cur = None;
                self.pic = None;
                return false;
            }
            if missing {
                // P (or B) without any reference: predict from mid-grey.
                let g = self.alloc_pic();
                self.ref_new = Some(g);
                self.ref_old = None;
                self.pool[g].emitted = true;
            }
            let idx = self.alloc_pic();
            self.cur = Some(CurFrame {
                idx,
                ptype,
                first_structure: hdr.structure,
                waiting_second: false,
                pts: self.pending_pts.take(),
                tr: hdr.temporal_reference,
                tff: if hdr.structure == Structure::Frame {
                    hdr.top_field_first
                } else {
                    hdr.structure == Structure::Top
                },
                rff: hdr.repeat_first_field,
                progressive_frame: hdr.progressive_frame || !seq.mpeg2,
                corrupt: missing,
                field_pictures: hdr.structure != Structure::Frame,
                closed_gop: self.closed_gop,
            });
        } else if let Some(cf) = self.cur.as_mut() {
            cf.waiting_second = false;
        }
        let parity = match hdr.structure {
            Structure::Bottom => 1,
            _ => 0,
        };
        let rows = if hdr.structure == Structure::Frame {
            rows_frame
        } else {
            rows_frame / 2
        };
        self.pic = Some(PicState {
            hdr,
            mb_done: vec![false; rows * seq.mb_width()],
            rows,
            parity,
            second_field: second,
        });
        true
    }

    /// Finish the picture being decoded: conceal macroblocks that never arrived.
    fn end_picture(&mut self) {
        let Some(st) = self.pic.take() else {
            return;
        };
        let Some(seq) = self.seq.clone() else {
            return;
        };
        let Some(cf) = self.cur.as_mut() else {
            return;
        };
        let mw = seq.mb_width();
        let missing = st.mb_done.iter().filter(|d| !**d).count();
        if missing > 0 {
            cf.corrupt = true;
            let (sx, sy) = chroma_shift(seq.chroma_format);
            let refidx = self.ref_new.filter(|&r| r != cf.idx);
            for (addr, done) in st.mb_done.iter().enumerate() {
                if *done {
                    continue;
                }
                let mb_x = addr % mw;
                let mb_y = addr / mw;
                conceal_mb(&mut self.pool, cf.idx, refidx, st.hdr.structure, st.parity, mb_x, mb_y, sx, sy);
            }
        }
        if st.hdr.structure != Structure::Frame {
            // Wait for the second field.
            if !st.second_field {
                cf.waiting_second = true;
            }
        }
        cf.progressive_frame &= st.hdr.progressive_frame || !seq.mpeg2;
    }

    /// Complete the frame under construction: update references, emit output.
    fn finish_frame(&mut self) {
        let Some(cf) = self.cur.take() else {
            return;
        };
        let Some(seq) = self.seq.as_ref() else {
            return;
        };
        let interlace = if cf.progressive_frame {
            Interlace::Progressive
        } else if cf.tff {
            Interlace::TopFieldFirst
        } else {
            Interlace::BottomFieldFirst
        };
        let info = PictureInfo {
            picture_type: cf.ptype,
            temporal_reference: cf.tr,
            top_field_first: cf.tff,
            repeat_first_field: cf.rff,
            progressive_frame: cf.progressive_frame,
            progressive_sequence: seq.progressive_sequence,
            field_pictures: cf.field_pictures,
            mpeg1: !seq.mpeg2,
            corrupt: cf.corrupt,
            closed_gop: cf.closed_gop,
        };
        self.pool[cf.idx].meta = Some(Meta {
            pts: cf.pts,
            info,
            interlace,
        });
        match cf.ptype {
            PictureType::B => self.emit(cf.idx),
            _ => {
                if let Some(prev) = self.ref_new {
                    self.emit(prev);
                }
                self.ref_old = self.ref_new;
                self.ref_new = Some(cf.idx);
            }
        }
    }

    fn emit(&mut self, idx: usize) {
        let Some(seq) = self.seq.as_ref() else {
            return;
        };
        let p = &mut self.pool[idx];
        if p.emitted {
            return;
        }
        p.emitted = true;
        let Some(meta) = p.meta.clone() else {
            return;
        };
        let (w, h) = (seq.width as usize, seq.height as usize);
        let (sx, sy) = chroma_shift(seq.chroma_format);
        let format = if seq.chroma_format == 1 {
            PixelFormat::I420
        } else {
            PixelFormat::I422
        };
        let dims = [(w, h), (w.div_ceil(1 << sx), h.div_ceil(1 << sy)), (w.div_ceil(1 << sx), h.div_ceil(1 << sy))];
        let mut planes = Vec::with_capacity(3);
        for (c, &(pw, ph)) in dims.iter().enumerate() {
            let stride = p.stride[c];
            // A picture decoded under different sequence parameters is dropped, not read out of range.
            if ph == 0 || p.planes[c].len() < (ph - 1) * stride + pw {
                return;
            }
            let mut data = Vec::with_capacity(pw * ph);
            for y in 0..ph {
                data.extend_from_slice(&p.planes[c][y * stride..y * stride + pw]);
            }
            planes.push(Plane::new(data, pw));
        }
        let mut f = VideoFrame {
            format,
            width: w as u32,
            height: h as u32,
            planes,
            color: ColorInfo::default(),
            pts: meta.pts,
            interlace: meta.interlace,
        };
        f.color = ColorInfo::default();
        if self.out.len() >= MAX_OUT {
            self.out.pop_front();
        }
        self.out.push_back((f, meta.info));
    }

    fn flush_output(&mut self) {
        if let Some(r) = self.ref_new {
            self.emit(r);
        }
    }

    /// End of stream: decode what is buffered and drain the reorder delay.
    pub(crate) fn flush(&mut self) -> Result<()> {
        let r = self.process(true);
        self.end_picture();
        self.finish_frame();
        self.flush_output();
        r
    }

    // ------------------------------------------------------------------
    // Slices
    // ------------------------------------------------------------------

    fn slice(&mut self, code: u8, payload: &[u8]) {
        if self.pic.is_none() {
            if self.pending_hdr.is_none() {
                return;
            }
            if !self.begin_picture() {
                return;
            }
        }
        if self.cur.is_none() {
            return;
        }
        let _ = self.decode_slice(code, payload);
    }

    #[allow(clippy::too_many_lines)]
    fn decode_slice(&mut self, code: u8, data: &[u8]) -> std::result::Result<(), ()> {
        let seq = self.seq.clone().ok_or_else(|| trace(line!()))?;
        let t = tables();
        let mut st = self.pic.take().ok_or_else(|| trace(line!()))?;
        let r = self.slice_inner(&seq, t, &mut st, code, data);
        self.pic = Some(st);
        r
    }

    fn slice_inner(
        &mut self,
        seq: &SequenceInfo,
        t: &Tables,
        st: &mut PicState,
        code: u8,
        data: &[u8],
    ) -> std::result::Result<(), ()> {
        let hdr = st.hdr.clone();
        let mpeg1 = !seq.mpeg2;
        let mut b = Bits::new(data);
        let mut mb_row = code as usize - 1;
        if seq.height > 2800 {
            mb_row += (b.read(3) as usize) << 7;
        }
        let mw = seq.mb_width();
        if mb_row >= st.rows {
            return Err(trace(line!()));
        }
        let qcode = b.read(5) as usize;
        if qcode == 0 {
            return Err(trace(line!()));
        }
        while b.read1() {
            b.skip(8);
        }
        let cur_idx = self.cur.as_ref().ok_or_else(|| trace(line!()))?.idx;
        // The picture must match this slice's geometry, or macroblock rows fall outside its planes.
        if self.pool[cur_idx].height[0] != seq.mb_height() * 16 {
            return Err(trace(line!()));
        }
        let ptype = hdr.picture_type;
        let field_pic = hdr.structure != Structure::Frame;
        let total = st.rows * mw;
        let scan: &[u8; 64] = if hdr.alternate_scan { &ALT_SCAN } else { &ZIGZAG };
        let qscale_of = |code: usize| -> i32 {
            if mpeg1 {
                code as i32
            } else if hdr.q_scale_type {
                NONLINEAR_QSCALE[code & 31] as i32
            } else {
                (code as i32) * 2
            }
        };
        let mut qscale = qscale_of(qcode);
        let dc_reset = if mpeg1 { 128 } else { 1 << (7 + hdr.intra_dc_precision as i32) };
        let dc_mult = if mpeg1 { 8 } else { 8 >> hdr.intra_dc_precision };
        let mut dc_pred = [dc_reset; 3];
        let mut pmv = [[[0i32; 2]; 2]; 2]; // [r][s][t]
        let mut motion = Motion::default();
        motion.fwd = true;
        let mut addr: isize = (mb_row * mw) as isize - 1;
        let mut first = true;
        let nblocks = if seq.chroma_format == 1 { 6 } else { 8 };
        let mut blocks = [[0i16; 64]; 8];

        loop {
            // macroblock_address_increment
            let mut inc = 0usize;
            loop {
                let v = b.vlc(&t.mba).ok_or_else(|| trace(line!()))?;
                if v == 34 {
                    inc += 33;
                } else if v == 35 {
                    // MPEG-1 macroblock stuffing
                } else {
                    inc += v as usize;
                    break;
                }
                if b.overrun() {
                    return Err(trace(line!()));
                }
            }
            if first {
                addr += inc as isize;
                first = false;
            } else {
                // skipped macroblocks
                for _ in 1..inc {
                    addr += 1;
                    if addr as usize >= total {
                        return Err(trace(line!()));
                    }
                    let a = addr as usize;
                    // Skipped macroblock.
                    dc_pred = [dc_reset; 3];
                    let mut m = motion;
                    if ptype == PictureType::B {
                        // Same directions and vectors as the previous macroblock,
                        // predicted frame-wise (field-wise from the same-parity
                        // field in a field picture).
                        m.kind = if field_pic { MV_FIELD16 } else { MV_FRAME };
                        for s2 in 0..2 {
                            m.mv[s2][0] = pmv[0][s2];
                            m.mv[s2][1] = pmv[0][s2];
                            m.sel[s2] = [st.parity as u8; 2];
                        }
                    }
                    if ptype == PictureType::P {
                        pmv = [[[0; 2]; 2]; 2];
                        m = Motion::default();
                        m.fwd = true;
                        m.kind = if field_pic { MV_FIELD16 } else { MV_FRAME };
                        let sel = st.parity as u8;
                        m.sel = [[sel; 2]; 2];
                        motion = m;
                    }
                    if ptype == PictureType::I {
                        return Err(trace(line!()));
                    }
                    let info = MbInfo { intra: false, m, dct_field: false, cbp: 0, last: [0; 8] };
                    self.recon_mb(seq, &hdr, st, cur_idx, a % mw, a / mw, &info, &mut blocks);
                    st.mb_done[a] = true;
                }
                addr += 1;
            }
            if addr < 0 || addr as usize >= total {
                return Err(trace(line!()));
            }
            let a = addr as usize;
            let mb_x = a % mw;
            let mb_y = a / mw;

            // macroblock_type
            let ti = match ptype {
                PictureType::I => 0,
                PictureType::P => 1,
                PictureType::B => 2,
            };
            let flags = b.vlc(&t.mbtype[ti]).ok_or_else(|| trace(line!()))? as u16;
            let intra = flags & MB_INTRA != 0;
            let mut m = Motion { fwd: flags & MB_FWD != 0, bwd: flags & MB_BWD != 0, ..Motion::default() };
            let mut dct_field = false;
            let mut kind_code = 0u32;
            if !mpeg1 && (m.fwd || m.bwd) {
                if hdr.structure == Structure::Frame && hdr.frame_pred_frame_dct {
                    kind_code = 2; // frame-based
                } else {
                    kind_code = b.read(2);
                    if kind_code == 0 {
                        return Err(trace(line!()));
                    }
                }
            } else if mpeg1 {
                kind_code = 2;
            }
            if !mpeg1 && hdr.structure == Structure::Frame && !hdr.frame_pred_frame_dct && (intra || flags & MB_PAT != 0) {
                dct_field = b.read1();
            }
            if flags & MB_QUANT != 0 {
                let qc = b.read(5) as usize;
                if qc == 0 {
                    return Err(trace(line!()));
                }
                qscale = qscale_of(qc);
            }
            m.kind = if field_pic {
                match kind_code {
                    1 => MV_FIELD16,
                    2 => MV_16X8,
                    _ => MV_DUAL_FIELD,
                }
            } else {
                match kind_code {
                    1 => MV_FIELD_IN_FRAME,
                    2 => MV_FRAME,
                    _ => MV_DUAL_FRAME,
                }
            };
            if intra && hdr.concealment_motion_vectors {
                m.kind = if field_pic { MV_FIELD16 } else { MV_FRAME };
            }
            // Motion vectors.
            if m.fwd || (intra && hdr.concealment_motion_vectors) {
                read_motion(&mut b, t, &hdr, mpeg1, &mut m, &mut pmv, 0)?;
            }
            if m.bwd {
                read_motion(&mut b, t, &hdr, mpeg1, &mut m, &mut pmv, 1)?;
            }
            if intra && hdr.concealment_motion_vectors {
                b.skip(1);
            }
            if intra {
                if !hdr.concealment_motion_vectors {
                    pmv = [[[0; 2]; 2]; 2];
                }
                m.fwd = false;
                m.bwd = false;
            } else {
                if ptype == PictureType::P && !m.fwd {
                    // No motion compensation: zero vector.
                    pmv = [[[0; 2]; 2]; 2];
                    m.fwd = true;
                    m.kind = if field_pic { MV_FIELD16 } else { MV_FRAME };
                    m.sel = [[st.parity as u8; 2]; 2];
                    m.mv = [[[0; 2]; 2]; 2];
                }
                dc_pred = [dc_reset; 3];
            }
            // coded_block_pattern
            let mut cbp = 0u32;
            if intra {
                cbp = (1 << nblocks) - 1;
            } else if flags & MB_PAT != 0 {
                cbp = b.vlc(&t.cbp).ok_or_else(|| trace(line!()))?;
                if seq.chroma_format == 2 {
                    cbp = (cbp << 2) | b.read(2);
                }
            }
            // Blocks.
            let mut last = [0usize; 8];
            let mut ok = true;
            for i in 0..nblocks {
                if cbp & (1 << (nblocks - 1 - i)) == 0 {
                    continue;
                }
                let comp = if i < 4 { 0 } else { 1 + (i & 1) };
                let chroma = i >= 4;
                let (matrix, _) = if intra {
                    (if chroma { &self.qm.chroma_intra } else { &self.qm.intra }, 0)
                } else {
                    (if chroma { &self.qm.chroma_non_intra } else { &self.qm.non_intra }, 0)
                };
                let p = BlockParams { mpeg1, qscale, matrix, scan, alt_vlc: hdr.intra_vlc_format };
                let r = if intra {
                    let tbl = if comp == 0 { &t.dc_luma } else { &t.dc_chroma };
                    let size = match b.vlc(tbl) {
                        Some(s) => s,
                        None => {
                            ok = false;
                            break;
                        }
                    };
                    let mut diff = 0i32;
                    if size > 0 {
                        let v = b.read(size) as i32;
                        diff = if v < (1 << (size - 1)) { v - (1 << size) + 1 } else { v };
                    }
                    dc_pred[comp] += diff;
                    decode_intra(&mut b, t, &p, &mut blocks[i], dc_pred[comp] * dc_mult)
                } else {
                    decode_inter(&mut b, t, &p, &mut blocks[i])
                };
                match r {
                    Some(l) => last[i] = l,
                    None => {
                        ok = false;
                        break;
                    }
                }
                if b.overrun() {
                    ok = false;
                    break;
                }
            }
            if !ok {
                for blk in blocks.iter_mut() {
                    *blk = [0; 64];
                }
                return Err(trace(line!()));
            }
            let info = MbInfo { intra, m, dct_field, cbp, last };
            self.recon_mb(seq, &hdr, st, cur_idx, mb_x, mb_y, &info, &mut blocks);
            st.mb_done[a] = true;
            if !intra {
                motion = m;
            } else if ptype == PictureType::B {
                // A skipped macroblock after an intra one repeats nothing useful.
                motion = Motion { fwd: true, ..Motion::default() };
            }
            if b.overrun() {
                return Err(trace(line!()));
            }
            if b.peek(23) == 0 {
                break;
            }
        }
        Ok(())
    }

    fn ref_for(&self, ptype: PictureType, s: usize, field: Option<u8>, st: &PicState, cur: usize) -> usize {
        match ptype {
            PictureType::I => cur,
            PictureType::P => {
                if st.hdr.structure != Structure::Frame
                    && st.second_field
                    && field.is_some_and(|f| f as usize != st.parity)
                {
                    cur
                } else {
                    self.ref_new.unwrap_or(cur)
                }
            }
            PictureType::B => {
                if s == 0 {
                    self.ref_old.or(self.ref_new).unwrap_or(cur)
                } else {
                    self.ref_new.unwrap_or(cur)
                }
            }
        }
    }

    /// Build the inter prediction of a macroblock into `pred` (pitch 16).
    #[allow(clippy::too_many_arguments)]
    fn build_pred(
        &self,
        pred: &mut [[u8; 256]; 3],
        seq: &SequenceInfo,
        hdr: &PictureHeader,
        st: &PicState,
        cur: usize,
        mb_x: usize,
        mb_y: usize,
        m: &Motion,
    ) {
        let (sx, sy) = chroma_shift(seq.chroma_format);
        let mut dirs_done = 0;
        for s in 0..2 {
            if !(if s == 0 { m.fwd } else { m.bwd }) {
                continue;
            }
            let mut parts: [Option<Part>; 4] = [None; 4];
            let mut np = 0;
            let mut push = |p: Part| {
                parts[np] = Some(p);
                np += 1;
            };
            let mvv = |r: usize| (m.mv[s][r][0], m.mv[s][r][1]);
            let ptype = hdr.picture_type;
            match m.kind {
                MV_FRAME => {
                    let (x, y) = mvv(0);
                    push(Part { dst0: 0, step: 1, rows: 16, src: self.ref_for(ptype, s, None, st, cur), field: None, src_row: (mb_y * 16) as i32, mvx: x, mvy: y, avg: false });
                }
                MV_FIELD_IN_FRAME => {
                    for r in 0..2 {
                        let (x, y) = mvv(r);
                        let f = m.sel[s][r];
                        push(Part { dst0: r, step: 2, rows: 8, src: self.ref_for(ptype, s, Some(f), st, cur), field: Some(f), src_row: (mb_y * 8) as i32, mvx: x, mvy: y, avg: false });
                    }
                }
                MV_DUAL_FRAME => {
                    let (x, y) = mvv(0);
                    for p in 0..2usize {
                        let mm = if (p == 0) == hdr.top_field_first { 1 } else { 3 };
                        let e = if p == 0 { -1 } else { 1 };
                        let ox = ((x * mm + (x > 0) as i32) >> 1) + m.dmv[0];
                        let oy = ((y * mm + (y > 0) as i32) >> 1) + m.dmv[1] + e;
                        let src = self.ref_for(ptype, s, Some(p as u8), st, cur);
                        push(Part { dst0: p, step: 2, rows: 8, src, field: Some(p as u8), src_row: (mb_y * 8) as i32, mvx: x, mvy: y, avg: false });
                        push(Part { dst0: p, step: 2, rows: 8, src, field: Some(1 - p as u8), src_row: (mb_y * 8) as i32, mvx: ox, mvy: oy, avg: true });
                    }
                }
                MV_FIELD16 => {
                    let (x, y) = mvv(0);
                    let f = m.sel[s][0];
                    push(Part { dst0: 0, step: 1, rows: 16, src: self.ref_for(ptype, s, Some(f), st, cur), field: Some(f), src_row: (mb_y * 16) as i32, mvx: x, mvy: y, avg: false });
                }
                MV_16X8 => {
                    for r in 0..2 {
                        let (x, y) = mvv(r);
                        let f = m.sel[s][r];
                        push(Part { dst0: 8 * r, step: 1, rows: 8, src: self.ref_for(ptype, s, Some(f), st, cur), field: Some(f), src_row: (mb_y * 16 + 8 * r) as i32, mvx: x, mvy: y, avg: false });
                    }
                }
                _ => {
                    // MV_DUAL_FIELD
                    let (x, y) = mvv(0);
                    let p = st.parity as u8;
                    let e = if p == 0 { -1 } else { 1 };
                    let ox = ((x + (x > 0) as i32) >> 1) + m.dmv[0];
                    let oy = ((y + (y > 0) as i32) >> 1) + m.dmv[1] + e;
                    let src_same = self.ref_for(ptype, s, Some(p), st, cur);
                    let src_opp = self.ref_for(ptype, s, Some(1 - p), st, cur);
                    push(Part { dst0: 0, step: 1, rows: 16, src: src_same, field: Some(p), src_row: (mb_y * 16) as i32, mvx: x, mvy: y, avg: false });
                    push(Part { dst0: 0, step: 1, rows: 16, src: src_opp, field: Some(1 - p), src_row: (mb_y * 16) as i32, mvx: ox, mvy: oy, avg: true });
                }
            }
            for part in parts.iter().take(np).flatten() {
                let pic = &self.pool[part.src];
                for c in 0..3 {
                    let (csx, csy) = if c == 0 { (0, 0) } else { (sx, sy) };
                    let w = 16 >> csx;
                    let rows = part.rows >> csy;
                    let (cmx, cmy) = (
                        if csx == 1 { part.mvx / 2 } else { part.mvx },
                        if csy == 1 { part.mvy / 2 } else { part.mvy },
                    );
                    let x = (mb_x * w) as i32 + (cmx >> 1);
                    let y = (part.src_row >> csy) + (cmy >> 1);
                    let height = pic.height[c];
                    let img = Image {
                        data: &pic.planes[c],
                        stride: pic.stride[c],
                        w: pic.stride[c],
                        h: if part.field.is_some() { height / 2 } else { height },
                        field: part.field,
                    };
                    let dst0 = if part.step == 2 { part.dst0 } else { part.dst0 >> csy };
                    predict(&img, x, y, cmx & 1 != 0, cmy & 1 != 0, w, rows, &mut pred[c], dst0, part.step, part.avg || dirs_done > 0);
                }
            }
            dirs_done += 1;
        }
    }

    /// Reconstruct one macroblock into the current picture.
    #[allow(clippy::too_many_arguments)]
    fn recon_mb(
        &mut self,
        seq: &SequenceInfo,
        hdr: &PictureHeader,
        st: &PicState,
        cur: usize,
        mb_x: usize,
        mb_y: usize,
        info: &MbInfo,
        blocks: &mut [[i16; 64]; 8],
    ) {
        let (sx, sy) = chroma_shift(seq.chroma_format);
        let field_pic = hdr.structure != Structure::Frame;
        let mut pred = [[0u8; 256]; 3];
        if !info.intra {
            self.build_pred(&mut pred, seq, hdr, st, cur, mb_x, mb_y, &info.m);
        }
        let nblocks = if seq.chroma_format == 1 { 6 } else { 8 };
        let pic = &mut self.pool[cur];
        let mut origin = [0usize; 3];
        let mut ls = [0usize; 3];
        for c in 0..3 {
            let (w, h) = if c == 0 { (16, 16) } else { (16 >> sx, 16 >> sy) };
            let stride = pic.stride[c];
            if field_pic {
                ls[c] = 2 * stride;
                origin[c] = (mb_y * h * 2 + st.parity) * stride + mb_x * w;
            } else {
                ls[c] = stride;
                origin[c] = mb_y * h * stride + mb_x * w;
            }
            if !info.intra {
                let plane = &mut pic.planes[c];
                for r in 0..h {
                    let o = origin[c] + r * ls[c];
                    plane[o..o + w].copy_from_slice(&pred[c][r * 16..r * 16 + w]);
                }
            }
        }
        let dct_field = info.dct_field;
        for i in 0..nblocks {
            if info.cbp & (1 << (nblocks - 1 - i)) == 0 {
                continue;
            }
            let (c, off, bs) = if i < 4 {
                if dct_field {
                    (0, origin[0] + (i >> 1) * ls[0] + (i & 1) * 8, 2 * ls[0])
                } else {
                    (0, origin[0] + (i >> 1) * 8 * ls[0] + (i & 1) * 8, ls[0])
                }
            } else {
                let c = 1 + (i & 1);
                if seq.chroma_format == 1 {
                    (c, origin[c], ls[c])
                } else {
                    let lower = (i >= 6) as usize;
                    if dct_field {
                        (c, origin[c] + lower * ls[c], 2 * ls[c])
                    } else {
                        (c, origin[c] + lower * 8 * ls[c], ls[c])
                    }
                }
            };
            let dst = &mut pic.planes[c][off..];
            let blk = &mut blocks[i];
            if info.last[i] == 0 {
                let dc = blk[0];
                if info.intra {
                    idct_put_dc(dc, dst, bs);
                } else {
                    idct_add_dc(dc, dst, bs);
                }
            } else if info.intra {
                idct_put(blk, dst, bs);
            } else {
                idct_add(blk, dst, bs);
            }
            *blk = [0; 64];
        }
    }
}

#[derive(Clone, Copy)]
struct Part {
    dst0: usize,
    step: usize,
    rows: usize,
    src: usize,
    field: Option<u8>,
    src_row: i32,
    mvx: i32,
    mvy: i32,
    avg: bool,
}

fn decode_component(b: &mut Bits, t: &Tables, f_code: u8, pred: i32) -> Option<i32> {
    if f_code == 0 || f_code > 9 {
        return None;
    }
    let code = b.vlc(&t.motion)? as i32;
    if code == 0 {
        // ffmpeg returns the prediction unchanged (no range wrap) on a zero delta
        return Some(pred);
    }
    let r_size = (f_code - 1) as u32;
    let mut delta = 0;
    if code != 0 {
        let neg = b.read1();
        let res = if r_size > 0 { b.read(r_size) as i32 } else { 0 };
        delta = ((code - 1) << r_size) + res + 1;
        if neg {
            delta = -delta;
        }
    }
    let f = 1i32 << r_size;
    let (lo, range) = (-16 * f, 32 * f);
    // Full modulo wrap (as ffmpeg's sign_extend): the prediction can lie
    // outside the range after a field vector was stored doubled.
    let v = (pred + delta - lo).rem_euclid(range) + lo;
    Some(v)
}

fn read_motion(
    b: &mut Bits,
    t: &Tables,
    hdr: &PictureHeader,
    mpeg1: bool,
    m: &mut Motion,
    pmv: &mut [[[i32; 2]; 2]; 2],
    s: usize,
) -> std::result::Result<(), ()> {
    let field_pic = hdr.structure != Structure::Frame;
    let dual = matches!(m.kind, MV_DUAL_FRAME | MV_DUAL_FIELD);
    let field_fmt = field_pic || m.kind != MV_FRAME;
    let frame_field = !field_pic && field_fmt;
    let n = if matches!(m.kind, MV_FIELD_IN_FRAME | MV_16X8) { 2 } else { 1 };
    let shift = if mpeg1 && hdr.full_pel[s] { 1 } else { 0 };
    for r in 0..n {
        if field_fmt && !dual {
            m.sel[s][r] = b.read1() as u8;
        }
        for tc in 0..2 {
            let mut pred = pmv[r][s][tc];
            if tc == 1 && frame_field {
                pred >>= 1;
            }
            let v = decode_component(b, t, hdr.f_code[s][tc], pred).ok_or_else(|| trace(line!()))?;
            pmv[r][s][tc] = if tc == 1 && frame_field { v << 1 } else { v };
            m.mv[s][r][tc] = v << shift;
            if dual {
                m.dmv[tc] = if b.read1() { 1 - 2 * (b.read1() as i32) } else { 0 };
            }
        }
    }
    if n == 1 {
        pmv[1][s] = pmv[0][s];
        m.mv[s][1] = m.mv[s][0];
        m.sel[s][1] = m.sel[s][0];
    }
    if b.overrun() { Err(()) } else { Ok(()) }
}

#[allow(clippy::too_many_arguments)]
fn conceal_mb(
    pool: &mut [Pic],
    cur: usize,
    refidx: Option<usize>,
    structure: Structure,
    parity: usize,
    mb_x: usize,
    mb_y: usize,
    sx: usize,
    sy: usize,
) {
    let field_pic = structure != Structure::Frame;
    for c in 0..3 {
        let (w, h) = if c == 0 { (16, 16) } else { (16 >> sx, 16 >> sy) };
        let stride = pool[cur].stride[c];
        for r in 0..h {
            let line = if field_pic { (mb_y * h + r) * 2 + parity } else { mb_y * h + r };
            let o = line * stride + mb_x * w;
            // Corrupt streams can address rows past the picture (field/odd-height cases).
            if pool[cur].planes[c].len() < o + w {
                continue;
            }
            match refidx {
                Some(ri) if ri != cur => {
                    let (a, bsl) = if ri < cur {
                        let (l, rr) = pool.split_at_mut(cur);
                        (&l[ri].planes[c], &mut rr[0].planes[c])
                    } else {
                        let (l, rr) = pool.split_at_mut(ri);
                        (&rr[0].planes[c], &mut l[cur].planes[c])
                    };
                    if a.len() >= o + w {
                        bsl[o..o + w].copy_from_slice(&a[o..o + w]);
                    }
                }
                _ => pool[cur].planes[c][o..o + w].fill(128),
            }
        }
    }
}

/// Debug aid: with `EC_MPEG2_TRACE` set, report where a slice gave up.
fn trace(line: u32) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("EC_MPEG2_TRACE").is_some()) {
        eprintln!("ec-mpeg2: slice error at decoder.rs:{line}");
    }
}

fn chroma_shift(cf: u8) -> (usize, usize) {
    match cf {
        1 => (1, 1),
        2 => (1, 0),
        _ => (0, 0),
    }
}