//! Sequence, GOP and picture headers and their extensions (ISO/IEC 13818-2 6.2).

use crate::bits::Bits;
use crate::tables::{DEFAULT_INTRA, ZIGZAG};
use ec_core::error::{Error, Result};

/// Largest picture the decoder accepts, per side and in samples.
pub const MAX_DIMENSION: u32 = 8192;
/// Largest luma area accepted.
pub const MAX_AREA: u32 = 8192 * 4352;

fn corrupt(msg: &str) -> Error {
    Error::corrupt(format!("MPEG video: {msg}"))
}

/// Sequence-level parameters (sequence header plus sequence extension).
#[derive(Debug, Clone)]
pub struct SequenceInfo {
    /// Coded luma width (horizontal_size_value plus extension).
    pub width: u32,
    /// Coded luma height.
    pub height: u32,
    /// aspect_ratio_information (4 bits).
    pub aspect_ratio_code: u8,
    /// frame_rate_code (4 bits).
    pub frame_rate_code: u8,
    /// Frame rate extension numerator (MPEG-2 only, 1 when absent).
    pub frame_rate_ext_n: u32,
    /// Frame rate extension denominator plus one (MPEG-2 only, 1 when absent).
    pub frame_rate_ext_d: u32,
    /// bit_rate (+ extension) in units of 400 bit/s.
    pub bit_rate: u32,
    /// MPEG-1 constrained_parameters_flag.
    pub constrained_parameters: bool,
    /// True when a sequence_extension followed (MPEG-2 syntax).
    pub mpeg2: bool,
    /// profile_and_level_indication (8 bits; 0 for MPEG-1).
    pub profile_level: u8,
    /// progressive_sequence.
    pub progressive_sequence: bool,
    /// chroma_format: 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    pub chroma_format: u8,
    /// low_delay.
    pub low_delay: bool,
}

impl SequenceInfo {
    /// Frame rate as numerator and denominator, when the code is defined.
    pub fn frame_rate(&self) -> Option<(u32, u32)> {
        let (n, d) = match self.frame_rate_code {
            1 => (24000, 1001),
            2 => (24, 1),
            3 => (25, 1),
            4 => (30000, 1001),
            5 => (30, 1),
            6 => (50, 1),
            7 => (60000, 1001),
            8 => (60, 1),
            _ => return None,
        };
        Some((n * self.frame_rate_ext_n, d * self.frame_rate_ext_d))
    }

    /// Macroblock columns.
    pub fn mb_width(&self) -> usize {
        self.width.div_ceil(16) as usize
    }

    /// Macroblock rows of a frame.
    pub fn mb_height(&self) -> usize {
        if self.progressive_sequence || !self.mpeg2 {
            self.height.div_ceil(16) as usize
        } else {
            (self.height.div_ceil(32) * 2) as usize
        }
    }
}

/// The four quantiser matrices, raster order.
#[derive(Debug, Clone)]
pub struct QuantMatrices {
    pub intra: [u8; 64],
    pub non_intra: [u8; 64],
    pub chroma_intra: [u8; 64],
    pub chroma_non_intra: [u8; 64],
}

impl Default for QuantMatrices {
    fn default() -> Self {
        let mut intra = [0u8; 64];
        for i in 0..64 {
            intra[ZIGZAG[i] as usize] = DEFAULT_INTRA[i];
        }
        QuantMatrices {
            intra,
            non_intra: [16; 64],
            chroma_intra: intra,
            chroma_non_intra: [16; 64],
        }
    }
}

fn read_matrix(b: &mut Bits) -> [u8; 64] {
    let mut m = [0u8; 64];
    for &z in ZIGZAG.iter() {
        m[z as usize] = b.read(8) as u8;
    }
    m
}

/// Parse a sequence header (the bytes after the start code).
pub(crate) fn parse_sequence_header(data: &[u8], q: &mut QuantMatrices) -> Result<SequenceInfo> {
    let mut b = Bits::new(data);
    let width = b.read(12);
    let height = b.read(12);
    let aspect = b.read(4) as u8;
    let frc = b.read(4) as u8;
    let bit_rate = b.read(18);
    b.skip(1); // marker
    b.skip(10); // vbv_buffer_size_value
    let constrained = b.read1();
    let mut m = QuantMatrices::default();
    if b.read1() {
        m.intra = read_matrix(&mut b);
        m.chroma_intra = m.intra;
    }
    if b.read1() {
        m.non_intra = read_matrix(&mut b);
        m.chroma_non_intra = m.non_intra;
    }
    if b.overrun() {
        return Err(corrupt("truncated sequence header"));
    }
    if width == 0 || height == 0 {
        return Err(corrupt("zero picture size"));
    }
    *q = m;
    Ok(SequenceInfo {
        width,
        height,
        aspect_ratio_code: aspect,
        frame_rate_code: frc,
        frame_rate_ext_n: 1,
        frame_rate_ext_d: 1,
        bit_rate,
        constrained_parameters: constrained,
        mpeg2: false,
        profile_level: 0,
        progressive_sequence: true,
        chroma_format: 1,
        low_delay: false,
    })
}

/// Parse a sequence_extension (after the 4-bit id) into `seq`.
pub(crate) fn parse_sequence_extension(b: &mut Bits, seq: &mut SequenceInfo) -> Result<()> {
    let pl = b.read(8) as u8;
    let progressive = b.read1();
    let chroma = b.read(2) as u8;
    let hext = b.read(2);
    let vext = b.read(2);
    let br_ext = b.read(12);
    b.skip(1);
    b.skip(8); // vbv_buffer_size_extension
    let low_delay = b.read1();
    let n = b.read(2);
    let d = b.read(5);
    if b.overrun() {
        return Err(corrupt("truncated sequence extension"));
    }
    seq.width |= hext << 12;
    seq.height |= vext << 12;
    seq.bit_rate |= br_ext << 18;
    seq.mpeg2 = true;
    seq.profile_level = pl;
    seq.progressive_sequence = progressive;
    seq.chroma_format = chroma;
    seq.low_delay = low_delay;
    seq.frame_rate_ext_n = n + 1;
    seq.frame_rate_ext_d = d + 1;
    Ok(())
}

pub(crate) fn parse_quant_matrix_extension(
    b: &mut Bits,
    q: &mut QuantMatrices,
    chroma_format: u8,
) -> Result<()> {
    if b.read1() {
        q.intra = read_matrix(b);
        if chroma_format == 1 {
            q.chroma_intra = q.intra;
        }
    }
    if b.read1() {
        q.non_intra = read_matrix(b);
        if chroma_format == 1 {
            q.chroma_non_intra = q.non_intra;
        }
    }
    if b.read1() {
        q.chroma_intra = read_matrix(b);
    }
    if b.read1() {
        q.chroma_non_intra = read_matrix(b);
    }
    if b.overrun() {
        return Err(corrupt("truncated quant matrix extension"));
    }
    Ok(())
}

/// Picture coding type (3 bits in the picture header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureType {
    /// Intra.
    I,
    /// Predictive.
    P,
    /// Bidirectional.
    B,
}

/// How a coded picture covers the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Structure {
    /// A whole frame.
    Frame,
    /// The top field.
    Top,
    /// The bottom field.
    Bottom,
}

/// Picture header plus picture_coding_extension.
#[derive(Debug, Clone)]
pub struct PictureHeader {
    pub temporal_reference: u16,
    pub picture_type: PictureType,
    pub full_pel: [bool; 2],
    /// f_code[s][t]: s = 0 forward / 1 backward, t = 0 horizontal / 1 vertical.
    pub f_code: [[u8; 2]; 2],
    pub intra_dc_precision: u8,
    pub structure: Structure,
    pub top_field_first: bool,
    pub frame_pred_frame_dct: bool,
    pub concealment_motion_vectors: bool,
    pub q_scale_type: bool,
    pub intra_vlc_format: bool,
    pub alternate_scan: bool,
    pub repeat_first_field: bool,
    pub chroma_420_type: bool,
    pub progressive_frame: bool,
    /// True once a picture_coding_extension was parsed.
    pub has_extension: bool,
}

pub(crate) fn parse_picture_header(data: &[u8]) -> Result<PictureHeader> {
    let mut b = Bits::new(data);
    let tr = b.read(10) as u16;
    let t = b.read(3);
    b.skip(16); // vbv_delay
    let picture_type = match t {
        1 => PictureType::I,
        2 => PictureType::P,
        3 => PictureType::B,
        4 => {
            return Err(Error::unsupported(
                "MPEG-1 D-picture",
                "DC-only pictures (picture_coding_type 4) are not decoded",
            ));
        }
        _ => return Err(corrupt("reserved picture_coding_type")),
    };
    let mut full_pel = [false; 2];
    let mut f_code = [[15u8; 2]; 2];
    if picture_type != PictureType::I {
        full_pel[0] = b.read1();
        let f = b.read(3) as u8;
        f_code[0] = [f, f];
        if picture_type == PictureType::B {
            full_pel[1] = b.read1();
            let f = b.read(3) as u8;
            f_code[1] = [f, f];
        }
    }
    if b.overrun() {
        return Err(corrupt("truncated picture header"));
    }
    Ok(PictureHeader {
        temporal_reference: tr,
        picture_type,
        full_pel,
        f_code,
        intra_dc_precision: 0,
        structure: Structure::Frame,
        top_field_first: false,
        frame_pred_frame_dct: true,
        concealment_motion_vectors: false,
        q_scale_type: false,
        intra_vlc_format: false,
        alternate_scan: false,
        repeat_first_field: false,
        chroma_420_type: true,
        progressive_frame: true,
        has_extension: false,
    })
}

pub(crate) fn parse_picture_coding_extension(b: &mut Bits, h: &mut PictureHeader) -> Result<()> {
    let mut f_code = [[0u8; 2]; 2];
    for s in 0..2 {
        for t in 0..2 {
            f_code[s][t] = b.read(4) as u8;
        }
    }
    let dc = b.read(2) as u8;
    let structure = match b.read(2) {
        1 => Structure::Top,
        2 => Structure::Bottom,
        3 => Structure::Frame,
        _ => return Err(corrupt("reserved picture_structure")),
    };
    h.top_field_first = b.read1();
    h.frame_pred_frame_dct = b.read1();
    h.concealment_motion_vectors = b.read1();
    h.q_scale_type = b.read1();
    h.intra_vlc_format = b.read1();
    h.alternate_scan = b.read1();
    h.repeat_first_field = b.read1();
    h.chroma_420_type = b.read1();
    h.progressive_frame = b.read1();
    if b.overrun() {
        return Err(corrupt("truncated picture coding extension"));
    }
    h.f_code = f_code;
    h.intra_dc_precision = dc;
    h.structure = structure;
    h.has_extension = true;
    Ok(())
}
