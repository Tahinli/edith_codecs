//! DV system profiles (`dv_profile.c`) and the macroblock placement tables (`dv.c`).

use crate::tables::{AUDIO_SHUFFLE525, AUDIO_SHUFFLE625};

/// Native chroma layout of a DV system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chroma {
    /// 4:1:1 (NTSC DV25 / DVCPRO25, and the PAL DVCPRO25 variant).
    Yuv411,
    /// 4:2:0 (PAL DV25).
    Yuv420,
    /// 4:2:2 (DVCPRO50 and DVCPRO HD).
    Yuv422,
}

/// One DV flavour: geometry, frame size and the tables its audio uses.
#[derive(Debug)]
pub struct Profile {
    /// `dsf` bit of the DIF header: 0 = 525/60, 1 = 625/50.
    pub dsf: u8,
    /// Video source `stype` (0 = 25 Mbps, 4 = 50 Mbps, 0x14 = 1080i HD, 0x18 = 720p HD, 1 = IEC 61883-5 PAL).
    pub video_stype: u8,
    /// Bytes per frame.
    pub frame_size: usize,
    /// DIF sequences per DIF channel (10 for 525, 12 for 625).
    pub difseg_size: usize,
    /// DIF channels per frame.
    pub n_difchan: usize,
    /// Frame duration as `(num, den)` seconds.
    pub time_base: (u32, u32),
    /// Picture width.
    pub width: u32,
    /// Picture height.
    pub height: u32,
    /// Sample aspect ratio for 4:3 and 16:9 pictures.
    pub sar: [(u32, u32); 2],
    /// Native chroma layout.
    pub chroma: Chroma,
    /// Blocks per macroblock.
    pub bpm: usize,
    /// AC block sizes in bits.
    pub block_sizes: &'static [u8; 8],
    /// Stride of the audio shuffle table.
    pub audio_stride: usize,
    /// Minimum audio samples per frame at 48, 44.1 and 32 kHz.
    pub audio_min_samples: [usize; 3],
    /// The audio shuffle table.
    pub audio_shuffle: &'static [[u8; 9]],
}

impl Profile {
    /// DVCPRO HD (SMPTE 370M).
    pub fn is_hd(&self) -> bool {
        self.video_stype & 0x10 != 0
    }
    pub(crate) fn is_1080i50(&self) -> bool {
        self.video_stype == 0x14 && self.dsf == 1
    }
    pub(crate) fn is_720p50(&self) -> bool {
        self.video_stype == 0x18 && self.dsf == 1
    }
    /// Frames per second as `(num, den)`.
    pub fn frame_rate(&self) -> (u32, u32) {
        (self.time_base.1, self.time_base.0)
    }
    /// Number of 5-macroblock video segments in a frame.
    pub fn work_size(&self) -> usize {
        let mut size = self.n_difchan * self.difseg_size * 27;
        if self.is_1080i50() {
            size -= 3 * 27;
        }
        if self.is_720p50() {
            size -= 4 * 27;
        }
        size
    }
}

static BLOCK_SIZES_DV2550: [u8; 8] = [112, 112, 112, 112, 80, 80, 0, 0];
static BLOCK_SIZES_DV100: [u8; 8] = [80, 80, 80, 80, 80, 80, 64, 64];

#[allow(clippy::too_many_arguments)]
const fn p(
    dsf: u8,
    video_stype: u8,
    frame_size: usize,
    difseg_size: usize,
    n_difchan: usize,
    time_base: (u32, u32),
    height: u32,
    width: u32,
    sar: [(u32, u32); 2],
    chroma: Chroma,
    bpm: usize,
    block_sizes: &'static [u8; 8],
    audio_stride: usize,
    audio_min_samples: [usize; 3],
    audio_shuffle: &'static [[u8; 9]],
) -> Profile {
    Profile {
        dsf,
        video_stype,
        frame_size,
        difseg_size,
        n_difchan,
        time_base,
        width,
        height,
        sar,
        chroma,
        bpm,
        block_sizes,
        audio_stride,
        audio_min_samples,
        audio_shuffle,
    }
}

const NTSC_MIN: [usize; 3] = [1580, 1452, 1053];
const PAL_MIN: [usize; 3] = [1896, 1742, 1264];

/// The profile list, in FFmpeg's order (the index matters for [`frame_profile`]).
pub static PROFILES: [Profile; 10] = [
    p(0, 0x0, 120000, 10, 1, (1001, 30000), 480, 720, [(8, 9), (32, 27)], Chroma::Yuv411, 6, &BLOCK_SIZES_DV2550, 90, NTSC_MIN, &AUDIO_SHUFFLE525),
    p(1, 0x0, 144000, 12, 1, (1, 25), 576, 720, [(16, 15), (64, 45)], Chroma::Yuv420, 6, &BLOCK_SIZES_DV2550, 108, PAL_MIN, &AUDIO_SHUFFLE625),
    p(1, 0x0, 144000, 12, 1, (1, 25), 576, 720, [(16, 15), (64, 45)], Chroma::Yuv411, 6, &BLOCK_SIZES_DV2550, 108, PAL_MIN, &AUDIO_SHUFFLE625),
    p(0, 0x4, 240000, 10, 2, (1001, 30000), 480, 720, [(8, 9), (32, 27)], Chroma::Yuv422, 6, &BLOCK_SIZES_DV2550, 90, NTSC_MIN, &AUDIO_SHUFFLE525),
    p(1, 0x4, 288000, 12, 2, (1, 25), 576, 720, [(16, 15), (64, 45)], Chroma::Yuv422, 6, &BLOCK_SIZES_DV2550, 108, PAL_MIN, &AUDIO_SHUFFLE625),
    p(0, 0x14, 480000, 10, 4, (1001, 30000), 1080, 1280, [(1, 1), (3, 2)], Chroma::Yuv422, 8, &BLOCK_SIZES_DV100, 90, NTSC_MIN, &AUDIO_SHUFFLE525),
    p(1, 0x14, 576000, 12, 4, (1, 25), 1080, 1440, [(1, 1), (4, 3)], Chroma::Yuv422, 8, &BLOCK_SIZES_DV100, 108, PAL_MIN, &AUDIO_SHUFFLE625),
    p(0, 0x18, 240000, 10, 2, (1001, 60000), 720, 960, [(1, 1), (4, 3)], Chroma::Yuv422, 8, &BLOCK_SIZES_DV100, 90, NTSC_MIN, &AUDIO_SHUFFLE525),
    p(1, 0x18, 288000, 12, 2, (1, 50), 720, 960, [(1, 1), (4, 3)], Chroma::Yuv422, 8, &BLOCK_SIZES_DV100, 90, PAL_MIN, &AUDIO_SHUFFLE625),
    p(1, 0x1, 144000, 12, 1, (1, 25), 576, 720, [(16, 15), (64, 45)], Chroma::Yuv420, 6, &BLOCK_SIZES_DV2550, 108, PAL_MIN, &AUDIO_SHUFFLE625),
];

/// Bytes needed to identify a profile (six DIF blocks).
pub const PROFILE_BYTES: usize = 6 * 80;

/// Pick the profile of a DV frame (`ff_dv_frame_profile`, minus the QuickTime `SL25` codec-tag
/// hack). `prev` is the profile of the previous frame, used for damaged headers.
pub fn frame_profile(prev: Option<&'static Profile>, frame: &[u8]) -> Option<&'static Profile> {
    if frame.len() < PROFILE_BYTES {
        return None;
    }
    let dsf = (frame[3] & 0x80) >> 7;
    let stype = frame[80 * 5 + 48 + 3] & 0x1f;
    let pal = frame[80 * 5 + 48 + 3] & 0x20 != 0;
    if dsf == 1 && stype == 0 && frame[4] & 0x07 != 0 {
        return Some(&PROFILES[2]);
    }
    if dsf == 0 && pal && stype == PROFILES[1].video_stype && frame.len() == PROFILES[1].frame_size {
        return Some(&PROFILES[1]);
    }
    if let Some(p) = PROFILES.iter().find(|p| p.dsf == dsf && p.video_stype == stype) {
        return Some(p);
    }
    if let Some(s) = prev
        && frame.len() == s.frame_size
    {
        return Some(s);
    }
    if frame[3] & 0x7f == 0x3f && frame[80 * 5 + 48 + 3] == 0xff {
        return Some(&PROFILES[usize::from(dsf)]);
    }
    None
}

/// One 5-macroblock video segment: where it starts (in 80-byte DIF blocks) and where its
/// macroblocks go (`x | y << 8`, units of 8 pixels).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WorkChunk {
    pub buf_offset: u16,
    pub mb: [u16; 5],
}

fn mb_coordinates(d: &Profile, chan: usize, seq: usize, slot: usize, tbl: &mut [u16; 5]) {
    const OFF: [usize; 5] = [2, 6, 8, 0, 4];
    const SHUF1: [usize; 5] = [36, 18, 54, 0, 72];
    const SHUF2: [usize; 5] = [24, 12, 36, 0, 48];
    const SHUF3: [usize; 5] = [18, 9, 27, 0, 36];
    const L_START: [usize; 10] = [0, 4, 9, 13, 18, 22, 27, 31, 36, 40];
    const L_START_SHUFFLED: [usize; 5] = [9, 4, 13, 0, 18];
    const SERPENT1: [usize; 27] = [
        0, 1, 2, 2, 1, 0, 0, 1, 2, 2, 1, 0, 0, 1, 2, 2, 1, 0, 0, 1, 2, 2, 1, 0, 0, 1, 2,
    ];
    const SERPENT2: [usize; 30] = [
        0, 1, 2, 3, 4, 5, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5,
    ];
    const REMAP: [[usize; 2]; 64] = [
        [0, 0], [0, 0], [0, 0], [0, 0], [0, 0], [0, 1], [0, 2], [0, 3], [10, 0],
        [10, 1], [10, 2], [10, 3], [20, 0], [20, 1], [20, 2], [20, 3], [30, 0], [30, 1], [30, 2],
        [30, 3], [40, 0], [40, 1], [40, 2], [40, 3], [50, 0], [50, 1], [50, 2], [50, 3], [60, 0],
        [60, 1], [60, 2], [60, 3], [70, 0], [70, 1], [70, 2], [70, 3], [0, 64], [0, 65], [0, 66],
        [10, 64], [10, 65], [10, 66], [20, 64], [20, 65], [20, 66], [30, 64], [30, 65], [30, 66],
        [40, 64], [40, 65], [40, 66], [50, 64], [50, 65], [50, 66], [60, 64], [60, 65], [60, 66],
        [70, 64], [70, 65], [70, 66], [0, 67], [20, 67], [40, 67], [60, 67],
    ];
    for m in 0..5 {
        match d.width {
            1440 => {
                let blk = (chan * 11 + seq) * 27 + slot;
                let (x, y);
                if chan == 0 && seq == 11 {
                    let xx = m * 27 + slot;
                    if xx < 90 {
                        x = xx;
                        y = 0;
                    } else {
                        x = (xx - 90) * 2;
                        y = 67;
                    }
                } else {
                    let i = (4 * chan + blk + OFF[m]) % 11;
                    let k = (blk / 11) % 27;
                    x = SHUF1[m] + (chan & 1) * 9 + k % 9;
                    y = (i * 3 + k / 9) * 2 + (chan >> 1) + 1;
                }
                tbl[m] = ((x << 1) | (y << 9)) as u16;
            }
            1280 => {
                let blk = (chan * 10 + seq) * 27 + slot;
                let i = (4 * chan + (seq / 5) + 2 * blk + OFF[m]) % 10;
                let k = (blk / 5) % 27;
                let mut x = SHUF1[m] + (chan & 1) * 9 + k % 9;
                let mut y = (i * 3 + k / 9) * 2 + (chan >> 1) + 4;
                if x >= 80 {
                    let r = REMAP[y.min(63)];
                    x = r[0] + ((x - 80) << usize::from(y > 59));
                    y = r[1];
                }
                tbl[m] = ((x << 1) | (y << 9)) as u16;
            }
            960 => {
                let blk = (chan * 10 + seq) * 27 + slot;
                let i = (4 * chan + (seq / 5) + 2 * blk + OFF[m]) % 10;
                let k = (blk / 5) % 27 + (i & 1) * 3;
                let x = SHUF2[m] + k % 6 + 6 * (chan & 1);
                let y = L_START[i] + k / 6 + 45 * (chan >> 1);
                tbl[m] = ((x << 1) | (y << 9)) as u16;
            }
            _ => match d.chroma {
                Chroma::Yuv422 => {
                    let x = SHUF3[m] + slot / 3;
                    let y = SERPENT1[slot] + ((((seq + OFF[m]) % d.difseg_size) << 1) + chan) * 3;
                    tbl[m] = ((x << 1) | (y << 8)) as u16;
                }
                Chroma::Yuv420 => {
                    let x = SHUF3[m] + slot / 3;
                    let y = SERPENT1[slot] + ((seq + OFF[m]) % d.difseg_size) * 3;
                    tbl[m] = ((x << 1) | (y << 9)) as u16;
                }
                Chroma::Yuv411 => {
                    let i = (seq + OFF[m]) % d.difseg_size;
                    let k = slot + if m == 1 || m == 2 { 3 } else { 0 };
                    let x = L_START_SHUFFLED[m] + k / 6;
                    let mut y = SERPENT2[k] + i * 6;
                    if x > 21 {
                        y = y * 2 - i * 6;
                    }
                    tbl[m] = ((x << 2) | (y << 8)) as u16;
                }
            },
        }
    }
}

pub(crate) fn work_chunks(d: &Profile) -> Vec<WorkChunk> {
    let mut v = Vec::with_capacity(d.work_size());
    let mut p = 0usize;
    for c in 0..d.n_difchan {
        for s in 0..d.difseg_size {
            p += 6;
            for j in 0..27 {
                if j % 3 == 0 {
                    p += 1;
                }
                if !(d.is_1080i50() && c != 0 && s == 11) && !(d.is_720p50() && s > 9) {
                    let mut w = WorkChunk { buf_offset: p as u16, mb: [0; 5] };
                    mb_coordinates(d, c, s, j, &mut w.mb);
                    v.push(w);
                }
                p += 5;
            }
        }
    }
    v
}
